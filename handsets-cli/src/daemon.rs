// connect / disconnect / devices — manages the on-device handsets daemon
// lifecycle from the host (push jar, start app_process, adb-forward TCP,
// kill on disconnect).

use std::io;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::state_cache;

const DEVICE_JAR: &str = "/data/local/tmp/hs.jar";
const DEVICE_LOG: &str = "/data/local/tmp/hs.log";
const DEFAULT_PORT: u16 = 9008;

/// Locate the on-device jar on the host. Probes (in order):
///   1. $HANDSETS_JAR  (or legacy $A11YDUMP_JAR)
///   2. <exe_dir>/hs.jar          — release tarball layout
///   3. <exe_dir>/../build/hs.jar (and ../../, ../../../)
///   4. ~/.handsets/hs.jar        — curl-installer layout (resolved even when
///                                  <exe_dir> points at a /usr/local/bin symlink)
///   5. ./build/hs.jar            — dev checkout
pub(crate) fn locate_jar() -> io::Result<PathBuf> {
    for var in ["HANDSETS_JAR", "A11YDUMP_JAR"] {
        if let Ok(env) = std::env::var(var) {
            let p = PathBuf::from(env);
            if p.is_file() { return Ok(p); }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in [
                "hs.jar",
                "../build/hs.jar",
                "../../build/hs.jar",
                // handsets-cli/target/release/hs → ../../../build/hs.jar
                "../../../build/hs.jar",
            ] {
                let p = dir.join(rel);
                if p.is_file() { return Ok(p); }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".handsets/hs.jar");
        if p.is_file() { return Ok(p); }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let p = cwd.join("build/hs.jar");
    if p.is_file() { return Ok(p); }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "hs.jar not found. Set HANDSETS_JAR, install via the official installer, \
         or run from a checkout with build/hs.jar",
    ))
}

/// Try to find the handsets-viewer binary. PATH first, then alongside our
/// binary, then the curl-installer layout under ~/.handsets.
pub(crate) fn locate_viewer() -> Option<PathBuf> {
    if let Ok(p) = which("handsets-viewer") { return Some(p); }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [
                dir.join("handsets-viewer"),
                dir.join("../../handsets-viewer/target/release/handsets-viewer"),
                dir.join("../../../handsets-viewer/target/release/handsets-viewer"),
            ] {
                if cand.is_file() { return Some(cand); }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".handsets/handsets-viewer");
        if p.is_file() { return Some(p); }
    }
    None
}

fn which(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("'{name}' not on PATH")))
}

/// Result of running an adb subcommand. We capture stdout/stderr so we can
/// surface the actual adb error on failure rather than just exit code.
struct AdbOut {
    code: i32,
    stdout: String,
    stderr: String,
}

fn adb(serial: Option<&str>, args: &[&str]) -> io::Result<AdbOut> {
    let mut cmd = Command::new("adb");
    if let Some(s) = serial { cmd.args(["-s", s]); }
    cmd.args(args).stdin(Stdio::null());
    let out = cmd.output()?;
    Ok(AdbOut {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn adb_ok(serial: Option<&str>, args: &[&str], what: &str) -> io::Result<String> {
    let r = adb(serial, args)?;
    if r.code != 0 {
        return Err(io::Error::other(format!(
            "{what}: adb exit {} — {}",
            r.code,
            if !r.stderr.trim().is_empty() { r.stderr.trim() } else { r.stdout.trim() }
        )));
    }
    Ok(r.stdout)
}

// ---------- public command entrypoints ----------

#[derive(Debug, Clone)]
pub(crate) struct ConnectOpts {
    pub serial: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone)]
pub(crate) struct DisconnectOpts {
    pub serial: Option<String>,
    pub keep_jar: bool,
}

pub(crate) fn connect(opts: &ConnectOpts) -> io::Result<u16> {
    // Talk to the adb-server directly over its host protocol on tcp:5037.
    // Each query is a sub-ms round-trip vs ~40ms for spawning the adb
    // subprocess. Falls back to subprocess if adb-server isn't reachable
    // (e.g. first `hs use` since boot), which is the only path that still
    // needs to start the server anyway.
    let serial = match adb_server_resolve_serial(opts.serial.as_deref()) {
        Ok(s) => s,
        Err(_) => resolve_serial(opts.serial.as_deref())?,
    };
    let forwards = adb_server_list_forwards().unwrap_or_else(|_|
        list_forwards().unwrap_or_default());

    let port = pick_port_with(opts.port, &serial, &forwards)?;

    // Fast path: if the chosen port is already forwarded to this serial AND
    // the daemon answers `ping`, we're done. This is the common case when
    // RPA scripts re-invoke `hs use` defensively before each step.
    let already_forwarded = forwards.iter().any(|f|
        f.serial == serial && f.host_spec == format!("tcp:{port}"));
    if already_forwarded && ping_daemon(port).is_ok() {
        let _ = state_cache::spawn_detached("127.0.0.1", port);
        return Ok(port);
    }

    // --- slow path: (re)start the daemon ---

    let jar = locate_jar()?;

    // 1. Hard-kill any prior daemon. linkToDeath inside system_server takes a
    //    moment to clear, so if we actually killed something we briefly wait
    //    until pgrep stops finding it. Skip the wait entirely when nothing
    //    matched (pkill exits non-zero) — saves ~500ms on a clean boot.
    let killed = adb(Some(&serial), &["shell", "pkill", "-9", "-f", "hsd"])
        .map(|r| r.code == 0).unwrap_or(false);
    if killed {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let r = adb(Some(&serial), &["shell", "pgrep", "-f", "hsd"])?;
            if r.stdout.trim().is_empty() { break; }
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(200));
    }

    // 2. Push jar.
    let jar_str = jar.to_string_lossy();
    adb_ok(Some(&serial), &["push", &jar_str, DEVICE_JAR], "push jar")?;

    // 3. (Re)set forward.
    let _ = adb(Some(&serial), &["forward", "--remove", &format!("tcp:{port}")]);
    adb_ok(
        Some(&serial),
        &["forward", &format!("tcp:{port}"), &format!("tcp:{port}")],
        "forward",
    )?;

    // 4. Start the daemon detached.
    let start = format!(
        "CLASSPATH={DEVICE_JAR} nohup app_process /system/bin \
         --nice-name=hsd dev.handsets.daemon.Main --port={port} > {DEVICE_LOG} 2>&1 &"
    );
    adb_ok(Some(&serial), &["shell", &start], "start daemon")?;

    // 5. Wait until the daemon answers `ping`. TCP probes against the local
    //    forward are ~5ms each, so we can poll much tighter than the old
    //    200ms `adb shell cat $LOG` loop.
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if ping_daemon(port).is_ok() {
            let _ = state_cache::spawn_detached("127.0.0.1", port);
            return Ok(port);
        }
        thread::sleep(Duration::from_millis(40));
    }
    let r = adb(Some(&serial), &["shell", "cat", DEVICE_LOG])?;
    Err(io::Error::other(format!(
        "daemon did not come up within 6s\n--- device log ---\n{}",
        r.stdout
    )))
}

/// Send a length-prefixed `ping` and expect `pong`. Tight timeouts so a
/// dead-but-still-listening forward fails quickly into the slow path.
fn ping_daemon(port: u16) -> io::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_millis(250))?;
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(Duration::from_millis(250)))?;
    sock.set_write_timeout(Some(Duration::from_millis(250)))?;
    let payload = b"ping";
    sock.write_all(&(payload.len() as u32).to_be_bytes())?;
    sock.write_all(payload)?;
    let mut hdr = [0u8; 4];
    sock.read_exact(&mut hdr)?;
    let n = u32::from_be_bytes(hdr) as usize;
    if n > 64 {
        return Err(io::Error::other("ping reply too large"));
    }
    let mut buf = vec![0u8; n];
    sock.read_exact(&mut buf)?;
    if buf == b"pong" { Ok(()) } else { Err(io::Error::other("ping: unexpected reply")) }
}

pub(crate) fn disconnect(opts: &DisconnectOpts) -> io::Result<()> {
    let serial = resolve_serial(opts.serial.as_deref())?;
    // 1. Kill the host-side state watcher(s) for any forward this device owns.
    if let Ok(forwards) = list_forwards() {
        for f in forwards.iter().filter(|f| f.serial == serial) {
            if let Some(p) = f.host_spec.strip_prefix("tcp:") {
                if let Ok(port) = p.parse::<u16>() {
                    let _ = state_cache::stop_watcher(port);
                }
            }
        }
    }
    // 2. Kill the device-side daemon.
    let _ = adb(Some(&serial), &["shell", "pkill", "-9", "-f", "hsd"]);
    // 3. Remove any matching adb forwards for this device.
    if let Ok(forwards) = list_forwards() {
        for f in forwards.iter().filter(|f| f.serial == serial) {
            let _ = adb(Some(&serial), &["forward", "--remove", &f.host_spec]);
        }
    }
    // 4. Optionally delete the on-device jar.
    if !opts.keep_jar {
        let _ = adb(Some(&serial), &["shell", "rm", "-f", DEVICE_JAR, DEVICE_LOG]);
    }
    Ok(())
}

pub(crate) struct DeviceRow {
    pub serial: String,
    pub state: String,
    pub jar_present: bool,
    pub running: bool,
    pub host_port: Option<u16>,
    pub sdk: Option<String>,
    pub model: Option<String>,
}

pub(crate) fn devices() -> io::Result<Vec<DeviceRow>> {
    let listing = adb_ok(None, &["devices"], "adb devices")?;
    let forwards = list_forwards().unwrap_or_default();
    let mut rows = Vec::new();
    for line in listing.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut it = line.split_whitespace();
        let serial = match it.next() { Some(s) => s.to_string(), None => continue };
        let state = it.next().unwrap_or("?").to_string();
        let mut row = DeviceRow {
            serial: serial.clone(),
            state: state.clone(),
            jar_present: false,
            running: false,
            host_port: None,
            sdk: None,
            model: None,
        };
        if state == "device" {
            row.jar_present = adb(Some(&serial),
                &["shell", "test", "-f", DEVICE_JAR, "&&", "echo", "y"])
                .map(|r| r.stdout.contains('y')).unwrap_or(false);
            row.running = adb(Some(&serial),
                &["shell", "pgrep", "-f", "hsd"])
                .map(|r| !r.stdout.trim().is_empty()).unwrap_or(false);
            row.host_port = forwards.iter()
                .find(|f| f.serial == serial && f.host_spec.starts_with("tcp:"))
                .and_then(|f| f.host_spec.strip_prefix("tcp:")?.parse().ok());
            if row.running {
                row.sdk = adb(Some(&serial),
                    &["shell", "getprop", "ro.build.version.sdk"])
                    .map(|r| r.stdout.trim().to_string()).ok();
                row.model = adb(Some(&serial),
                    &["shell", "getprop", "ro.product.model"])
                    .map(|r| r.stdout.trim().to_string()).ok();
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

// ---------- helpers ----------

struct ForwardEntry {
    serial: String,
    host_spec: String,
}

fn list_forwards() -> io::Result<Vec<ForwardEntry>> {
    let r = adb(None, &["forward", "--list"])?;
    if r.code != 0 { return Ok(Vec::new()); }
    let mut out = Vec::new();
    for line in r.stdout.lines() {
        let mut it = line.split_whitespace();
        let serial = match it.next() { Some(s) => s.to_string(), None => continue };
        let host = match it.next() { Some(s) => s.to_string(), None => continue };
        out.push(ForwardEntry { serial, host_spec: host });
    }
    Ok(out)
}

/// If no serial is specified and exactly one device is attached, use it.
/// Otherwise error out so the user picks.
fn resolve_serial(explicit: Option<&str>) -> io::Result<String> {
    if let Some(s) = explicit { return Ok(s.to_string()); }
    let listing = adb_ok(None, &["devices"], "adb devices")?;
    let mut found: Vec<String> = listing
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let s = it.next()?;
            let st = it.next().unwrap_or("");
            if st == "device" { Some(s.to_string()) } else { None }
        })
        .collect();
    match found.len() {
        0 => Err(io::Error::other("no devices in 'device' state — check `adb devices`")),
        1 => Ok(found.remove(0)),
        _ => Err(io::Error::other(format!(
            "multiple devices attached ({}); pass --device SERIAL",
            found.join(", ")
        ))),
    }
}

/// Pick a host port for this device. Default 9008. If already forwarded to a
/// different device, walk forward until we find one nobody uses.
fn pick_port_with(
    requested: Option<u16>,
    our_serial: &str,
    forwards: &[ForwardEntry],
) -> io::Result<u16> {
    if let Some(p) = requested {
        // Honour the explicit request; if a different device claims it, fail.
        for f in forwards {
            if f.host_spec == format!("tcp:{p}") && f.serial != our_serial {
                return Err(io::Error::other(format!(
                    "host port {p} is already forwarded to device {}", f.serial
                )));
            }
        }
        return Ok(p);
    }
    let mut p = DEFAULT_PORT;
    loop {
        let busy_elsewhere = forwards.iter().any(|f|
            f.host_spec == format!("tcp:{p}") && f.serial != our_serial);
        if !busy_elsewhere { return Ok(p); }
        p = p.checked_add(1).ok_or_else(||
            io::Error::other("ran out of TCP ports above 9008"))?;
        if p > 9100 {
            return Err(io::Error::other(
                "couldn't find a free local port in 9008..9100"));
        }
    }
}

// ---------- direct adb-server protocol (tcp:5037) ----------
//
// Talking to adb-server directly skips the ~40ms `adb` subprocess fork on
// every query. The protocol is just a 4-char hex length prefix followed by
// an ASCII payload; the server replies "OKAY" + (hex-len + body) on success
// or "FAIL" + (hex-len + reason) on failure. We use it for two read-only
// host:* queries; the writeable / device-targeted commands still go through
// the subprocess `adb` since they're invoked at most once per connect.

const ADB_SERVER_PORT: u16 = 5037;

fn adb_server_query(payload: &str) -> io::Result<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], ADB_SERVER_PORT));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(200))?;
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    s.set_write_timeout(Some(Duration::from_millis(500)))?;
    let hdr = format!("{:04x}", payload.len());
    s.write_all(hdr.as_bytes())?;
    s.write_all(payload.as_bytes())?;
    let mut status = [0u8; 4];
    s.read_exact(&mut status)?;
    let mut ln_buf = [0u8; 4];
    let ln = if s.read_exact(&mut ln_buf).is_ok() {
        usize::from_str_radix(std::str::from_utf8(&ln_buf)
            .map_err(|_| io::Error::other("adb-server: bad length"))?, 16)
            .map_err(|_| io::Error::other("adb-server: bad length"))?
    } else { 0 };
    let mut body = vec![0u8; ln];
    if ln > 0 { s.read_exact(&mut body)?; }
    let body = String::from_utf8(body)
        .map_err(|_| io::Error::other("adb-server: non-utf8 body"))?;
    if &status != b"OKAY" {
        return Err(io::Error::other(format!("adb-server FAIL: {body}")));
    }
    Ok(body)
}

fn adb_server_resolve_serial(explicit: Option<&str>) -> io::Result<String> {
    if let Some(s) = explicit { return Ok(s.to_string()); }
    let body = adb_server_query("host:devices")?;
    let mut found: Vec<String> = body.lines().filter_map(|l| {
        let mut it = l.split('\t');
        let s = it.next()?.trim();
        let st = it.next().unwrap_or("").trim();
        if st == "device" && !s.is_empty() { Some(s.to_string()) } else { None }
    }).collect();
    match found.len() {
        0 => Err(io::Error::other("no devices in 'device' state")),
        1 => Ok(found.remove(0)),
        _ => Err(io::Error::other(format!(
            "multiple devices attached ({}); pass --device SERIAL",
            found.join(", ")
        ))),
    }
}

fn adb_server_list_forwards() -> io::Result<Vec<ForwardEntry>> {
    let body = adb_server_query("host:list-forward")?;
    let mut out = Vec::new();
    for line in body.lines() {
        let mut it = line.split_whitespace();
        let serial = match it.next() { Some(s) => s.to_string(), None => continue };
        let host = match it.next() { Some(s) => s.to_string(), None => continue };
        out.push(ForwardEntry { serial, host_spec: host });
    }
    Ok(out)
}

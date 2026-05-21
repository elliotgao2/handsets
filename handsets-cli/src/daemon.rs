// connect / disconnect / devices — manages the on-device handsets daemon
// lifecycle from the host (push jar, start app_process, adb-forward TCP,
// kill on disconnect).

use std::io;
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
    let serial = resolve_serial(opts.serial.as_deref())?;
    let jar = locate_jar()?;
    let port = pick_port(opts.port, &serial)?;

    // 1. Hard-kill any prior daemon. linkToDeath inside system_server takes a
    //    moment to clear, so we wait until pgrep stops finding it.
    let _ = adb(Some(&serial), &["shell", "pkill", "-9", "-f", "hsd"]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let r = adb(Some(&serial), &["shell", "pgrep", "-f", "hsd"])?;
        if r.stdout.trim().is_empty() { break; }
        thread::sleep(Duration::from_millis(200));
    }
    thread::sleep(Duration::from_millis(500));

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

    // 5. Wait for "listening" line in the device-side log.
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        let r = adb(Some(&serial), &["shell", "cat", DEVICE_LOG])?;
        if r.stdout.contains("hsd listening") {
            // 6. Spawn the host-side state watcher so `hs state <field>`
            //    can serve out of the local cache. Best-effort: failure here
            //    doesn't block the connect.
            let _ = state_cache::spawn_detached("127.0.0.1", port);
            return Ok(port);
        }
        thread::sleep(Duration::from_millis(200));
    }
    let r = adb(Some(&serial), &["shell", "cat", DEVICE_LOG])?;
    Err(io::Error::other(format!(
        "daemon did not come up within 6s\n--- device log ---\n{}",
        r.stdout
    )))
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
fn pick_port(requested: Option<u16>, our_serial: &str) -> io::Result<u16> {
    let forwards = list_forwards().unwrap_or_default();
    if let Some(p) = requested {
        // Honour the explicit request; if a different device claims it, fail.
        for f in &forwards {
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

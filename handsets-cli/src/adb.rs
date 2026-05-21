// ADB-CLI-equivalent commands routed over the existing handsets socket.
//
// Wire shape matches what `Server.java` expects:
//   pull   →  "pull path=…"       ; server streams [len][chunk]* [len=0]
//   push   →  "push path=… mode=… size=N"  ; client streams chunks then reads
//                                            one final [ok|ERR:…] frame
//   install→  "install size=N reinstall=… grant=…"   ; same as push
//   pm/am  →  single text request → single text response

use std::fs::File;
use std::io::{self, IsTerminal, Write};

use crate::{state_cache, Conn};

#[derive(Debug)]
pub(crate) enum Cmd {
    Pull { device: String, host: Option<String> },
    Push { host: String, device: String, mode: Option<u32> },
    Install { apk: String, reinstall: bool, grant: bool },
    InstallMulti { apks: Vec<String>, reinstall: bool, grant: bool },
    PmList { third: bool, system: bool },
    PmPath(String),
    PmUninstall(String),
    PmGrant(String, String),
    PmRevoke(String, String),
    AmStart { component: String, action: Option<String>, data: Option<String>, flag: Option<i32> },
    AmForceStop(String),
    AmKill(String),
    AmBroadcast { action: Option<String>, component: Option<String>, data: Option<String> },
    GetProp(String),
    SetProp(String, String),
    Dumpsys { service: String, args: Vec<String> },
    Logcat(Vec<String>),
    SettingsGet { namespace: String, key: String },
    SettingsPut { namespace: String, key: String, value: String },
    Shell(Vec<String>),
    WmInfo,
    WmRotation(i32),
    Monitor,
    State(String),
    StateWatch,
}

// ---------- parsers ----------

pub(crate) fn parse_pull(rest: &[&str]) -> Result<Cmd, String> {
    if rest.is_empty() {
        return Err("pull needs DEVICE_PATH [HOST_PATH]".into());
    }
    let device = rest[0].to_string();
    let host = rest.get(1).map(|s| s.to_string());
    Ok(Cmd::Pull { device, host })
}

pub(crate) fn parse_push(rest: &[&str]) -> Result<Cmd, String> {
    let mut positional: Vec<&str> = Vec::new();
    let mut mode: Option<u32> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--mode" => {
                i += 1;
                let v = rest.get(i).ok_or("--mode needs a value")?;
                mode = Some(u32::from_str_radix(v.trim_start_matches('0'), 8)
                    .map_err(|_| format!("invalid --mode {v}"))?);
            }
            other => positional.push(other),
        }
        i += 1;
    }
    if positional.len() != 2 {
        return Err("push needs HOST_PATH DEVICE_PATH".into());
    }
    Ok(Cmd::Push {
        host: positional[0].to_string(),
        device: positional[1].to_string(),
        mode,
    })
}

pub(crate) fn parse_install(rest: &[&str]) -> Result<Cmd, String> {
    let mut positional: Vec<&str> = Vec::new();
    let mut reinstall = false;
    let mut grant = false;
    for tok in rest {
        match *tok {
            "--reinstall" | "-r" => reinstall = true,
            "--grant" | "-g" => grant = true,
            other => positional.push(other),
        }
    }
    if positional.is_empty() {
        return Err("install needs at least one APK_PATH".into());
    }
    if positional.len() == 1 {
        Ok(Cmd::Install { apk: positional[0].to_string(), reinstall, grant })
    } else {
        Ok(Cmd::InstallMulti {
            apks: positional.iter().map(|s| s.to_string()).collect(),
            reinstall, grant,
        })
    }
}

pub(crate) fn parse_getprop(rest: &[&str]) -> Result<Cmd, String> {
    let k = rest.first().ok_or("getprop needs KEY")?.to_string();
    Ok(Cmd::GetProp(k))
}

pub(crate) fn parse_setprop(rest: &[&str]) -> Result<Cmd, String> {
    if rest.len() < 2 { return Err("setprop needs KEY VALUE".into()); }
    Ok(Cmd::SetProp(rest[0].to_string(), rest[1..].join(" ")))
}

pub(crate) fn parse_dumpsys(rest: &[&str]) -> Result<Cmd, String> {
    let (svc, args) = rest.split_first().ok_or("dumpsys needs SERVICE")?;
    Ok(Cmd::Dumpsys {
        service: svc.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    })
}

pub(crate) fn parse_logcat(rest: &[&str]) -> Result<Cmd, String> {
    Ok(Cmd::Logcat(rest.iter().map(|s| s.to_string()).collect()))
}

pub(crate) fn parse_settings(rest: &[&str]) -> Result<Cmd, String> {
    let (sub, rest) = rest.split_first().ok_or("settings needs get|put NS KEY [VALUE]")?;
    match *sub {
        "get" => {
            if rest.len() < 2 { return Err("settings get needs NS KEY".into()); }
            Ok(Cmd::SettingsGet { namespace: rest[0].into(), key: rest[1].into() })
        }
        "put" => {
            if rest.len() < 3 { return Err("settings put needs NS KEY VALUE".into()); }
            Ok(Cmd::SettingsPut {
                namespace: rest[0].into(),
                key: rest[1].into(),
                value: rest[2..].join(" "),
            })
        }
        other => Err(format!("unknown settings subcommand: {other}")),
    }
}

pub(crate) fn parse_shell(rest: &[&str]) -> Result<Cmd, String> {
    if rest.is_empty() { return Err("shell needs ARGV".into()); }
    Ok(Cmd::Shell(rest.iter().map(|s| s.to_string()).collect()))
}

pub(crate) fn parse_wm(rest: &[&str]) -> Result<Cmd, String> {
    let (sub, rest) = rest.split_first().ok_or("wm needs info|rotation N")?;
    match *sub {
        "info" => Ok(Cmd::WmInfo),
        "rotation" => {
            let v = rest.first().ok_or("wm rotation needs N")?;
            let r: i32 = v.parse().map_err(|_| format!("bad rotation: {v}"))?;
            Ok(Cmd::WmRotation(r))
        }
        other => Err(format!("unknown wm subcommand: {other}")),
    }
}

pub(crate) fn parse_monitor(_rest: &[&str]) -> Result<Cmd, String> {
    Ok(Cmd::Monitor)
}

pub(crate) fn parse_state(rest: &[&str]) -> Result<Cmd, String> {
    let field = rest.first().ok_or(
        "state needs FIELD (interactive|battery_level|battery_charging|top|procs|device|watch)",
    )?;
    if *field == "watch" {
        return Ok(Cmd::StateWatch);
    }
    Ok(Cmd::State(field.to_string()))
}

pub(crate) fn parse_pm(rest: &[&str]) -> Result<Cmd, String> {
    let (sub, rest) = rest.split_first().ok_or("pm needs a subcommand")?;
    match *sub {
        "list" => {
            let mut third = false;
            let mut system = false;
            for tok in rest {
                match *tok {
                    "--3" | "-3" => third = true,
                    "--s" | "-s" => system = true,
                    other => return Err(format!("unknown pm list arg: {other}")),
                }
            }
            Ok(Cmd::PmList { third, system })
        }
        "path" => {
            let pkg = rest.first().ok_or("pm path needs PKG")?.to_string();
            Ok(Cmd::PmPath(pkg))
        }
        "uninstall" => {
            let pkg = rest.first().ok_or("pm uninstall needs PKG")?.to_string();
            Ok(Cmd::PmUninstall(pkg))
        }
        "grant" => {
            if rest.len() < 2 { return Err("pm grant needs PKG PERM".into()); }
            Ok(Cmd::PmGrant(rest[0].to_string(), rest[1].to_string()))
        }
        "revoke" => {
            if rest.len() < 2 { return Err("pm revoke needs PKG PERM".into()); }
            Ok(Cmd::PmRevoke(rest[0].to_string(), rest[1].to_string()))
        }
        other => Err(format!("unknown pm subcommand: {other}")),
    }
}

pub(crate) fn parse_am(rest: &[&str]) -> Result<Cmd, String> {
    let (sub, rest) = rest.split_first().ok_or("am needs a subcommand")?;
    match *sub {
        "start" => {
            if rest.is_empty() { return Err("am start needs COMPONENT".into()); }
            let mut component = String::new();
            let mut action: Option<String> = None;
            let mut data: Option<String> = None;
            let mut flag: Option<i32> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "-n" | "--component" => {
                        i += 1;
                        component = rest.get(i).ok_or("-n needs a value")?.to_string();
                    }
                    "-a" | "--action" => {
                        i += 1;
                        action = Some(rest.get(i).ok_or("-a needs a value")?.to_string());
                    }
                    "-d" | "--data" => {
                        i += 1;
                        data = Some(rest.get(i).ok_or("-d needs a value")?.to_string());
                    }
                    "-f" | "--flag" => {
                        i += 1;
                        let v = rest.get(i).ok_or("-f needs a value")?;
                        flag = Some(parse_int(v).map_err(|_| format!("invalid -f {v}"))?);
                    }
                    other if component.is_empty() && other.contains('/') => {
                        component = other.to_string();
                    }
                    other => return Err(format!("unknown am start arg: {other}")),
                }
                i += 1;
            }
            if component.is_empty() {
                return Err("am start needs -n COMPONENT or a positional pkg/.Class".into());
            }
            Ok(Cmd::AmStart { component, action, data, flag })
        }
        "force-stop" => {
            let pkg = rest.first().ok_or("am force-stop needs PKG")?.to_string();
            Ok(Cmd::AmForceStop(pkg))
        }
        "kill" => {
            let pkg = rest.first().ok_or("am kill needs PKG")?.to_string();
            Ok(Cmd::AmKill(pkg))
        }
        "broadcast" => {
            let mut action: Option<String> = None;
            let mut component: Option<String> = None;
            let mut data: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "-a" | "--action" => {
                        i += 1;
                        action = Some(rest.get(i).ok_or("-a needs a value")?.to_string());
                    }
                    "-n" | "--component" => {
                        i += 1;
                        component = Some(rest.get(i).ok_or("-n needs a value")?.to_string());
                    }
                    "-d" | "--data" => {
                        i += 1;
                        data = Some(rest.get(i).ok_or("-d needs a value")?.to_string());
                    }
                    other => return Err(format!("unknown am broadcast arg: {other}")),
                }
                i += 1;
            }
            Ok(Cmd::AmBroadcast { action, component, data })
        }
        other => Err(format!("unknown am subcommand: {other}")),
    }
}

fn parse_int(s: &str) -> Result<i32, std::num::ParseIntError> {
    if let Some(stripped) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i32::from_str_radix(stripped, 16)
    } else {
        s.parse::<i32>()
    }
}

// ---------- runner ----------

pub(crate) fn run(host: &str, port: u16, cmd: &Cmd) -> io::Result<()> {
    // Fast path: state queries served straight out of the host-side mirror
    // file (populated by the `state-daemon` watcher that `connect` spawns).
    // No TCP, no parsing on the wire — just a file read + tiny string scan.
    if let Cmd::State(field) = cmd {
        if let Some(snap) = state_cache::read_cached(port) {
            return print_state_field(&snap, field);
        }
    }

    let mut conn = Conn::connect(host, port)?;
    match cmd {
        Cmd::Pull { device, host } => do_pull(&mut conn, device, host.as_deref()),
        Cmd::Push { host, device, mode } => do_push(&mut conn, host, device, *mode),
        Cmd::Install { apk, reinstall, grant } => do_install(&mut conn, apk, *reinstall, *grant),
        Cmd::InstallMulti { apks, reinstall, grant } =>
            do_install_multi(&mut conn, apks, *reinstall, *grant),
        Cmd::GetProp(k) => text_call(&mut conn, &format!("getprop {k}")),
        Cmd::SetProp(k, v) => text_call(&mut conn, &format!("setprop {k} {v}")),
        Cmd::Dumpsys { service, args } => {
            let mut wire = format!("dumpsys {service}");
            for a in args { wire.push(' '); wire.push_str(a); }
            stream_call(&mut conn, &wire)
        }
        Cmd::Logcat(args) => {
            let mut wire = String::from("logcat");
            for a in args { wire.push(' '); wire.push_str(a); }
            stream_call(&mut conn, &wire)
        }
        Cmd::SettingsGet { namespace, key } =>
            text_call(&mut conn, &format!("settings_get {namespace} {key}")),
        Cmd::SettingsPut { namespace, key, value } =>
            text_call(&mut conn, &format!("settings_put {namespace} {key} {value}")),
        Cmd::Shell(argv) => {
            let mut wire = String::from("shell");
            for a in argv { wire.push(' '); wire.push_str(a); }
            shell_stream(&mut conn, &wire)
        }
        Cmd::WmInfo => text_call(&mut conn, "wm_info"),
        Cmd::WmRotation(r) => text_call(&mut conn, &format!("wm_rotation {r}")),
        Cmd::Monitor => stream_call(&mut conn, "monitor"),
        Cmd::State(field) => text_call(&mut conn, &format!("state {field}")),
        Cmd::StateWatch => do_state_watch(&mut conn),
        Cmd::PmList { third, system } => {
            let mut wire = String::from("pm_list");
            if *third { wire.push_str(" 3"); }
            if *system { wire.push_str(" s"); }
            text_call(&mut conn, &wire)
        }
        Cmd::PmPath(pkg) => text_call(&mut conn, &format!("pm_path {pkg}")),
        Cmd::PmUninstall(pkg) => text_call(&mut conn, &format!("pm_uninstall {pkg}")),
        Cmd::PmGrant(pkg, perm) => text_call(&mut conn, &format!("pm_grant {pkg} {perm}")),
        Cmd::PmRevoke(pkg, perm) => text_call(&mut conn, &format!("pm_revoke {pkg} {perm}")),
        Cmd::AmStart { component, action, data, flag } => {
            let mut wire = format!("am_start n={component}");
            if let Some(a) = action { wire.push_str(&format!(" a={a}")); }
            if let Some(d) = data   { wire.push_str(&format!(" d={d}")); }
            if let Some(f) = flag   { wire.push_str(&format!(" f={f}")); }
            text_call(&mut conn, &wire)
        }
        Cmd::AmForceStop(pkg) => text_call(&mut conn, &format!("am_force_stop {pkg}")),
        Cmd::AmKill(pkg) => text_call(&mut conn, &format!("am_kill {pkg}")),
        Cmd::AmBroadcast { action, component, data } => {
            let mut wire = String::from("am_broadcast");
            if let Some(a) = action    { wire.push_str(&format!(" a={a}")); }
            if let Some(c) = component { wire.push_str(&format!(" n={c}")); }
            if let Some(d) = data      { wire.push_str(&format!(" d={d}")); }
            text_call(&mut conn, &wire)
        }
    }
}

/// Print a single field extracted from a cached snapshot. For "device" we
/// just dump the whole JSON. For other fields we scan-and-extract.
fn print_state_field(snap: &[u8], field: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    if field == "device" {
        out.write_all(snap)?;
    } else {
        // CLI uses short names; map them to the JSON keys the daemon emits.
        let json_key = match field {
            "top" => "top_activity",
            other => other,
        };
        let v = state_cache::extract_field(snap, json_key).ok_or_else(|| io::Error::other(
            format!("field '{field}' not present in cached snapshot")
        ))?;
        out.write_all(v.as_bytes())?;
    }
    out.write_all(b"\n")?;
    out.flush()
}

fn text_call(conn: &mut Conn, wire: &str) -> io::Result<()> {
    let body = conn.call(wire)?;
    if body.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&body).into_owned()));
    }
    let mut out = io::stdout().lock();
    out.write_all(&body)?;
    // Always end on a newline so chained CLI calls don't run together.
    let needs_newline = !body.is_empty() && body[body.len() - 1] != b'\n';
    if needs_newline { out.write_all(b"\n")?; }
    out.flush()
}

/// Subscribe to State.device() snapshots. Server pushes one length-prefixed
/// frame per state change. Each snapshot is written as a single line on
/// stdout (newline-delimited JSON) and flushed immediately so downstream
/// tools see updates as they happen.
///
/// Runs until the user kills the client or the daemon goes away.
fn do_state_watch(conn: &mut Conn) -> io::Result<()> {
    conn.send_cmd("state_watch")?;
    let mut out = io::stdout().lock();
    loop {
        let frame = conn.read_frame()?;
        if frame.starts_with(b"ERR:") {
            return Err(io::Error::other(String::from_utf8_lossy(&frame).into_owned()));
        }
        out.write_all(&frame)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
}

/// Like `stream_call` but for `shell` passthrough: drops the daemon's
/// `__exit__ N` trailer (added by ShellExec.java to report the exit code).
/// Non-zero exit codes surface as an io::Error.
fn shell_stream(conn: &mut Conn, wire: &str) -> io::Result<()> {
    conn.send_cmd(wire)?;
    let mut out = io::stdout().lock();
    let mut first = true;
    loop {
        let frame = conn.read_frame()?;
        if frame.is_empty() { break; }                     // [len=0] terminator
        if first && frame.starts_with(b"ERR:") {
            return Err(io::Error::other(
                String::from_utf8_lossy(&frame).into_owned()));
        }
        first = false;
        if let Some(code) = parse_exit_trailer(&frame) {
            // Drain the terminator, then surface non-zero exits.
            let _ = conn.read_frame();
            out.flush()?;
            if code != 0 {
                return Err(io::Error::other(format!("shell exited {code}")));
            }
            return Ok(());
        }
        out.write_all(&frame)?;
    }
    out.flush()
}

fn parse_exit_trailer(b: &[u8]) -> Option<i32> {
    std::str::from_utf8(b).ok()?
        .trim()
        .strip_prefix("__exit__ ")?
        .parse()
        .ok()
}

/// Server-streamed call. Sends the command, then dumps chunked frames to
/// stdout until the 0-length terminator.
fn stream_call(conn: &mut Conn, wire: &str) -> io::Result<()> {
    conn.send_cmd(wire)?;
    let mut out = io::stdout().lock();
    conn.recv_chunks_to(&mut out)?;
    out.flush()
}

fn do_install_multi(conn: &mut Conn, apks: &[String], reinstall: bool, grant: bool)
        -> io::Result<()> {
    let mut sizes: Vec<u64> = Vec::with_capacity(apks.len());
    for p in apks {
        sizes.push(File::open(p)?.metadata()?.len());
    }
    let sizes_str = sizes.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");
    let mut wire = format!("install_multi sizes={sizes_str}");
    if reinstall { wire.push_str(" reinstall=1"); }
    if grant     { wire.push_str(" grant=1"); }
    conn.send_cmd(&wire)?;
    for p in apks {
        let mut f = File::open(p)?;
        let _ = conn.send_chunks_from(&mut f)?;
    }
    let reply = conn.read_frame()?;
    if reply.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&reply).into_owned()));
    }
    let mut out = io::stderr().lock();
    out.write_all(&reply)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn do_pull(conn: &mut Conn, device: &str, host: Option<&str>) -> io::Result<()> {
    conn.send_cmd(&format!("pull path={device}"))?;
    let written = match host {
        Some(p) => {
            let mut f = File::create(p)?;
            let n = conn.recv_chunks_to(&mut f)?;
            f.flush()?;
            n
        }
        None => {
            let mut out = io::stdout().lock();
            conn.recv_chunks_to(&mut out)?
        }
    };
    if host.is_some() {
        eprintln!("pulled {written} bytes from {device}");
    }
    Ok(())
}

fn do_push(conn: &mut Conn, host: &str, device: &str, mode: Option<u32>) -> io::Result<()> {
    let mut f = File::open(host)?;
    let size = f.metadata()?.len();
    let mut wire = format!("push path={device} size={size}");
    if let Some(m) = mode { wire.push_str(&format!(" mode=0{m:o}")); }
    conn.send_cmd(&wire)?;
    let written = conn.send_chunks_from(&mut f)?;
    let reply = conn.read_frame()?;
    if reply.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&reply).into_owned()));
    }
    eprintln!("pushed {written} bytes to {device}");
    Ok(())
}

fn do_install(conn: &mut Conn, apk: &str, reinstall: bool, grant: bool) -> io::Result<()> {
    let mut f = File::open(apk)?;
    let size = f.metadata()?.len();
    let mut wire = format!("install size={size}");
    if reinstall { wire.push_str(" reinstall=1"); }
    if grant     { wire.push_str(" grant=1"); }
    conn.send_cmd(&wire)?;
    let _ = conn.send_chunks_from(&mut f)?;
    let reply = conn.read_frame()?;
    if reply.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&reply).into_owned()));
    }
    let mut out = io::stderr().lock();
    out.write_all(&reply)?;
    out.write_all(b"\n")?;
    Ok(())
}

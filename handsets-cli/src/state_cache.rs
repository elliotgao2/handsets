// Host-side state mirror.
//
// `hs connect` spawns a detached child process that subscribes to the
// daemon's `state_watch` stream and atomically rewrites
// `$HOME/.handsets/state-<port>.json` on every push. Subsequent
// `hs state <field>` invocations read that file instead of going over
// the wire — turning a ~2 ms TCP round-trip into a sub-100µs file read.
//
// File layout:
//   ~/.handsets/state-<port>.json    latest snapshot (atomic via tmp+rename)
//   ~/.handsets/state-<port>.pid     PID of the running watcher

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::Conn;

const FRESHNESS_LIMIT: Duration = Duration::from_secs(30);

fn dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".handsets")
}

pub(crate) fn cache_path(port: u16) -> PathBuf {
    dir().join(format!("state-{port}.json"))
}

fn pid_path(port: u16) -> PathBuf {
    dir().join(format!("state-{port}.pid"))
}

/// Read the cached snapshot if it's fresher than {@link FRESHNESS_LIMIT}.
pub(crate) fn read_cached(port: u16) -> Option<Vec<u8>> {
    let p = cache_path(port);
    let meta = fs::metadata(&p).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).unwrap_or(Duration::MAX);
    if age > FRESHNESS_LIMIT {
        return None;
    }
    fs::read(&p).ok()
}

/// Foreground daemon loop: subscribe and persist forever. Used as the body
/// of `hs state-daemon`.
pub(crate) fn run_daemon(host: &str, port: u16) -> io::Result<()> {
    fs::create_dir_all(dir())?;
    let pid_p = pid_path(port);
    fs::write(&pid_p, std::process::id().to_string())?;

    let mut conn = Conn::connect(host, port)?;
    conn.send_cmd("state_watch")?;

    let cache_p = cache_path(port);
    let tmp_p = cache_p.with_extension("json.tmp");
    loop {
        let frame = conn.read_frame()?;
        if frame.starts_with(b"ERR:") {
            return Err(io::Error::other(String::from_utf8_lossy(&frame).into_owned()));
        }
        fs::write(&tmp_p, &frame)?;
        fs::rename(&tmp_p, &cache_p)?;
    }
}

/// Spawn the state-daemon as a detached background child. Returns the PID
/// that's now writing the cache file.
pub(crate) fn spawn_detached(host: &str, port: u16) -> io::Result<u32> {
    // If something's already running for this port, leave it alone.
    if let Some(existing) = pid_alive(port) {
        return Ok(existing);
    }
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .args(["--host", host, "--port", &port.to_string(), "state-daemon"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child.id())
}

/// Stop the watcher for this port, if any.
pub(crate) fn stop_watcher(port: u16) -> io::Result<()> {
    let p = pid_path(port);
    let Ok(raw) = fs::read_to_string(&p) else { return Ok(()); };
    if let Ok(pid) = raw.trim().parse::<i32>() {
        unsafe { libc_kill(pid, 15); }   // SIGTERM
    }
    let _ = fs::remove_file(&p);
    let _ = fs::remove_file(cache_path(port));
    Ok(())
}

fn pid_alive(port: u16) -> Option<u32> {
    let raw = fs::read_to_string(pid_path(port)).ok()?;
    let pid: i32 = raw.trim().parse().ok()?;
    let alive = unsafe { libc_kill(pid, 0) == 0 };   // signal 0 = existence probe
    if alive { Some(pid as u32) } else { None }
}

// Tiny FFI shim — avoids pulling in the `libc` crate just for two calls.
// The symbol name is renamed to dodge std::process::kill.
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// Extract one top-level field from a flat JSON object. The state snapshot
/// has only scalar values (bool / int / string) so we don't pull in a JSON
/// crate — handle the three cases directly.
pub(crate) fn extract_field(json: &[u8], key: &str) -> Option<String> {
    let s = std::str::from_utf8(json).ok()?;
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)?;
    let tail = &s[i + pat.len()..];
    let tail = tail.trim_start();
    if let Some(stripped) = tail.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(stripped[..end].to_string())
    } else if tail.starts_with("true") {
        Some("true".into())
    } else if tail.starts_with("false") {
        Some("false".into())
    } else if tail.starts_with("null") {
        Some("null".into())
    } else {
        let end = tail.find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
            .unwrap_or(tail.len());
        Some(tail[..end].to_string())
    }
}

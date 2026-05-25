// `hs tui` — launch the standalone keyboard-driven UI inspector at
// `handsets-tui/`. Mirrors mirror.rs: we just locate the binary and exec
// it with --host / --port pointing at the daemon, so the `hs` binary
// itself stays free of ratatui / crossterm deps.

use std::io;
use std::process::Command;

use crate::daemon;

pub(crate) fn run(host: &str, port: u16) -> io::Result<()> {
    let bin = daemon::locate_tui().ok_or_else(|| io::Error::new(
        io::ErrorKind::NotFound,
        "handsets-tui binary not found. Build it with:\n  \
         cargo build --release --manifest-path handsets-tui/Cargo.toml\n\
         Then ensure it's on PATH, or run hs from the workspace root.",
    ))?;

    let status = Command::new(bin)
        .args(["--host", host, "--port", &port.to_string()])
        .status()?;

    if !status.success() {
        return Err(io::Error::other(format!("handsets-tui exited with status {status}")));
    }
    Ok(())
}

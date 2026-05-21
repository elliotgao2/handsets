// `hs mirror` — launch the standalone GUI viewer at
// `handsets-viewer/`, which renders the daemon's frames through Metal (macOS)
// with a zero-copy VideoToolbox path for H.264.
//
// The previous Kitty-graphics-protocol terminal mirror has been removed; this
// crate just locates the viewer binary and exec's it with the right
// --host / --port pointing at the daemon.

use std::io;
use std::process::Command;

use crate::daemon;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Args;

pub(crate) fn run(host: &str, port: u16, _args: Args) -> io::Result<()> {
    let viewer = daemon::locate_viewer().ok_or_else(|| io::Error::new(
        io::ErrorKind::NotFound,
        "handsets-viewer binary not found. Build it with:\n  \
         cargo build --release --manifest-path handsets-viewer/Cargo.toml\n\
         Then ensure it's on PATH, or run hs from the workspace root.",
    ))?;

    let status = Command::new(viewer)
        .args(["--host", host, "--port", &port.to_string()])
        .status()?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "handsets-viewer exited with status {status}"
        )));
    }
    Ok(())
}

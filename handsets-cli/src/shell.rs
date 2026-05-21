// `hs shell` — interactive REPL over the daemon's persistent socket.
//
// Same wire syntax as the daemon protocol — `ping`, `dump_active`,
// `tap x=720 y=1500`, `node_click text=hello`. Streaming commands
// (`pull`, `dumpsys`, `logcat`, `shell`, `monitor`, `state_watch`,
// `stream*`) drain frames until the daemon's zero-length terminator.
//
// Built-ins (handled client-side, never hit the wire):
//   help, ?            — verb cheat-sheet
//   exit, quit         — leave the REPL
//   history            — print accepted lines from ~/.handsets/history
//   clear              — clear the terminal
//
// Batch mode kicks in automatically when stdin isn't a TTY: lines are read,
// run, responses emitted with no prompts. Lines starting with `#` are
// ignored. `cat script.txt | hs shell` works as a fast scripted driver.

use std::fs::OpenOptions;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;

use crate::Conn;

const STREAM_COMMANDS: &[&str] = &[
    "pull", "dumpsys", "logcat", "shell", "monitor",
    "stream", "stream_h264", "stream_tilejpeg",
];

const BANNER: &str = "\
hs shell — connected to {ADDR}. ^D / `exit` to leave.
  hint: daemon wire-cmds work directly (`ping`, `dump_active`, `tap x=… y=…`);
        anything else falls through to /system/bin/sh (`ls`, `pwd`, `ps -A`, …).
        `help` lists the wire surface.
";

const HELP: &str = "\
Built-in:    help | ?    exit | quit    history    clear
Daemon:      ping  dump  dump_active  screenshot
             state device | state device_fresh | state_watch
             tap x=N y=N  swipe x1=N y1=N x2=N y2=N dur=N  swipe_dir up|down|left|right
             text \"…\"  key BACK|HOME|RECENTS|…
             node_click text=…  node_set_text id=… value=…  node_scroll id=… dir=forward
             wait_for_idle [idle_ms=N]  wait_for_text text=…  wait_for_activity n=PKG
             am_start n=PKG/.X  am_force_stop PKG  am_kill PKG
             pm_list  pm_path PKG  pm_uninstall PKG  pm_grant PKG PERM  pm_revoke …
             pull path=/…  push path=/… size=N  install size=N [reinstall=1]
             getprop KEY  setprop KEY VAL
             settings_get NS KEY  settings_put NS KEY VAL
             wm_info  wm_rotation N  dumpsys SERVICE  logcat -d -t N  monitor
";

pub(crate) fn run(host: &str, port: u16) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    let tty = stdin.is_terminal();
    let history = history_path();

    if tty {
        write!(out, "{}", BANNER.replace("{ADDR}", &format!("{host}:{port}")))?;
        out.flush()?;
    }

    let mut reader = stdin.lock();
    let mut line = String::new();
    loop {
        if tty {
            out.write_all(b"hs> ")?;
            out.flush()?;
        }
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 { break; }
        let cmd = line.trim();
        if cmd.is_empty() || cmd.starts_with('#') { continue; }

        // Built-ins handled client-side.
        match cmd {
            "exit" | "quit" => break,
            "help" | "?" => { write!(out, "{HELP}")?; out.flush()?; continue; }
            "clear"    => { out.write_all(b"\x1b[2J\x1b[H")?; out.flush()?; continue; }
            "history"  => { dump_history(&history, &mut out)?; continue; }
            _ => {}
        }

        // Persist the accepted line so `history` shows it next time.
        let _ = append_history(&history, cmd);

        let head = cmd.split_whitespace().next().unwrap_or("");
        let streaming = STREAM_COMMANDS.contains(&head) || head == "state_watch";
        let r = if streaming {
            dispatch_stream(&mut conn, cmd, &mut out)
        } else {
            dispatch_text(&mut conn, cmd, &mut out)
        };
        if let Err(e) = r {
            writeln!(out, "ERR: {e}")?;
            if matches!(e.kind(),
                io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::UnexpectedEof) {
                return Err(e);
            }
        }
    }
    Ok(())
}

// ---------- helpers ----------

fn history_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".handsets").join("history")
}

fn append_history(path: &PathBuf, line: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

fn dump_history(path: &PathBuf, out: &mut io::StdoutLock<'_>) -> io::Result<()> {
    match std::fs::read_to_string(path) {
        Ok(s)  => { out.write_all(s.as_bytes())?; out.flush() }
        Err(_) => { writeln!(out, "(no history yet)") }
    }
}

fn dispatch_text(conn: &mut Conn, cmd: &str, out: &mut io::StdoutLock<'_>) -> io::Result<()> {
    let body = conn.call(cmd)?;
    // If the daemon doesn't know the verb, transparently re-fire it as
    // `shell <cmd>` so `ls`, `pwd`, `cat /foo`, `ps -A` etc. just work.
    if is_unknown_cmd(&body) {
        return dispatch_shell_fallback(conn, cmd, out);
    }
    out.write_all(&body)?;
    if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
    out.flush()
}

fn dispatch_stream(conn: &mut Conn, cmd: &str, out: &mut io::StdoutLock<'_>) -> io::Result<()> {
    conn.send_cmd(cmd)?;
    conn.recv_chunks_to(out)?;
    out.flush()
}

/// "ERR:unknown-cmd:<head>" — the daemon's well-known no-such-command response.
fn is_unknown_cmd(body: &[u8]) -> bool {
    body.starts_with(b"ERR:unknown-cmd:")
}

/// Send the line as a shell exec, drain stdout, drop the `__exit__ N`
/// trailer. Lets the REPL feel like a real Android shell when the verb
/// isn't part of the daemon's wire protocol.
fn dispatch_shell_fallback(conn: &mut Conn, cmd: &str, out: &mut io::StdoutLock<'_>) -> io::Result<()> {
    let wire = format!("shell {cmd}");
    conn.send_cmd(&wire)?;
    loop {
        let frame = conn.read_frame()?;
        if frame.is_empty() { break; }
        if frame.starts_with(b"ERR:") {
            return Err(io::Error::other(String::from_utf8_lossy(&frame).into_owned()));
        }
        // Drop the daemon's exit-code trailer.
        if let Some(s) = std::str::from_utf8(&frame).ok()
            .and_then(|s| s.trim().strip_prefix("__exit__ "))
        {
            // Drain the [len=0] terminator and surface non-zero exits.
            let _ = conn.read_frame();
            out.flush()?;
            if let Ok(code) = s.parse::<i32>() {
                if code != 0 {
                    writeln!(out, "(exit {code})")?;
                }
            }
            return Ok(());
        }
        out.write_all(&frame)?;
    }
    out.flush()
}

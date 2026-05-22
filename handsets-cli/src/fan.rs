// `hs fan SERIAL,SERIAL,... -- VERB ARGS` — spawn one child `hs` per device
// in parallel and aggregate the results. Each child re-executes the current
// binary with the device-specific `--port` resolved from the host-side
// forward listing, so any verb that works in one-shot mode works fanned.
//
// We deliberately spawn subprocesses rather than driving multiple sockets
// in-process. The action verbs already encapsulate retry/reporting; a
// subprocess fan-out reuses all of that and isolates per-device failures.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::daemon;
use crate::flags::OutFmt;
use crate::json_out::Obj;

pub fn run(out_fmt: OutFmt, serials: &[String], argv: &[String]) -> io::Result<()> {
    if argv.is_empty() {
        return Err(io::Error::other("fan: nothing to run after `--`"));
    }
    let rows = daemon::devices()?;
    // Map serial → host_port so the child invocation hits the right daemon.
    let exe = std::env::current_exe()?;
    let exe = Arc::new(exe);

    let mut handles = Vec::with_capacity(serials.len());
    let outputs: Arc<Mutex<Vec<(String, FanResult)>>> = Arc::new(Mutex::new(Vec::new()));
    for serial in serials {
        let port = rows.iter()
            .find(|r| &r.serial == serial)
            .and_then(|r| r.host_port);
        let exe = Arc::clone(&exe);
        let serial = serial.clone();
        let argv = argv.to_vec();
        let outputs = Arc::clone(&outputs);
        handles.push(thread::spawn(move || {
            let mut cmd = Command::new(&*exe);
            if let Some(p) = port {
                cmd.args(["--port", &p.to_string()]);
            }
            cmd.args(&argv);
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
            let out = cmd.output();
            let res = match out {
                Ok(o) => FanResult {
                    code:   o.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
                },
                Err(e) => FanResult {
                    code: -1,
                    stdout: String::new(),
                    stderr: format!("spawn-failed: {e}"),
                },
            };
            outputs.lock().unwrap().push((serial, res));
        }));
    }
    for h in handles { let _ = h.join(); }

    let mut results = Arc::try_unwrap(outputs)
        .map_err(|_| io::Error::other("fan: lock poisoned"))?
        .into_inner()
        .unwrap();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    let mut overall_failure = false;
    let mut out = io::stdout().lock();
    match out_fmt {
        OutFmt::Human => {
            for (serial, r) in &results {
                if r.code != 0 { overall_failure = true; }
                writeln!(out, "=== {serial} (exit {}) ===", r.code)?;
                if !r.stdout.is_empty() {
                    out.write_all(r.stdout.as_bytes())?;
                    if !r.stdout.ends_with('\n') { out.write_all(b"\n")?; }
                }
                if !r.stderr.is_empty() {
                    out.write_all(r.stderr.as_bytes())?;
                    if !r.stderr.ends_with('\n') { out.write_all(b"\n")?; }
                }
            }
        }
        OutFmt::Json => {
            for (serial, r) in &results {
                if r.code != 0 { overall_failure = true; }
                let line = Obj::new()
                    .s("verb", "fan")
                    .s("device", serial)
                    .n("exit", r.code as i64)
                    .s("stdout", &r.stdout)
                    .s("stderr", &r.stderr)
                    .finish();
                writeln!(out, "{line}")?;
            }
        }
    }
    if overall_failure {
        return Err(io::Error::other("fan: one or more devices failed"));
    }
    Ok(())
}

struct FanResult {
    code: i32,
    stdout: String,
    stderr: String,
}

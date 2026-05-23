// hs CLI.
//
// Talks to the on-device daemon over a TCP socket using a length-prefixed
// binary protocol (uint32 big-endian length + payload, both directions).
//
// One-shot calls open a fresh socket each invocation. The `bench` subcommand
// reuses a persistent socket to measure true wire-level latency.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

mod act;
mod adb;
mod daemon;
mod errors;
mod fan;
mod flags;
mod init;
mod json;
mod json_out;
mod mirror;
mod output;
mod provider;
mod run_script;
mod screen;
mod selector;
mod session;
mod shell;
mod snapshot;
mod state_cache;
mod tap_text;
mod term;
mod ui_dump;
mod usage;
mod xml_dump;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 9008;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // If the verb funnelled its failure through `output::Reporter`,
            // the structured ErrCode rides in the error payload so we can
            // surface a distinct exit status. Otherwise this is a generic
            // I/O / setup failure and we fall back to 1.
            if let Some(code) = output::err_code_of(&e) {
                ExitCode::from(code.exit_code())
            } else {
                eprintln!("hs: {e}");
                ExitCode::from(1)
            }
        }
    }
}

use usage::USAGE;

#[derive(Debug)]
struct Opts {
    host: String,
    port: u16,
    out_fmt: flags::OutFmt,
    cmd: Cmd,
}

#[derive(Debug)]
enum Cmd {
    Ping,
    Find { selector: String, flags: flags::ActionFlags },
    Input(String),       // pre-built wire command, e.g. "tap x=720 y=1500"
    TapText { query: String, flags: flags::ActionFlags },
    TapXY { x: i32, y: i32, flags: flags::ActionFlags },
    Snapshot,
    Screen,
    Quit,
    Bench { n: u32 },
    Adb(adb::Cmd),
    Connect(daemon::ConnectOpts),
    Disconnect(daemon::DisconnectOpts),
    See(Option<String>),
    Wait { spec: String, flags: flags::ActionFlags },
    Cp { src: String, dst: String },
    ShowPkg(String),
    Info,
    TypeFocused { text: String, flags: flags::ActionFlags },
    TypeInto { selector: String, text: String, flags: flags::ActionFlags },
    SettingsListAll,
    Ui { format: UiFormat, all: bool },
    Devices,
    StateDaemon,
    Shell,
    Run { script: Option<String>, flags: flags::ActionFlags },  // hs run [SCRIPT|-]
    Act(act::ActOpts),                                          // hs act --tap … --until …
    Fan { serials: Vec<String>, argv: Vec<String> },            // hs fan SERIALS -- VERB ARGS
    Init { path: Option<String> },                              // hs init [PATH]
    Submit { sel: Option<String>, flags: flags::ActionFlags },
    Paste { sel: Option<String>, flags: flags::ActionFlags },
    Links(String),
    Sms { kind: String, limit: u32, json: bool },
    Calls { kind: String, limit: u32, json: bool },
    Contacts { limit: u32, json: bool },
    Calendar { from_ms: Option<i64>, to_ms: Option<i64>, days: Option<i64>, limit: u32, json: bool },
    Notif { pkg: Option<String>, history: bool, limit: u32, json: bool },
    Clip { text: Option<String>, watch: bool, interval_ms: u64 },
}

// SMS type codes — Android Telephony.TextBasedSmsColumns.
const SMS_TYPES: &[(i64, &str)] = &[
    (1, "inbox"), (2, "sent"), (3, "draft"),
    (4, "outbox"), (5, "failed"), (6, "queued"),
];
// Call log type codes — Android CallLog.Calls.TYPE.
const CALL_TYPES: &[(i64, &str)] = &[
    (1, "in"), (2, "out"), (3, "missed"), (4, "voicemail"),
    (5, "rejected"), (6, "blocked"), (7, "external"),
];
// Phone-number type codes — Android ContactsContract.CommonDataKinds.Phone.
const PHONE_TYPES: &[(i64, &str)] = &[
    (1, "home"),     (2, "mobile"),     (3, "work"),
    (4, "fax-work"), (5, "fax-home"),   (6, "pager"),
    (7, "other"),    (9, "car"),        (10, "company"),
    (12, "main"),    (17, "work-mob"),
];

#[derive(Debug, Clone, Copy)]
enum UiFormat { Human, Interactive, Json, Xml }

fn parse_use(rest: &[&str]) -> Result<daemon::ConnectOpts, String> {
    let mut opts = daemon::ConnectOpts { serial: None, port: None };
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-s" | "--device" => {
                i += 1;
                opts.serial = Some(rest.get(i).ok_or("--device needs a SERIAL")?.to_string());
            }
            "--port" => {
                i += 1;
                opts.port = Some(rest.get(i).ok_or("--port needs a value")?
                    .parse().map_err(|_| "invalid --port".to_string())?);
            }
            other => return Err(format!("unknown connect arg: {other}")),
        }
        i += 1;
    }
    Ok(opts)
}

fn parse_drop(rest: &[&str]) -> Result<daemon::DisconnectOpts, String> {
    let mut opts = daemon::DisconnectOpts { serial: None, keep_jar: false };
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-s" | "--device" => {
                i += 1;
                opts.serial = Some(rest.get(i).ok_or("--device needs a SERIAL")?.to_string());
            }
            "--keep-jar" => opts.keep_jar = true,
            other => return Err(format!("unknown disconnect arg: {other}")),
        }
        i += 1;
    }
    Ok(opts)
}

/// `hs tap` — text-lookup when arg isn't a pair of integers, raw coords
/// when it is. Strips the shared ActionFlags surface first so RPA scripts
/// can `hs tap "Login" --visible --unique --timeout 5s --retries 3`.
fn parse_tap(rest: &[&str]) -> Result<Cmd, String> {
    let mut flags = flags::ActionFlags::default();
    let positional = flags.take(rest)?;
    if positional.is_empty() {
        return Err("tap needs either TEXT or X Y coords".into());
    }
    if positional.len() == 2 {
        let (x, y) = (positional[0].parse::<i32>(), positional[1].parse::<i32>());
        if let (Ok(x), Ok(y)) = (x, y) {
            return Ok(Cmd::TapXY { x, y, flags });
        }
    }
    if positional.len() == 1 && positional[0].parse::<i32>().is_ok() {
        return Err("tap with a single number is ambiguous — pass two ints for coords, or quote text".into());
    }
    Ok(Cmd::TapText { query: positional.join(" "), flags })
}

/// True if `s` looks like a package name: dot-separated, no slash.
fn is_pkg(s: &str) -> bool {
    !s.contains('/') && s.contains('.') &&
        s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// Friendly "did you mean" for retired verbs.
fn suggest(old: &str) -> String {
    let hint = match old {
        "devices"      => Some("hs"),
        "connect"      => Some("hs use"),
        "disconnect"   => Some("hs drop"),
        "screenshot"   => Some("hs see <PATH.jpg|.png>"),
        "dump" | "dump-active" => Some("hs see <PATH.xml|.json>"),
        "query"        => Some("hs find <SELECTOR>"),
        "mirror"       => Some("hs see   (bare opens the viewer)"),
        "pull" | "push" => Some("hs cp  (device:/path host:/path or reverse)"),
        "pm"           => Some("hs apps / hs install / hs uninstall / hs show <pkg>"),
        "am"           => Some("hs open / hs close / hs watch events"),
        "getprop" | "setprop" => Some("hs prop KEY [VAL]"),
        "logcat"       => Some("hs logs"),
        "monitor"      => Some("hs events"),
        "watch"        => Some("hs logs | hs events | hs do state_watch"),
        "dumpsys"      => Some("hs do dumpsys <svc>"),
        "state"        => Some("hs show  /  hs watch"),
        "input"        => Some("hs tap / hs type / hs go / hs swipe"),
        _              => None,
    };
    match hint {
        Some(h) => format!("unknown command: {old}  — try `{h}`"),
        None    => format!("unknown command: {old}"),
    }
}

/// `hs dev <sub>` — explicit namespace for the daemon-debugging verbs.
/// Bare `hs dev` prints what's available; `hs dev ping` etc. dispatch the
/// same `Cmd` variants the top-level aliases produce.
fn parse_dev(rest: &[&str]) -> Result<Cmd, String> {
    const HELP: &str = "\
hs dev <sub>:
  ping              round-trip the daemon socket
  snapshot          print the cached state JSON
  screen            print one screenshot frame to stdout
  bench [-n N]      timed wire-call benchmark (default 50 iterations)
  quit              ask the daemon to exit (`hs drop` is friendlier)
  state-daemon      run the host-side state mirror (used by `hs use`)";
    match rest.split_first() {
        None => Err(HELP.to_string()),
        Some((&"ping",         _))   => Ok(Cmd::Ping),
        Some((&"snapshot",     _))   => Ok(Cmd::Snapshot),
        Some((&"screen",       _))   => Ok(Cmd::Screen),
        Some((&"quit",         _))   => Ok(Cmd::Quit),
        Some((&"state-daemon", _))   => Ok(Cmd::StateDaemon),
        Some((&"bench",        more)) => parse_bench(more),
        Some((other, _)) => Err(format!("hs dev: unknown sub-verb '{other}'\n{HELP}")),
    }
}

fn parse_bench(rest: &[&str]) -> Result<Cmd, String> {
    let mut n: u32 = 50;
    let mut j = 0;
    while j < rest.len() {
        match rest[j] {
            "-n" => {
                j += 1;
                n = rest.get(j).ok_or("-n needs a value")?.parse()
                    .map_err(|_| "invalid -n value")?;
            }
            other => return Err(format!("unknown bench arg: {other}")),
        }
        j += 1;
    }
    Ok(Cmd::Bench { n })
}

/// Shared arg parser for the simple provider verbs (`sms`, `calls`,
/// `contacts`): consumes `--limit N` and `--json`, collects anything
/// else as positional tokens.
fn parse_provider_common<'a>(rest: &[&'a str]) -> Result<(u32, bool, Vec<&'a str>), String> {
    let mut limit = 50_u32;
    let mut json = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--limit" => {
                i += 1;
                limit = rest.get(i).ok_or("--limit needs N")?.parse()
                    .map_err(|_| "bad --limit".to_string())?;
            }
            "--json" => json = true,
            other => positional.push(other),
        }
        i += 1;
    }
    Ok((limit, json, positional))
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT;
    let mut out_fmt = flags::OutFmt::from_env();
    let mut device_serial: Option<String> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--host" => {
                i += 1;
                host = args.get(i).ok_or("--host needs a value")?.clone();
            }
            "--port" => {
                i += 1;
                port = args
                    .get(i)
                    .ok_or("--port needs a value")?
                    .parse()
                    .map_err(|_| "invalid --port".to_string())?;
            }
            "-s" | "--device" => {
                i += 1;
                device_serial = Some(args.get(i)
                    .ok_or("--device needs a SERIAL")?.clone());
            }
            "--json" => out_fmt = flags::OutFmt::Json,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => positional.push(a),
        }
        i += 1;
    }

    // `--device SERIAL` resolves to the per-device forwarded port so the
    // rest of the CLI can stay device-agnostic. We deliberately do this
    // before verb parsing so a missing forward fails fast with a clear
    // error rather than later as "no daemon listening".
    if let Some(serial) = device_serial.as_deref() {
        match resolve_device_port(serial) {
            Ok(p) => port = p,
            Err(e) => return Err(e),
        }
    }

    let cmd = match positional.split_first() {
        // Bare `hs` lists devices — Reitz-style: the noun is implicit.
        None => Cmd::Devices,

        // ─── Lifecycle ────────────────────────────────────────────────
        Some((&"use", rest))  => Cmd::Connect(parse_use(rest)?),
        Some((&"drop", rest)) => Cmd::Disconnect(parse_drop(rest)?),

        // ─── See — capture by extension, bare = viewer ───────────────
        Some((&"see", rest)) => {
            if rest.is_empty() {
                Cmd::See(None)
            } else if rest.len() == 1 {
                Cmd::See(Some(rest[0].to_string()))
            } else {
                return Err("see takes at most one PATH (extension picks format)".into());
            }
        }

        // ─── Apps ─────────────────────────────────────────────────────
        Some((&"apps", rest)) => {
            let mut third = false;
            for tok in rest {
                match *tok {
                    "--3" | "--3rd" | "--third-party" => third = true,
                    "--plain" => { /* placeholder for future formatting flag */ }
                    other => return Err(format!("unknown apps arg: {other}")),
                }
            }
            Cmd::Adb(adb::Cmd::PmList { third, system: false })
        }
        Some((&"open", rest)) => {
            let c = rest.first().ok_or("open needs COMPONENT (pkg or pkg/.Class)")?;
            Cmd::Adb(adb::Cmd::AmStart {
                component: c.to_string(),
                action: None, data: None, flag: None,
            })
        }
        Some((&"close", rest)) => {
            let p = rest.first().ok_or("close needs PKG")?;
            Cmd::Adb(adb::Cmd::AmForceStop(p.to_string()))
        }
        Some((&"install", rest)) => Cmd::Adb(adb::parse_install(rest)?),
        Some((&"uninstall", rest)) => {
            let p = rest.first().ok_or("uninstall needs PKG")?;
            Cmd::Adb(adb::Cmd::PmUninstall(p.to_string()))
        }

        // ─── Query / state ───────────────────────────────────────────
        Some((&"find", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            if positional.is_empty() { return Err("find needs a CSS-like SELECTOR".into()); }
            Cmd::Find { selector: positional.join(" "), flags }
        }
        Some((&"ui", rest)) => {
            // `hs ui` defaults to the flat, agent-friendly interactive
            // table — that's the 99% caller in an LLM loop and what the
            // README sells. The indented outline is still available via
            // `--tree`; `-i` / `--interactive` remain accepted no-ops so
            // older scripts keep working.
            let mut fmt = UiFormat::Interactive;
            let mut all = false;
            for tok in rest {
                match *tok {
                    "--json"            => fmt = UiFormat::Json,
                    "--xml"             => fmt = UiFormat::Xml,
                    "--tree"            => fmt = UiFormat::Human,
                    "-i" | "--interactive" => fmt = UiFormat::Interactive,
                    "--all"             => all = true,
                    other               => return Err(format!("unknown ui arg: {other}")),
                }
            }
            Cmd::Ui { format: fmt, all }
        }
        Some((&"show", rest)) => match rest.first() {
            None => Cmd::Adb(adb::Cmd::State("device".into())),
            Some(&"top") => Cmd::Adb(adb::Cmd::State("top".into())),
            Some(arg) if is_pkg(arg) => Cmd::ShowPkg(arg.to_string()),
            Some(other) => return Err(format!("show: unknown target '{other}'")),
        }

        // ─── Wait — smart-dispatched at runtime ──────────────────────
        Some((&"wait", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            if positional.is_empty() { return Err("wait needs SPEC (idle | TEXT | PKG | Nms)".into()); }
            Cmd::Wait { spec: positional.join(" "), flags }
        }

        // ─── Input ───────────────────────────────────────────────────
        Some((&"tap", rest)) => parse_tap(rest)?,
        Some((&"type", rest)) => {
            // `type TEXT` types into the focused field via KeyEvents.
            // For atomic set-text on a specific node, use `hs fill SELECTOR TEXT`
            // (Playwright vocabulary; ACTION_SET_TEXT, bypasses the IME).
            let mut tflags = flags::ActionFlags::default();
            let positional = tflags.take(rest)?;
            match positional.len() {
                0 => return Err("type needs TEXT".into()),
                1 => Cmd::TypeFocused { text: positional[0].into(), flags: tflags },
                _ => return Err(
                    "type takes one TEXT argument; use `hs fill SELECTOR TEXT` to target a node".into()
                ),
            }
        }
        Some((&"fill", rest)) => {
            // ACTION_SET_TEXT against the matching node. Atomic, bypasses the
            // IME — matches Playwright's `page.fill(selector, value)`.
            let mut tflags = flags::ActionFlags::default();
            let positional = tflags.take(rest)?;
            match positional.len() {
                0 | 1 => return Err("fill needs SELECTOR TEXT".into()),
                2 => Cmd::TypeInto {
                    selector: positional[0].into(),
                    text:     positional[1].into(),
                    flags:    tflags,
                },
                _ => return Err(
                    "fill takes SELECTOR TEXT — quote multi-word arguments".into()
                ),
            }
        }
        Some((&"go", rest)) => {
            let k = rest.first().ok_or("go needs KEY (back|home|recents|enter|…)")?;
            Cmd::Input(format!("key {}", k.to_uppercase()))
        }
        Some((&"swipe", rest)) => {
            // Direction shortcut: `hs swipe left|right|up|down [DUR_MS]`.
            // Daemon resolves the coordinates from the live display size.
            if let Some(&first) = rest.first() {
                let d = first.to_lowercase();
                if matches!(d.as_str(), "left" | "right" | "up" | "down") {
                    if rest.len() > 2 {
                        return Err("swipe DIR takes at most one extra arg (DUR_MS)".into());
                    }
                    // Only pass `dur=` if the user gave one; let the daemon's
                    // 500ms default win otherwise (reads as a drag, not a fling).
                    let wire = match rest.get(1) {
                        Some(s) => {
                            let dur: i32 = s.parse().map_err(|_| "bad swipe DUR_MS")?;
                            format!("swipe_dir {d} dur={dur}")
                        }
                        None => format!("swipe_dir {d}"),
                    };
                    return Ok(Opts { host, port, out_fmt, cmd: Cmd::Input(wire) });
                }
            }
            if rest.len() < 4 || rest.len() > 5 {
                return Err("swipe needs DIR [DUR_MS] or X1 Y1 X2 Y2 [DUR_MS]".into());
            }
            let x1: i32 = rest[0].parse().map_err(|_| "bad swipe X1")?;
            let y1: i32 = rest[1].parse().map_err(|_| "bad swipe Y1")?;
            let x2: i32 = rest[2].parse().map_err(|_| "bad swipe X2")?;
            let y2: i32 = rest[3].parse().map_err(|_| "bad swipe Y2")?;
            let dur: i32 = if rest.len() == 5 {
                rest[4].parse().map_err(|_| "bad swipe DUR_MS")?
            } else { 300 };
            Cmd::Input(format!("swipe x1={x1} y1={y1} x2={x2} y2={y2} dur={dur}"))
        }

        // ─── Files ───────────────────────────────────────────────────
        Some((&"cp", rest)) => {
            if rest.len() != 2 { return Err("cp needs SRC DST (one side starts with `device:`)".into()); }
            Cmd::Cp { src: rest[0].into(), dst: rest[1].into() }
        }

        // ─── Properties + settings (arity-dispatched) ────────────────
        Some((&"prop", rest)) => match rest.len() {
            // Bare `hs prop` → dump every property via the on-device getprop tool.
            0 => Cmd::Adb(adb::Cmd::Shell(vec![
                "/system/bin/getprop".into(),
            ])),
            1 => Cmd::Adb(adb::Cmd::GetProp(rest[0].into())),
            2 => Cmd::Adb(adb::Cmd::SetProp(rest[0].into(), rest[1].into())),
            _ => return Err("prop needs nothing (list all), KEY (get) or KEY VALUE (set)".into()),
        }
        Some((&"settings", rest)) => match rest.len() {
            // `hs settings` → composite of all three namespaces. The bulk
            // listing is a Settings variant handled by run() below; we use
            // a marker constant so the existing dispatcher routes correctly.
            0 => Cmd::SettingsListAll,
            1 => {
                let ns = rest[0];
                if !matches!(ns, "system" | "secure" | "global") {
                    return Err("settings NS must be system|secure|global".into());
                }
                Cmd::Adb(adb::Cmd::Shell(vec![
                    "/system/bin/settings".into(), "list".into(), ns.into(),
                ]))
            }
            2 => Cmd::Adb(adb::Cmd::SettingsGet { namespace: rest[0].into(), key: rest[1].into() }),
            n if n >= 3 => Cmd::Adb(adb::Cmd::SettingsPut {
                namespace: rest[0].into(),
                key: rest[1].into(),
                value: rest[2..].join(" "),
            }),
            _ => unreachable!(),
        }

        // ─── Logs / events (first-class) ─────────────────────────────
        Some((&"logs", rest)) => {
            // Default: `logcat -d -t 100` (last 100 lines, exit). `--follow`/`-f`
            // switches to live tail. `--tail N` overrides the count.
            let mut follow = false;
            let mut tail: u32 = 100;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "-f" | "--follow" => follow = true,
                    "-t" | "--tail" => {
                        i += 1;
                        let v = rest.get(i).ok_or("--tail needs N")?;
                        tail = v.parse().map_err(|_| format!("bad --tail {v}"))?;
                    }
                    other => return Err(format!("unknown logs arg: {other}")),
                }
                i += 1;
            }
            let mut args: Vec<String> = Vec::new();
            if !follow {
                args.push("-d".into());
                args.push("-t".into());
                args.push(tail.to_string());
            }
            Cmd::Adb(adb::Cmd::Logcat(args))
        }
        Some((&"events", _)) => Cmd::Adb(adb::Cmd::Monitor),
        Some((&"info", _))   => Cmd::Info,

        // ─── Submit (IME action button) ──────────────────────────────
        Some((&"submit", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            let sel = if positional.is_empty() { None } else { Some(positional.join(" ")) };
            Cmd::Submit { sel, flags }
        }

        // ─── Paste (ACTION_PASTE on focused/selected field) ──────────
        Some((&"paste", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            let sel = if positional.is_empty() { None } else { Some(positional.join(" ")) };
            Cmd::Paste { sel, flags }
        }

        // ─── Batch / session execution ───────────────────────────────
        Some((&"run", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            let script = positional.first().map(|s| s.to_string());
            Cmd::Run { script, flags }
        }

        // ─── hs act — one-shot tap-then-assert composite ─────────────
        Some((&"act", rest)) => Cmd::Act(act::parse(rest)?),

        // ─── hs fan — multi-device fan-out ───────────────────────────
        Some((&"fan", rest)) => {
            // Form: `hs fan SERIAL,SERIAL[,...] -- VERB ARGS`
            let idx = rest.iter().position(|t| *t == "--")
                .ok_or("fan: expected `--` between serial list and verb")?;
            let serials_blob = rest[..idx].join(" ");
            let serials: Vec<String> = serials_blob.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
            if serials.is_empty() { return Err("fan: serial list is empty".into()); }
            let argv: Vec<String> = rest[idx + 1..].iter().map(|s| s.to_string()).collect();
            if argv.is_empty() { return Err("fan: nothing to run after `--`".into()); }
            Cmd::Fan { serials, argv }
        }

        // ─── hs init — scaffold a starter script ─────────────────────
        Some((&"init", rest)) => Cmd::Init {
            path: rest.first().map(|s| s.to_string()),
        },

        // ─── Deeplinks (parsed straight from the APK's AndroidManifest) ─
        Some((&"links", rest)) => {
            let pkg = rest.first().ok_or("links needs PKG")?;
            Cmd::Links(pkg.to_string())
        }

        // ─── ContentProvider readers ─────────────────────────────────
        Some((&"sms", rest)) => {
            let (limit, json, positional) = parse_provider_common(rest)?;
            let kind = positional.first().copied().unwrap_or("inbox").to_string();
            Cmd::Sms { kind, limit, json }
        }
        Some((&"calls", rest)) => {
            let (limit, json, positional) = parse_provider_common(rest)?;
            let kind = positional.first().copied().unwrap_or("all").to_string();
            Cmd::Calls { kind, limit, json }
        }
        Some((&"contacts", rest)) => {
            let (limit, json, positional) = parse_provider_common(rest)?;
            if let Some(other) = positional.first() {
                return Err(format!("contacts takes no positional: {other}"));
            }
            Cmd::Contacts { limit, json }
        }
        // ─── Clipboard ────────────────────────────────────────────────
        Some((&"clip", rest)) => {
            let mut watch = false;
            let mut interval_ms = 500_u64;
            let mut positional: Vec<&str> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--watch" => watch = true,
                    "--interval" => {
                        i += 1;
                        interval_ms = rest.get(i).ok_or("--interval needs MS")?
                            .parse().map_err(|_| "bad --interval".to_string())?;
                    }
                    other => positional.push(other),
                }
                i += 1;
            }
            let text = if positional.is_empty() { None } else { Some(positional.join(" ")) };
            if watch && text.is_some() {
                return Err("clip --watch is read-only; can't combine with TEXT".into());
            }
            Cmd::Clip { text, watch, interval_ms }
        }

        Some((&"notif", rest)) => {
            let mut limit = 50_u32;
            let mut json = false;
            let mut history = false;
            let mut pkg: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--limit" => {
                        i += 1;
                        limit = rest.get(i).ok_or("--limit needs N")?.parse()
                            .map_err(|_| "bad --limit".to_string())?;
                    }
                    "--json"    => json = true,
                    "--history" => history = true,
                    other if pkg.is_none() && !other.starts_with("--") => {
                        pkg = Some(other.to_string());
                    }
                    other => return Err(format!("unknown notif arg: {other}")),
                }
                i += 1;
            }
            Cmd::Notif { pkg, history, limit, json }
        }
        Some((&"calendar", rest)) => {
            let mut limit = 50_u32;
            let mut json = false;
            let mut from_ms: Option<i64> = None;
            let mut to_ms: Option<i64> = None;
            let mut days: Option<i64> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--limit" => {
                        i += 1;
                        limit = rest.get(i).ok_or("--limit needs N")?.parse()
                            .map_err(|_| "bad --limit".to_string())?;
                    }
                    "--json" => json = true,
                    "--days" => {
                        i += 1;
                        days = Some(rest.get(i).ok_or("--days needs N")?.parse()
                            .map_err(|_| "bad --days".to_string())?);
                    }
                    "--from" => {
                        i += 1;
                        from_ms = Some(rest.get(i).ok_or("--from needs MS")?.parse()
                            .map_err(|_| "bad --from".to_string())?);
                    }
                    "--to" => {
                        i += 1;
                        to_ms = Some(rest.get(i).ok_or("--to needs MS")?.parse()
                            .map_err(|_| "bad --to".to_string())?);
                    }
                    other => return Err(format!("unknown calendar arg: {other}")),
                }
                i += 1;
            }
            Cmd::Calendar { from_ms, to_ms, days, limit, json }
        }

        // ─── Shell + raw wire ────────────────────────────────────────
        // `shell` and `do` are synonyms: shell is the natural REPL verb,
        // do <wire> is a one-shot raw call.
        Some((&"shell", _)) => Cmd::Shell,
        Some((&"do", rest)) => {
            if rest.is_empty() { Cmd::Shell } else { Cmd::Input(rest.join(" ")) }
        }

        // ─── Low-level / debugging ───────────────────────────────────
        // `hs dev <sub>` is the documented namespace; the bare verbs
        // (`hs ping`, `hs snapshot`, …) stay as undocumented aliases so
        // older scripts and the test harness don't break.
        Some((&"dev", rest)) => parse_dev(rest)?,
        Some((&"ping", _))      => Cmd::Ping,
        Some((&"snapshot", _))  => Cmd::Snapshot,
        Some((&"screen", _))    => Cmd::Screen,
        Some((&"quit", _))      => Cmd::Quit,
        Some((&"state-daemon", _)) => Cmd::StateDaemon,
        Some((&"bench", rest)) => parse_bench(rest)?,

        Some((other, _)) => return Err(suggest(other)),
    };

    Ok(Opts { host, port, out_fmt, cmd })
}

/// `--device SERIAL` → host-side forwarded port for that device. Errors if
/// the device isn't attached or the daemon hasn't been brought up with
/// `hs use`. Keeps the global flag thin so it doesn't accidentally touch
/// the daemon lifecycle.
fn resolve_device_port(serial: &str) -> Result<u16, String> {
    let rows = daemon::devices().map_err(|e| format!("adb devices: {e}"))?;
    let row = rows.iter().find(|r| r.serial == serial)
        .ok_or_else(|| format!("--device {serial}: not attached"))?;
    row.host_port.ok_or_else(|| format!(
        "--device {serial}: no forwarded daemon — run `hs use {serial}` first"))
}

fn run(opts: &Opts) -> io::Result<()> {
    match &opts.cmd {
        Cmd::Bench { n } => bench(&opts.host, opts.port, *n),
        Cmd::TapText { query, flags } => {
            let reporter = output::Reporter::new(flags.out(opts.out_fmt));
            let mut sess = session::Session::connect(
                &opts.host, opts.port, flags.clone(), opts.out_fmt)?;
            // One-shot: zero TTL so the dump never lingers between runs.
            sess.dump_ttl_ms = 0;
            tap_text::run_session(&mut sess, query, &reporter, "tap")
        }
        Cmd::TapXY { x, y, flags } => {
            let reporter = output::Reporter::new(flags.out(opts.out_fmt));
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call(&format!("tap x={x} y={y}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("tap", e));
            }
            reporter.ok("tap", &String::from_utf8_lossy(&body),
                json_out::Obj::new().n("x", *x as i64).n("y", *y as i64))
        }
        Cmd::Snapshot => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            snapshot::run(&mut conn)
        }
        Cmd::Screen => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            screen::run(&mut conn)
        }
        Cmd::Adb(sub) => adb::run(&opts.host, opts.port, sub),
        Cmd::Connect(o) => {
            let port = daemon::connect(o)?;
            eprintln!("daemon up on tcp:{port}");
            Ok(())
        }
        Cmd::Disconnect(o) => daemon::disconnect(o),
        Cmd::Devices => print_devices(),
        Cmd::StateDaemon => state_cache::run_daemon(&opts.host, opts.port),
        Cmd::Shell => shell::run(&opts.host, opts.port),
        Cmd::Find { selector, flags } => {
            run_find(&opts.host, opts.port, opts.out_fmt, selector, flags)
        }
        Cmd::See(dest) => run_see(&opts.host, opts.port, dest.as_deref()),
        Cmd::Wait { spec, flags } => {
            run_wait(&opts.host, opts.port, opts.out_fmt, spec, flags)
        }
        Cmd::Run { script, flags } => {
            run_script::run(&opts.host, opts.port, opts.out_fmt, script.as_deref(), flags.clone())
        }
        Cmd::Act(a) => act::run(&opts.host, opts.port, opts.out_fmt, a),
        Cmd::Fan { serials, argv } => fan::run(opts.out_fmt, serials, argv),
        Cmd::Init { path } => init::run(path.as_deref()),
        Cmd::Cp { src, dst } => run_cp(&opts.host, opts.port, src, dst),
        Cmd::ShowPkg(pkg) => run_show_pkg(&opts.host, opts.port, pkg),
        Cmd::Info => run_info(&opts.host, opts.port),
        Cmd::Ui { format, all } => run_ui(&opts.host, opts.port, *format, *all),
        Cmd::SettingsListAll => {
            // Three Shell-passthrough calls glued client-side with header
            // lines — keeps the daemon's shell command simple (no quote
            // parsing needed) and gives a single readable dump.
            let mut out = io::stdout().lock();
            for ns in ["system", "secure", "global"] {
                writeln!(out, "=== {ns} ===")?;
                out.flush()?;
                adb::run(&opts.host, opts.port, &adb::Cmd::Shell(vec![
                    "/system/bin/settings".into(), "list".into(), ns.into(),
                ]))?;
            }
            Ok(())
        }
        Cmd::Links(pkg) => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call(&format!("deeplinks {pkg}"))?;
            if body.starts_with(b"ERR:") {
                return Err(io::Error::other(String::from_utf8_lossy(&body).into_owned()));
            }
            let mut out = io::stdout().lock();
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
            out.flush()
        }
        Cmd::Sms { kind, limit, json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("sms type={kind} limit={limit}");
            provider::run(&mut conn, &wire, *json,
                &[provider::TypeMap { column: "type", map: SMS_TYPES }],
                &[])
        }
        Cmd::Calls { kind, limit, json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("calls type={kind} limit={limit}");
            provider::run(&mut conn, &wire, *json,
                &[provider::TypeMap { column: "type", map: CALL_TYPES }],
                &[])
        }
        Cmd::Contacts { limit, json } => {
            // Daemon emits raw Phone columns (data1/data2/data3) since
            // ContactsContract rejects SQL AS aliases. Rename them to
            // friendly labels for display + JSON.
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("contacts limit={limit}");
            provider::run(&mut conn, &wire, *json,
                &[provider::TypeMap { column: "type", map: PHONE_TYPES }],
                &[("data1", "number"), ("data2", "type"), ("data3", "label")])
        }
        Cmd::Clip { text, watch, interval_ms } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            if *watch {
                let wire = format!("clip_watch interval_ms={interval_ms}");
                conn.send_cmd(&wire)?;
                let mut out = io::stdout().lock();
                loop {
                    let frame = conn.read_frame()?;
                    if frame.is_empty() { break; }
                    if frame.starts_with(b"ERR:") {
                        return Err(io::Error::other(
                            String::from_utf8_lossy(&frame).into_owned()));
                    }
                    out.write_all(&frame)?;
                    out.write_all(b"\n")?;
                    out.flush()?;
                }
                Ok(())
            } else if let Some(t) = text {
                let body = conn.call(&format!("clip_set {t}"))?;
                if body.starts_with(b"ERR:") {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&body).into_owned()));
                }
                Ok(())
            } else {
                let body = conn.call("clip_get")?;
                if body.starts_with(b"ERR:") {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&body).into_owned()));
                }
                let mut out = io::stdout().lock();
                out.write_all(&body)?;
                if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
                out.flush()
            }
        }
        Cmd::Notif { pkg, history, limit, json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let mut wire = format!("notifications limit={limit}");
            if *history { wire.push_str(" history=1"); }
            if let Some(p) = pkg { wire.push_str(&format!(" pkg={p}")); }
            provider::run(&mut conn, &wire, *json, &[], &[])
        }
        Cmd::Calendar { from_ms, to_ms, days, limit, json } => {
            // Resolve window. --days N takes precedence over --from/--to if
            // both are given; default = now → now + 7 days.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let (from, to) = if let Some(d) = days {
                (now, now + d * 24 * 60 * 60 * 1000)
            } else {
                (from_ms.unwrap_or(now),
                 to_ms.unwrap_or(now + 7 * 24 * 60 * 60 * 1000))
            };
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("calendar from={from} to={to} limit={limit}");
            provider::run(&mut conn, &wire, *json, &[], &[])
        }
        Cmd::Submit { sel, flags } => run_submit(opts, sel.as_deref(), flags),
        Cmd::Paste  { sel, flags } => run_paste (opts, sel.as_deref(), flags),
        Cmd::TypeFocused { text, flags } => run_type_focused(opts, text, flags),
        Cmd::TypeInto { selector, text, flags } => run_type_into(opts, selector, text, flags),
        Cmd::Ping => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call("ping")?;
            write_response(&opts.cmd, &body)
        }
        Cmd::Quit => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call("quit")?;
            write_response(&opts.cmd, &body)
        }
        Cmd::Input(wire) => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call(wire)?;
            write_response(&opts.cmd, &body)
        }
    }
}

fn run_submit(opts: &Opts, sel: Option<&str>, flags: &flags::ActionFlags) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    let mut attempts = flags.total_attempts();
    loop {
        let wire = match sel {
            Some(s) => format!("submit {s}"),
            None => "submit".into(),
        };
        let body = conn.call(&wire)?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 {
                std::thread::sleep(std::time::Duration::from_millis(flags.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("submit", e));
        }
        return reporter.ok("submit", &String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new().opt_s("selector", sel));
    }
}

fn run_paste(opts: &Opts, sel: Option<&str>, flags: &flags::ActionFlags) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    let mut attempts = flags.total_attempts();
    loop {
        let wire = match sel {
            Some(s) => format!("paste {s}"),
            None => "paste".into(),
        };
        let body = conn.call(&wire)?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 {
                std::thread::sleep(std::time::Duration::from_millis(flags.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("paste", e));
        }
        return reporter.ok("paste", &String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new().opt_s("selector", sel));
    }
}

fn run_type_focused(opts: &Opts, text: &str, flags: &flags::ActionFlags) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    let body = conn.call(&format!("text {text}"))?;
    if let Some(e) = errors::parse_err(&body) {
        return Err(reporter.fail("type", e));
    }
    reporter.ok("type", &String::from_utf8_lossy(&body).trim_end(),
        json_out::Obj::new().s("text", text))
}

/// `hs fill` accepts both grammars. Any selector containing `=` is treated
/// as daemon-grammar (`id=…`, `class=EditText text~=…`) and passed through
/// unchanged. Bare strings are free-text queries: dump the active window,
/// find the best-matching input widget, then rebuild a daemon selector
/// (preferring `id=<full-rid>`, falling back to class + exact text) so the
/// daemon's narrower parser can still resolve it.
fn resolve_fill_selector(conn: &mut Conn, sel_in: &str) -> io::Result<String> {
    if sel_in.contains('=') {
        return Ok(sel_in.to_string());
    }
    let body = conn.call("dump_active")?;
    if errors::parse_err(&body).is_some() {
        // Couldn't read the tree — fall through and let the daemon error.
        return Ok(sel_in.to_string());
    }
    let text = std::str::from_utf8(&body)
        .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    let dump = json::parse(text)
        .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
    let root = json::obj_get(&dump, "root").unwrap_or(&dump);

    match find_fill_target(root, sel_in) {
        Some(node) => Ok(node_to_daemon_selector(node)),
        None => Err(io::Error::other(format!(
            "fill: no input widget matched {sel_in:?} — pass an explicit \
             selector (`id=...` or `class=EditText text~=...`)"
        ))),
    }
}

/// Build a daemon-grammar selector that uniquely names `node`. Prefers the
/// full resource id when present (always unique); falls back to class + the
/// node's own text when the widget is anonymous.
fn node_to_daemon_selector(node: &json::Value) -> String {
    let rid = selector::get_str(node, "rid").unwrap_or("");
    if !rid.is_empty() {
        return format!("id={rid}");
    }
    let cls_full = selector::get_str(node, "cls").unwrap_or("EditText");
    let simple = cls_full.rsplit('.').next().unwrap_or(cls_full);
    let txt = selector::get_str(node, "text").unwrap_or("");
    if !txt.is_empty() {
        return format!("class={simple} text={txt:?}");
    }
    format!("class={simple}")
}

fn is_input_widget(node: &json::Value) -> bool {
    let cls = selector::get_str(node, "cls").unwrap_or("");
    let simple = cls.rsplit('.').next().unwrap_or(cls);
    matches!(simple,
        "EditText" | "AutoCompleteTextView" | "MultiAutoCompleteTextView")
}

/// Score how well an input widget matches the free-text query. Mirrors the
/// priority ladder in `tap_text::classify` so `hs fill` and `hs tap` rank
/// candidates the same way.
fn fill_priority(q: &str, q_low: &str, text: &str, desc: &str, hint: &str) -> Option<u8> {
    if text == q || desc == q || hint == q { return Some(0); }
    let t = text.to_lowercase();
    let d = desc.to_lowercase();
    let h = hint.to_lowercase();
    if t == q_low || d == q_low || h == q_low { return Some(1); }
    if !q_low.is_empty() && (t.contains(q_low) || d.contains(q_low) || h.contains(q_low)) {
        return Some(2);
    }
    None
}

fn find_fill_target<'a>(root: &'a json::Value, query: &str) -> Option<&'a json::Value> {
    let q_low = query.to_lowercase();

    // Phase 1: prefer an input widget whose own text/desc/hint matches.
    // Empty EditTexts surface their hint as `text` in the dump, so this
    // catches the common "fill the field whose placeholder mentions X" case.
    let mut best: Option<(u8, &json::Value)> = None;
    find_input_match(root, query, &q_low, &mut best);
    if let Some((_, n)) = best { return Some(n); }

    // Phase 2: label-anchored — find any node matching the query, then
    // take the nearest input widget below or right-of it. Mirrors how a
    // human reads a form: label on the left or above, field next to it.
    let mut labels: Vec<&json::Value> = Vec::new();
    collect_labels(root, &q_low, &mut labels);
    for label in labels {
        let Some(lb) = selector::bounds(label) else { continue; };
        let mut nearest: Option<(i64, &json::Value)> = None;
        find_nearest_input(root, lb, &mut nearest);
        if let Some((_, n)) = nearest { return Some(n); }
    }
    None
}

fn find_input_match<'a>(
    node: &'a json::Value,
    q: &str,
    q_low: &str,
    best: &mut Option<(u8, &'a json::Value)>,
) {
    if is_input_widget(node) {
        let text = selector::get_str(node, "text").unwrap_or("");
        let desc = selector::get_str(node, "desc").unwrap_or("");
        let hint = selector::get_str(node, "hint").unwrap_or("");
        if let Some(p) = fill_priority(q, q_low, text, desc, hint) {
            let beat = match *best { None => true, Some((bp, _)) => p < bp };
            if beat { *best = Some((p, node)); }
        }
    }
    if let Some(kids) = selector::children(node) {
        for c in kids { find_input_match(c, q, q_low, best); }
    }
}

fn collect_labels<'a>(node: &'a json::Value, q_low: &str, out: &mut Vec<&'a json::Value>) {
    let text = selector::get_str(node, "text").unwrap_or("").to_lowercase();
    let desc = selector::get_str(node, "desc").unwrap_or("").to_lowercase();
    if !q_low.is_empty()
        && ((!text.is_empty() && text.contains(q_low))
            || (!desc.is_empty() && desc.contains(q_low)))
    {
        out.push(node);
    }
    if let Some(kids) = selector::children(node) {
        for c in kids { collect_labels(c, q_low, out); }
    }
}

fn find_nearest_input<'a>(
    node: &'a json::Value,
    lb: (i64, i64, i64, i64),
    best: &mut Option<(i64, &'a json::Value)>,
) {
    if is_input_widget(node) {
        if let Some(eb) = selector::bounds(node) {
            // 8px slack so a label whose bottom exactly meets the field
            // top still counts as "above" rather than "overlapping".
            let below = eb.1 >= lb.3 - 8;
            let right = eb.0 >= lb.2 - 8;
            if below || right {
                let lcx = (lb.0 + lb.2) / 2;
                let lcy = (lb.1 + lb.3) / 2;
                let ecx = (eb.0 + eb.2) / 2;
                let ecy = (eb.1 + eb.3) / 2;
                let dx = ecx - lcx;
                let dy = ecy - lcy;
                let dist = dx * dx + dy * dy;
                let beat = match *best { None => true, Some((bd, _)) => dist < bd };
                if beat { *best = Some((dist, node)); }
            }
        }
    }
    if let Some(kids) = selector::children(node) {
        for c in kids { find_nearest_input(c, lb, best); }
    }
}

fn run_type_into(
    opts: &Opts, selector: &str, text: &str, flags: &flags::ActionFlags,
) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    // Selectors with no `=` are free-text queries: look up the best
    // matching EditText in the live dump and rebuild a daemon-grammar
    // selector for it. Mirrors `hs tap "TEXT"`.
    let resolved = resolve_fill_selector(&mut conn, selector)?;
    let mut attempts = flags.total_attempts();
    loop {
        // Daemon wire command: node_set_text <selector> value="..."
        let body = conn.call(&format!("node_set_text {resolved} value={text:?}"))?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 {
                std::thread::sleep(std::time::Duration::from_millis(flags.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("type", e));
        }
        return reporter.ok("type", &String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new()
                .s("selector", &resolved)
                .s("query", selector)
                .s("text", text));
    }
}

/// `hs ui [--json|--xml] [--all]` — UI tree dump in the chosen format.
/// Default is a human-readable indented outline; `--all` covers every
/// window (otherwise just the active one).
fn run_ui(host: &str, port: u16, format: UiFormat, all: bool) -> io::Result<()> {
    let wire = if all { "dump" } else { "dump_active" };
    let mut conn = Conn::connect(host, port)?;
    let body = conn.call(wire)?;
    let mut out = io::stdout().lock();
    match format {
        UiFormat::Json => {
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
        }
        UiFormat::Xml => {
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            out.write_all(xml_dump::render(&json, 0).as_bytes())?;
        }
        UiFormat::Human => {
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            out.write_all(ui_dump::render_human(&json).as_bytes())?;
        }
        UiFormat::Interactive => {
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            out.write_all(ui_dump::render_interactive(&json).as_bytes())?;
        }
    }
    out.flush()
}

/// `hs see [PATH]` — bare opens the viewer; otherwise extension picks
/// the format (jpg/png screenshot, xml/json UI hierarchy).
fn run_see(host: &str, port: u16, dest: Option<&str>) -> io::Result<()> {
    let Some(path) = dest else {
        return mirror::run(host, port, mirror::Args::default());
    };
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mut conn = Conn::connect(host, port)?;
    let bytes: Vec<u8> = match ext.as_str() {
        "jpg" | "jpeg" => conn.call("screenshot max=1")?,
        "png"          => conn.call("screenshot max=1 fmt=png")?,
        "json"         => conn.call("dump_active")?,
        "xml" => {
            let body = conn.call("dump_active")?;
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            xml_dump::render(&json, 0).into_bytes()
        }
        other => return Err(io::Error::other(
            format!("see: unsupported extension '.{other}' (jpg|png|xml|json)"))),
    };
    if bytes.starts_with(b"ERR:secure-window:") {
        let win = String::from_utf8_lossy(&bytes[b"ERR:secure-window:".len()..]);
        return Err(io::Error::other(format!(
            "screenshot blocked: {win} has FLAG_SECURE set — the system denies \
             screen capture of this window (common for banking, password, and \
             incognito screens). Not saving to {path}."
        )));
    }
    if bytes.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&bytes).into_owned()));
    }
    std::fs::write(path, &bytes)?;
    eprintln!("saved {} bytes to {path}", bytes.len());
    Ok(())
}

/// `hs wait <spec>` — smart-dispatch on `spec` shape. `flags` lets RPA
/// scripts override the daemon-side default (10 s) so long-running things
/// like cold app launches don't silently hit the timeout.
fn run_wait(
    host: &str,
    port: u16,
    out_fmt: flags::OutFmt,
    spec: &str,
    f: &flags::ActionFlags,
) -> io::Result<()> {
    let reporter = output::Reporter::new(f.out(out_fmt));
    let spec = spec.trim();
    // "Nms" / "Ns" — sleep client-side, no daemon hop needed.
    if let Some(ms) = parse_duration_ms(spec) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        return reporter.ok("wait", "ok",
            json_out::Obj::new().n("slept_ms", ms as i64));
    }
    let mut conn = Conn::connect(host, port)?;
    let timeout_ms = f.timeout_ms.unwrap_or(10_000);
    let wire: String = if let Some(rest) = spec.strip_prefix("idle") {
        let rest = rest.trim();
        let idle_ms = if rest.is_empty() {
            200
        } else if let Some(ms) = parse_duration_ms(rest) {
            ms
        } else {
            return Err(io::Error::other(format!("wait idle: bad duration '{rest}'")));
        };
        format!("wait_for_idle idle_ms={idle_ms} timeout_ms={timeout_ms}")
    } else if is_component(spec) || is_pkg(spec) {
        format!("wait_for_activity n={spec} timeout_ms={timeout_ms}")
    } else {
        format!("wait_for_text text={spec:?} timeout_ms={timeout_ms}")
    };
    // Retry layer: timeout exhaustion gets retried `retries` additional
    // times. For most waits this is a no-op (retries=0), but it lets RPA
    // scripts chain `--timeout 2s --retries 5` for cheap polling.
    let mut attempts = f.total_attempts();
    loop {
        let body = conn.call(&wire)?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 && e.code == errors::ErrCode::Timeout {
                std::thread::sleep(std::time::Duration::from_millis(f.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("wait", e));
        }
        return reporter.ok("wait", &String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new().s("spec", spec).n("timeout_ms", timeout_ms as i64));
    }
}

/// `hs find SELECTOR` — match a CSS-like selector against the current
/// dump_active tree and print one node summary per match. Honours
/// --visible/--clickable/--enabled/--unique/--nth.
fn run_find(
    host: &str,
    port: u16,
    out_fmt: flags::OutFmt,
    sel: &str,
    f: &flags::ActionFlags,
) -> io::Result<()> {
    let reporter = output::Reporter::new(f.out(out_fmt));
    let selectors = match selector::Selector::parse(sel) {
        Ok(s) => s,
        Err(e) => return Err(reporter.fail("find",
            errors::ErrInfo::new(errors::ErrCode::BadArg, e))),
    };
    let mut conn = Conn::connect(host, port)?;
    let body = conn.call("dump_active")?;
    if let Some(e) = errors::parse_err(&body) {
        return Err(reporter.fail("find", e));
    }
    let text = std::str::from_utf8(&body)
        .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    let json = json::parse(text)
        .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
    let ctx = selector::MatchCtx::new(&json);
    let mut matches = selector::find_all_with(&ctx, &selectors);
    selector::apply_filters(&mut matches, f);

    if matches.is_empty() {
        return Err(reporter.fail("find",
            errors::ErrInfo::new(errors::ErrCode::NotFound,
                format!("no node matched '{sel}'"))));
    }
    if f.require_unique && matches.len() > 1 {
        return Err(reporter.fail("find",
            errors::ErrInfo::new(errors::ErrCode::Ambiguous,
                format!("--unique: '{sel}' matched {} nodes", matches.len()))));
    }
    if let Some(n) = f.nth {
        if n == 0 || n > matches.len() {
            return Err(reporter.fail("find",
                errors::ErrInfo::new(errors::ErrCode::NotFound,
                    format!("--nth {n} out of range (have {})", matches.len()))));
        }
        let picked = matches[n - 1];
        matches.clear();
        matches.push(picked);
    }

    // Output: either one human row per match, or one JSON line per match
    // (so machine consumers can read with --json | jq -c).
    match f.out(out_fmt) {
        flags::OutFmt::Human => {
            let mut out = io::stdout().lock();
            for n in &matches {
                write_find_row(&mut out, n)?;
            }
            out.flush()?;
        }
        flags::OutFmt::Json => {
            let mut out = io::stdout().lock();
            for n in &matches {
                let row = find_row_obj(n).finish();
                let line = json_out::Obj::new()
                    .s("verb", "find")
                    .b("ok", true)
                    .raw("result", &row)
                    .finish();
                writeln!(out, "{line}")?;
            }
            out.flush()?;
        }
    }
    Ok(())
}

fn write_find_row(out: &mut io::StdoutLock<'_>, n: &json::Value) -> io::Result<()> {
    let cls = selector::get_str(n, "cls").unwrap_or("");
    let id  = selector::get_str(n, "rid").unwrap_or("");
    let t   = selector::get_str(n, "text").unwrap_or("");
    let d   = selector::get_str(n, "desc").unwrap_or("");
    let bs  = match selector::bounds(n) {
        Some((x1, y1, x2, y2)) => {
            let cx = (x1 + x2) / 2;
            let cy = (y1 + y2) / 2;
            format!("[{x1},{y1}][{x2},{y2}] center=({cx},{cy})")
        }
        None => "[]".into(),
    };
    writeln!(out, "{bs}\tclass={cls}\tid={id}\ttext={t:?}\tdesc={d:?}")
}

fn find_row_obj(n: &json::Value) -> json_out::Obj {
    let mut o = json_out::Obj::new()
        .s("class", selector::get_str(n, "cls").unwrap_or(""))
        .s("id",    selector::get_str(n, "rid").unwrap_or(""))
        .s("text",  selector::get_str(n, "text").unwrap_or(""))
        .s("desc",  selector::get_str(n, "desc").unwrap_or(""))
        .s("flags", selector::get_str(n, "flags").unwrap_or(""));
    if let Some((x1, y1, x2, y2)) = selector::bounds(n) {
        let cx = (x1 + x2) / 2;
        let cy = (y1 + y2) / 2;
        o = o.n("x1", x1).n("y1", y1).n("x2", x2).n("y2", y2)
             .n("cx", cx).n("cy", cy);
    }
    o
}

/// Dispatch a verb line parsed inside `hs run`. Re-uses the one-shot verb
/// parser, then routes the cmd through a session-aware handler when one
/// exists (TapText, Find, Wait, …) or the standard one-shot path otherwise.
pub(crate) fn dispatch_session_verb(
    sess: &mut session::Session,
    argv: &[String],
) -> io::Result<()> {
    let opts = match parse_args(argv) {
        Ok(o) => o,
        Err(e) => return Err(io::Error::other(e)),
    };
    // Pull defaults from the session unless the verb line set its own.
    match opts.cmd {
        Cmd::TapText { query, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            sess.defaults = flags.clone();
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            tap_text::run_session(sess, &query, &reporter, "tap")
        }
        Cmd::TapXY { x, y, flags } => {
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let body = sess.conn.call(&format!("tap x={x} y={y}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("tap", e));
            }
            reporter.ok("tap", &String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().n("x", x as i64).n("y", y as i64))
        }
        Cmd::Wait { spec, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            run_wait(&sess.peer_host(), sess.peer_port(),
                sess.default_out, &spec, &flags)
        }
        Cmd::Find { selector: sel, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            run_find(&sess.peer_host(), sess.peer_port(),
                sess.default_out, &sel, &flags)
        }
        Cmd::Submit { sel, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let wire = match &sel {
                Some(s) => format!("submit {s}"),
                None => "submit".into(),
            };
            let body = sess.conn.call(&wire)?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("submit", e));
            }
            reporter.ok("submit", &String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().opt_s("selector", sel.as_deref()))
        }
        Cmd::Paste { sel, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let wire = match &sel {
                Some(s) => format!("paste {s}"),
                None => "paste".into(),
            };
            let body = sess.conn.call(&wire)?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("paste", e));
            }
            reporter.ok("paste", &String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().opt_s("selector", sel.as_deref()))
        }
        Cmd::TypeFocused { text, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let body = sess.conn.call(&format!("text {text}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("type", e));
            }
            reporter.ok("type", &String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().s("text", &text))
        }
        Cmd::TypeInto { selector, text, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let resolved = resolve_fill_selector(&mut sess.conn, &selector)?;
            let body = sess.conn.call(&format!("node_set_text {resolved} value={text:?}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("type", e));
            }
            reporter.ok("type", &String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new()
                    .s("selector", &resolved)
                    .s("query", &selector)
                    .s("text", &text))
        }
        Cmd::Input(wire) => {
            let body = sess.conn.call(&wire)?;
            if let Some(e) = errors::parse_err(&body) {
                let reporter = output::Reporter::new(sess.default_out);
                return Err(reporter.fail("do", e));
            }
            let mut out = io::stdout().lock();
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
            out.flush()
        }
        other => {
            // For verbs that aren't session-aware yet, build a fresh Opts
            // and reuse the one-shot dispatch. We deliberately lose the
            // warm-socket benefit here — but it's still strictly better
            // than the user spawning a child `hs` process per line.
            let synthetic = Opts {
                host: sess.peer_host(),
                port: sess.peer_port(),
                out_fmt: sess.default_out,
                cmd: other,
            };
            run(&synthetic)
        }
    }
}

/// Apply session-level defaults to a verb-line flags struct: only fields
/// the verb didn't set itself get inherited. Lets `set timeout=8s` at the
/// top of a script flow into every subsequent `tap`/`wait`/etc.
fn merge_session_defaults(f: &mut flags::ActionFlags, sess: &session::Session) {
    if f.timeout_ms.is_none() { f.timeout_ms = sess.defaults.timeout_ms; }
    if f.retries == 0           { f.retries = sess.defaults.retries; }
    if f.retry_delay_ms == 200  { f.retry_delay_ms = sess.defaults.retry_delay_ms; }
    if !f.require_visible       { f.require_visible   = sess.defaults.require_visible; }
    if !f.require_clickable     { f.require_clickable = sess.defaults.require_clickable; }
    if !f.require_enabled       { f.require_enabled   = sess.defaults.require_enabled; }
    if f.out_fmt.is_none()      { f.out_fmt = sess.defaults.out_fmt; }
}

/// Parse "Nms", "Ns" (where N is unsigned int).
fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        n.trim().parse::<u64>().ok()
    } else if let Some(n) = s.strip_suffix('s') {
        n.trim().parse::<u64>().ok().map(|n| n * 1000)
    } else {
        None
    }
}

fn is_component(s: &str) -> bool {
    s.contains('/') && s.chars().all(|c|
        c.is_ascii_alphanumeric() || c == '.' || c == '/' || c == '_' || c == '$')
}

/// `hs cp` — direction from the `device:` prefix.
fn run_cp(host: &str, port: u16, src: &str, dst: &str) -> io::Result<()> {
    let to_dev = dst.starts_with("device:");
    let from_dev = src.starts_with("device:");
    if to_dev && from_dev {
        return Err(io::Error::other("cp: both sides device — not supported"));
    }
    if !to_dev && !from_dev {
        return Err(io::Error::other("cp: one side must start with 'device:'"));
    }
    let cmd = if from_dev {
        let device = src.strip_prefix("device:").unwrap().to_string();
        adb::Cmd::Pull { device, host: Some(dst.to_string()) }
    } else {
        let device = dst.strip_prefix("device:").unwrap().to_string();
        adb::Cmd::Push { host: src.to_string(), device, mode: None }
    };
    adb::run(host, port, &cmd)
}

fn format_uptime(s: u64) -> String {
    if s == 0 { return String::new(); }
    let d = s / 86400;
    let h = (s % 86400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 { format!("{d}d {h}h {m}m") }
    else if h > 0 { format!("{h}h {m}m") }
    else { format!("{m}m") }
}

fn format_bytes_pair(used_kb: u64, total_kb: u64) -> String {
    if total_kb == 0 { return String::new(); }
    let to_gib = |kb: u64| (kb as f64) / 1024.0 / 1024.0;
    format!("{:.1} / {:.1} GiB", to_gib(used_kb), to_gib(total_kb))
}

/// `hs info` — neofetch-style snapshot. Reads the local push-mirrored
/// cache file (the state-daemon spawned by `hs use` keeps it fresh via
/// event hooks + 5-s heartbeat). No daemon round-trips on the hot path —
/// total cost is one file read + JSON parse + format.
fn run_info(host: &str, port: u16) -> io::Result<()> {
    // 1. Fast path — cache file written by the host-side state-daemon.
    let snap_bytes = match state_cache::read_cached(port) {
        Some(b) => b,
        None => {
            // No daemon yet (or stale). Fall back to a single wire call.
            let mut conn = Conn::connect(host, port)?;
            conn.call("state device")?
        }
    };
    let state = json::parse(std::str::from_utf8(&snap_bytes).unwrap_or("{}"))
        .unwrap_or(json::Value::Null);
    let get_str = |k: &str| -> String {
        if let json::Value::Obj(fields) = &state {
            for (kk, v) in fields {
                if kk == k {
                    return match v {
                        json::Value::Str(s) => s.clone(),
                        json::Value::Num(n) => n.to_string(),
                        json::Value::Bool(b) => b.to_string(),
                        _ => String::new(),
                    };
                }
            }
        }
        String::new()
    };
    // Pulled from the snapshot — static (cached once on the daemon side)
    // and dynamic (refreshed on every snapshot refresh).
    let model       = get_str("model");
    let mfg         = get_str("manufacturer");
    let device      = get_str("device");
    let release     = get_str("release");
    let codename    = get_str("codename");
    let build_type  = get_str("build_type");
    let fingerprint = get_str("fingerprint");
    let abi         = get_str("abi");
    let sdk         = get_str("sdk");
    let width       = get_str("width");
    let height      = get_str("height");
    let rotation    = get_str("rotation");
    let battery     = get_str("battery_level");
    let charging    = get_str("battery_charging");
    let inter       = get_str("interactive");
    let top         = get_str("top_activity");
    let temp_c      = get_str("battery_temp_c");
    let uptime_s    = get_str("uptime_s");
    let mem_total_k = get_str("total_ram_kb");
    let mem_avail_k = get_str("mem_available_kb");
    let stor_total_k = get_str("total_storage_kb");
    let stor_free_k = get_str("storage_free_kb");
    let cpu_cores   = get_str("cpu_cores");
    let cpu_model   = get_str("cpu_model");
    let kernel      = get_str("kernel");
    let locale      = get_str("locale");
    let timezone    = get_str("timezone");
    let theme       = get_str("theme");
    let net         = get_str("network");
    let ip          = get_str("ip");
    let ssid        = get_str("wifi_ssid");
    let app_count   = get_str("app_count");
    let app_3rd     = get_str("app_count_3rd");
    let _ = (host, fingerprint.clone());   // hush unused warnings

    // Compose the right-hand-side rows.
    let title  = if !model.is_empty() && !mfg.is_empty() {
        format!("{mfg} {model}")
    } else if !model.is_empty() { model.clone() } else { device.clone() };
    let title_under = "─".repeat(title.chars().count().max(1));

    let mut rows: Vec<(String, String)> = Vec::new();
    let push = |rows: &mut Vec<(String, String)>, k: &str, v: String| {
        if !v.is_empty() { rows.push((k.into(), v)); }
    };
    push(&mut rows, "OS",      if release.is_empty() {
            String::new()
        } else {
            format!("Android {release}{}{}{}",
                if codename.is_empty() || codename == "REL" { String::new() }
                    else { format!(" ({codename})") },
                if sdk.is_empty() { String::new() } else { format!(" — SDK {sdk}") },
                if build_type.is_empty() || build_type == "user" { String::new() }
                    else { format!(" ({build_type})") })
        });
    push(&mut rows, "Kernel",  kernel.clone());
    push(&mut rows, "Uptime",  format_uptime(uptime_s.parse().unwrap_or(0)));
    push(&mut rows, "Display", if width.is_empty() { String::new() }
        else { format!("{width} × {height}   (rotation {rotation})") });
    push(&mut rows, "CPU",     if cpu_cores.is_empty() { abi.clone() }
        else if cpu_model.is_empty() { format!("{cpu_cores}× {abi}") }
        else { format!("{cpu_cores}× {abi}  ({cpu_model})") });
    let mem_t: u64 = mem_total_k.parse().unwrap_or(0);
    let mem_a: u64 = mem_avail_k.parse().unwrap_or(0);
    push(&mut rows, "Memory", format_bytes_pair(mem_t.saturating_sub(mem_a), mem_t));
    let stor_t: u64 = stor_total_k.parse().unwrap_or(0);
    let stor_f: u64 = stor_free_k.parse().unwrap_or(0);
    push(&mut rows, "Storage", format_bytes_pair(stor_t.saturating_sub(stor_f), stor_t));
    push(&mut rows, "Battery", if battery.is_empty() { String::new() }
        else {
            let onoff = if inter == "true" { "screen on" } else { "screen off" };
            let chg   = if charging == "true" { ", charging" } else { "" };
            let temp  = if temp_c.is_empty() || temp_c == "null" { String::new() }
                        else { format!(", {temp_c} °C") };
            format!("{battery}%  ({onoff}{chg}{temp})")
        });
    push(&mut rows, "Network", if net.is_empty() { String::new() } else {
        let mut s = net.clone();
        if !ssid.is_empty() { s.push_str(" · "); s.push_str(&ssid); }
        if !ip.is_empty()   { s.push_str(" · "); s.push_str(&ip); }
        s
    });
    push(&mut rows, "Theme",   theme.clone());
    push(&mut rows, "Locale",  if locale.is_empty() { String::new() }
        else if timezone.is_empty() { locale.clone() }
        else { format!("{locale}  ·  {timezone}") });
    push(&mut rows, "Apps",    if app_count.is_empty() { String::new() }
        else if app_3rd.is_empty() || app_3rd == "0" { format!("{app_count} packages") }
        else { format!("{app_count} packages  ({app_3rd} third-party)") });
    push(&mut rows, "Top",     top);
    push(&mut rows, "Daemon",  format!("hsd on tcp:{port}"));

    // ASCII android bugdroid (7 rows, ~13 wide).
    let logo: [&str; 12] = [
        "                ",
        "    \\       /   ",
        "     \\ ___ /    ",
        "      /   \\     ",
        "     | o o |    ",
        "     |  _  |    ",
        "      \\___/     ",
        "     |     |    ",
        "     |     |    ",
        "     |_____|    ",
        "      |   |     ",
        "                ",
    ];

    // Header rows: title and underline.
    let mut header = vec![
        ("".to_string(), title),
        ("".to_string(), title_under),
    ];
    header.extend(rows);
    let rows = header;

    let mut out = io::stdout().lock();
    let n = rows.len().max(logo.len());
    for i in 0..n {
        let logo_line = logo.get(i).copied().unwrap_or("                ");
        let (key, val) = rows.get(i).cloned().unwrap_or_default();
        if key.is_empty() {
            writeln!(out, "{logo_line}{val}")?;
        } else {
            writeln!(out, "{logo_line}{key:<8}  {val}")?;
        }
    }
    out.flush()
}

/// `hs show <pkg>` — pm_path + the daemon's dumpsys package summary.
fn run_show_pkg(host: &str, port: u16, pkg: &str) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    let path = conn.call(&format!("pm_path {pkg}"))?;
    let path_s = std::str::from_utf8(&path).unwrap_or("").trim();
    let mut out = io::stdout().lock();
    if path_s.starts_with("ERR:") {
        return Err(io::Error::other(path_s.to_string()));
    }
    writeln!(out, "package: {pkg}")?;
    writeln!(out, "  path:  {path_s}")?;
    // Try a short dumpsys package — the first few lines usually cover
    // versionName / versionCode / firstInstallTime.
    let info = conn.call(&format!("dumpsys package {pkg}"));
    if let Ok(_) = info {
        // dumpsys returns chunked frames; for now skip detailed parsing.
        // The path alone is the most-asked-for field.
    }
    out.flush()
}

fn write_response(cmd: &Cmd, body: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(body)?;
    // Trailing newline only for text commands on a terminal — never for binary.
    let is_text = matches!(
        cmd,
        Cmd::Ping | Cmd::Quit | Cmd::Input(_) | Cmd::Bench { .. } | Cmd::Find { .. }
    );
    if is_text && !body.is_empty() && body[body.len() - 1] != b'\n' {
        out.write_all(b"\n")?;
    }
    out.flush()
}

// ---------- protocol ----------

pub(crate) struct Conn {
    sock: TcpStream,
}

impl Conn {
    pub(crate) fn connect(host: &str, port: u16) -> io::Result<Self> {
        let sock = match TcpStream::connect((host, port)) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused
                    && is_local(host) => {
                // No daemon listening on the default port. If exactly
                // one device is attached, transparently auto-`hs use`
                // it so users don't have to do that step manually.
                match try_auto_use(port) {
                    Ok(()) => TcpStream::connect((host, port))?,
                    Err(why) => return Err(io::Error::new(e.kind(),
                        format!("no daemon on {host}:{port}; {why}"))),
                }
            }
            Err(e) => return Err(e),
        };
        sock.set_nodelay(true)?;
        sock.set_read_timeout(Some(Duration::from_secs(30)))?;
        sock.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self { sock })
    }

    pub(crate) fn call(&mut self, cmd: &str) -> io::Result<Vec<u8>> {
        self.send_cmd(cmd)?;
        self.read_frame()
    }

    /// Length-prefixed command write; doesn't read a reply.
    pub(crate) fn send_cmd(&mut self, cmd: &str) -> io::Result<()> {
        let payload = cmd.as_bytes();
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cmd too long"))?;
        self.sock.write_all(&len.to_be_bytes())?;
        self.sock.write_all(payload)?;
        Ok(())
    }

    /// Read one length-prefixed response frame.
    pub(crate) fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut hdr = [0u8; 4];
        self.sock.read_exact(&mut hdr)?;
        let n = u32::from_be_bytes(hdr) as usize;
        if n > 256 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("oversized response: {n} bytes"),
            ));
        }
        let mut buf = vec![0u8; n];
        self.sock.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Drain a server-streamed chunked response into `out`. Reads
    /// `[len][chunk]…` frames until `len == 0`. A leading `ERR:` payload is
    /// surfaced as an io::Error.
    pub(crate) fn recv_chunks_to(&mut self, out: &mut dyn Write) -> io::Result<u64> {
        let mut total = 0u64;
        let mut first = true;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let mut hdr = [0u8; 4];
            self.sock.read_exact(&mut hdr)?;
            let n = u32::from_be_bytes(hdr) as usize;
            if n == 0 {
                return Ok(total);
            }
            if n > 8 * 1024 * 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("oversized chunk: {n} bytes"),
                ));
            }
            if buf.len() < n { buf.resize(n, 0); }
            self.sock.read_exact(&mut buf[..n])?;
            if first && n >= 4 && &buf[..4] == b"ERR:" {
                let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
                // Drain the terminating 0-frame so the socket stays in sync.
                let mut h2 = [0u8; 4];
                let _ = self.sock.read_exact(&mut h2);
                return Err(io::Error::other(msg));
            }
            first = false;
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
    }

    /// Stream `src` to the server as `[len][chunk]…[len=0]` frames.
    pub(crate) fn send_chunks_from(&mut self, src: &mut dyn Read) -> io::Result<u64> {
        let mut total = 0u64;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                self.sock.write_all(&0u32.to_be_bytes())?;
                return Ok(total);
            }
            let len = u32::try_from(n).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "chunk too large")
            })?;
            self.sock.write_all(&len.to_be_bytes())?;
            self.sock.write_all(&buf[..n])?;
            total += n as u64;
        }
    }
}

// ---------- bench ----------

fn bench(host: &str, port: u16, n: u32) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    // Discover available commands by probing — start with cheap ones.
    // Warm the default mirror first so the size-sweep timing doesn't include
    // the one-time VirtualDisplay creation for each new size.
    let _ = conn.call("screenshot")?;
    let _ = conn.call("screenshot size=480")?;
    let _ = conn.call("screenshot size=1080")?;
    let _ = conn.call("screenshot max=1")?;
    // Then re-warm 768 so the bench starts from the default.
    let _ = conn.call("screenshot size=768")?;

    let cases: &[(&str, &str)] = &[
        ("ping",                          "ping"),
        ("dump",                          "dump"),
        ("dump_active",                   "dump_active"),
        ("screenshot 480 q80 jpeg",       "screenshot size=480"),
        ("screenshot 768 q80 jpeg",       "screenshot size=768"),
        ("screenshot 1080 q80 jpeg",      "screenshot size=1080"),
        ("screenshot 1080 q95 jpeg",      "screenshot size=1080 q=95"),
        ("screenshot native q80 jpeg",    "screenshot max=1"),
        ("screenshot 768 q95 jpeg",       "screenshot size=768 q=95"),
        ("screenshot 768 png",            "screenshot size=768 fmt=png"),
    ];

    eprintln!("warm-socket benchmark, n={n} per command");
    eprintln!("{:<30}  {:>10}  {:>10}  {:>10}  {:>12}", "command", "min ms", "p50 ms", "p95 ms", "bytes");
    for (label, wire) in cases {
        // Warm-up.
        let _ = conn.call(wire)?;
        let mut samples = Vec::with_capacity(n as usize);
        let mut last_len = 0usize;
        for _ in 0..n {
            let t0 = Instant::now();
            let body = conn.call(wire)?;
            samples.push(t0.elapsed());
            last_len = body.len();
        }
        samples.sort();
        let min = samples[0];
        let p50 = samples[samples.len() / 2];
        let p95 = samples[(samples.len() as f64 * 0.95) as usize];
        eprintln!(
            "{:<30}  {:>10.3}  {:>10.3}  {:>10.3}  {:>12}",
            label,
            ms(min),
            ms(p50),
            ms(p95),
            last_len,
        );
    }
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn print_devices() -> io::Result<()> {
    let mut rows = daemon::devices()?;
    if rows.is_empty() {
        println!("no devices attached");
        return Ok(());
    }
    // If exactly one device is attached and no daemon is running for it
    // yet, transparently `hs use` so the listing shows it ready to go.
    if rows.len() == 1 && rows[0].state == "device" && !rows[0].running {
        let opts = daemon::ConnectOpts {
            serial: Some(rows[0].serial.clone()),
            port: None,
        };
        if let Ok(port) = daemon::connect(&opts) {
            eprintln!("daemon up on tcp:{port} (auto)");
            rows = daemon::devices()?;
        }
    }
    println!(
        "{:<24} {:<10} {:<7} {:<8} {:<10} {:<16} model",
        "serial", "state", "jar", "running", "host_port", "sdk"
    );
    for r in rows {
        println!(
            "{:<24} {:<10} {:<7} {:<8} {:<10} {:<16} {}",
            r.serial,
            r.state,
            if r.jar_present { "yes" } else { "no" },
            if r.running { "yes" } else { "no" },
            r.host_port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            r.sdk.unwrap_or_else(|| "-".into()),
            r.model.unwrap_or_else(|| "-".into()),
        );
    }
    Ok(())
}

fn is_local(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// If exactly one device is attached, run the equivalent of `hs use`
/// against it so daemon-needing verbs work without an explicit `hs use`
/// first. Returns a short human reason on the error path.
fn try_auto_use(port: u16) -> Result<(), String> {
    let rows = daemon::devices().map_err(|e| format!("adb devices failed: {e}"))?;
    let alive: Vec<_> = rows.iter().filter(|r| r.state == "device").collect();
    match alive.len() {
        0 => Err("no devices attached".into()),
        1 => {
            let opts = daemon::ConnectOpts {
                serial: Some(alive[0].serial.clone()),
                port: Some(port),
            };
            let p = daemon::connect(&opts)
                .map_err(|e| format!("daemon::connect failed: {e}"))?;
            eprintln!("daemon up on tcp:{p} (auto)");
            Ok(())
        }
        n => Err(format!(
            "{n} devices attached; pass `hs use SERIAL` to pick one"
        )),
    }
}

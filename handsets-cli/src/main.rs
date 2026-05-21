// hs CLI.
//
// Talks to the on-device daemon over a TCP socket using a length-prefixed
// binary protocol (uint32 big-endian length + payload, both directions).
//
// One-shot calls open a fresh socket each invocation. The `bench` subcommand
// reuses a persistent socket to measure true wire-level latency.

use std::io::{self, IsTerminal, Read, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

mod adb;
mod daemon;
mod json;
mod mirror;
mod screen;
mod selector;
mod shell;
mod snapshot;
mod state_cache;
mod tap_text;
mod term;
mod ui_dump;
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
            eprintln!("hs: {e}");
            ExitCode::from(1)
        }
    }
}

const USAGE: &str = "\
Usage: hs [--host HOST] [--port PORT] <verb> [args]

  hs                                 list attached devices
  hs use [SERIAL] [--port N]         connect (or switch) the active device
  hs drop [SERIAL] [--keep-jar]      disconnect; rm /data/local/tmp/hs.jar

  hs see                             open the live viewer (GUI)
  hs see foo.jpg | foo.png           save a screenshot at native resolution
  hs see foo.xml | foo.json          save the UI hierarchy

  hs ui [-i|--json|--xml] [--all]    UI tree dump (human outline by default;
                                       -i / --interactive = only tappable /
                                          content-bearing nodes, flat columnar;
                                       --json = raw daemon JSON,
                                       --xml  = uiautomator-style XML;
                                       --all  = every window, not just active)
  hs find SELECTOR                   CSS-like match over the live tree:
                                       `Tag[attr=val][attr~=sub]:flag`
                                       comma = OR. Prints bounds + centre.
  hs info                            neofetch-style device summary
  hs show                            device snapshot (cached, ~2ms)
  hs show top                        top activity component
  hs show PKG                        package info (path, ...)

  hs apps [--3rd]                    installed packages
  hs open PKG | PKG/.Cls             start activity
  hs close PKG                       force-stop
  hs install APK [APK ...]           streamed PackageInstaller session
  hs uninstall PKG

  hs tap \"Login\"                     find by text/desc, tap centre
  hs tap X Y                         raw-coordinate tap
  hs type TEXT                       type into the focused field (KeyEvents)
  hs type SELECTOR TEXT              ACTION_SET_TEXT on the matching node
                                       (no virtual keyboard, atomic)
  hs go back | home | recents | …    key events (case-insensitive)
  hs swipe left|right|up|down [DUR_MS]   80% screen swipe (daemon picks coords)
  hs swipe X1 Y1 X2 Y2 [DUR_MS]          raw-coordinate swipe

  hs wait idle [Nms|Ns]              wait for the UI to settle
  hs wait \"Login\"                    wait for that text to appear
  hs wait PKG | PKG/.Cls             wait for activity (package-prefix)
  hs wait Nms | Ns                   client-side sleep

  hs cp device:/path /host/path      pull (rsync direction)
  hs cp /host/path device:/path      push

  hs prop KEY                        getprop
  hs prop KEY VALUE                  setprop
  hs settings NS KEY                 settings get
  hs settings NS KEY VALUE           settings put

  hs logs [--tail N | --follow]      tail logcat (default last 100 lines)
  hs events                          stream lifecycle events (am monitor)

  hs shell                           interactive REPL (`help`, `exit`, history)
                                       hs shell  is the canonical verb;
                                       hs do     is the same thing.
  hs do <wire-cmd>                   fire one wire command (raw protocol)

Global options:
  --host HOST                        default 127.0.0.1
  --port PORT                        default 9008

Low-level (kept for power users):
  hs ping  hs snapshot  hs screen  hs bench  hs quit  hs input <subcmd>
";

#[derive(Debug)]
struct Opts {
    host: String,
    port: u16,
    cmd: Cmd,
}

#[derive(Debug)]
enum Cmd {
    Ping,
    Dump { xml: bool },
    DumpActive { xml: bool },
    Query(String),
    Screenshot(ShotOpts),
    Input(String),       // pre-built wire command, e.g. "tap x=720 y=1500"
    TapText(String),     // dump→find→tap by text/content-description
    Snapshot,            // dump→list clickable labels in reading order
    Screen,              // dump→render layout as a text grid (aspect-fit)
    Mirror(mirror::Args),// launch GUI viewer (handsets-viewer)
    Quit,
    Bench { n: u32 },
    Adb(adb::Cmd),       // wire-level subcommands routed via the daemon socket
    Connect(daemon::ConnectOpts),
    Disconnect(daemon::DisconnectOpts),
    See(Option<String>),                 // None = open viewer; Some(path) → dispatch on extension
    Wait(String),                        // smart-dispatch: "idle", "<text>", "<pkg>", "Nms"
    Cp { src: String, dst: String },     // direction inferred from "device:" prefix
    ShowPkg(String),                     // composed: pm path + dumpsys package summary
    Info,                                // neofetch-style device snapshot
    TypeInto { selector: String, text: String },  // node_set_text variant
    SettingsListAll,                     // hs settings (bare) → system+secure+global
    Ui { format: UiFormat, all: bool },  // human / json / xml dump
    Devices,
    StateDaemon,
    Shell,
}

#[derive(Debug, Clone, Copy)]
enum UiFormat { Human, Interactive, Json, Xml }

#[derive(Debug, Default, Clone)]
struct ShotOpts {
    size: Option<u32>,
    quality: Option<u32>,
    format: Option<String>,
    native: bool,
}

impl ShotOpts {
    fn wire(&self) -> String {
        let mut s = String::from("screenshot");
        if self.native {
            s.push_str(" max=1");
        } else if let Some(sz) = self.size {
            s.push_str(&format!(" size={sz}"));
        }
        if let Some(q) = self.quality {
            s.push_str(&format!(" q={q}"));
        }
        if let Some(f) = &self.format {
            s.push_str(&format!(" fmt={f}"));
        }
        s
    }
}

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

/// Parse the trailing tokens of `hs input <subcmd> ...` into the wire
/// command the daemon expects.
/// `hs tap` — text-lookup when arg isn't a pair of integers, raw coords
/// when it is.
fn parse_tap(rest: &[&str]) -> Result<Cmd, String> {
    if rest.is_empty() {
        return Err("tap needs either TEXT or X Y coords".into());
    }
    if rest.len() == 2 {
        let (x, y) = (rest[0].parse::<i32>(), rest[1].parse::<i32>());
        if let (Ok(x), Ok(y)) = (x, y) {
            return Ok(Cmd::Input(format!("tap x={x} y={y}")));
        }
    }
    if rest.len() == 1 && rest[0].parse::<i32>().is_ok() {
        return Err("tap with a single number is ambiguous — pass two ints for coords, or quote text".into());
    }
    Ok(Cmd::TapText(rest.join(" ")))
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

fn parse_input_subcommand(rest: &[&str]) -> Result<String, String> {
    let (sub, rest) = rest.split_first().ok_or("input needs a subcommand")?;
    match *sub {
        "tap" => {
            if rest.len() != 2 {
                return Err("input tap takes: X Y".into());
            }
            let x: i32 = rest[0].parse().map_err(|_| "bad tap X")?;
            let y: i32 = rest[1].parse().map_err(|_| "bad tap Y")?;
            Ok(format!("tap x={x} y={y}"))
        }
        "swipe" => {
            if rest.len() != 4 && rest.len() != 5 {
                return Err("input swipe takes: X1 Y1 X2 Y2 [DUR_MS]".into());
            }
            let x1: i32 = rest[0].parse().map_err(|_| "bad swipe X1")?;
            let y1: i32 = rest[1].parse().map_err(|_| "bad swipe Y1")?;
            let x2: i32 = rest[2].parse().map_err(|_| "bad swipe X2")?;
            let y2: i32 = rest[3].parse().map_err(|_| "bad swipe Y2")?;
            let dur: i32 = if rest.len() == 5 {
                rest[4].parse().map_err(|_| "bad swipe DUR_MS")?
            } else {
                300
            };
            Ok(format!("swipe x1={x1} y1={y1} x2={x2} y2={y2} dur={dur}"))
        }
        "key" => {
            if rest.len() != 1 {
                return Err("input key takes: NAME (or code=N)".into());
            }
            // Pass-through: "input key BACK" → "key BACK"; "input key code=4" → "key code=4".
            Ok(format!("key {}", rest[0]))
        }
        "text" => {
            if rest.is_empty() {
                return Err("input text needs STRING".into());
            }
            // Re-join all positional args with single spaces, preserving the
            // string content. The daemon takes everything after "text " as the
            // typed text verbatim.
            Ok(format!("text {}", rest.join(" ")))
        }
        other => Err(format!("unknown input subcommand: {other}")),
    }
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT;
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
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => positional.push(a),
        }
        i += 1;
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
            if rest.is_empty() { return Err("find needs a CSS-like SELECTOR".into()); }
            Cmd::Query(rest.join(" "))
        }
        Some((&"ui", rest)) => {
            let mut fmt = UiFormat::Human;
            let mut all = false;
            for tok in rest {
                match *tok {
                    "--json"            => fmt = UiFormat::Json,
                    "--xml"             => fmt = UiFormat::Xml,
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
            if rest.is_empty() { return Err("wait needs SPEC (idle | TEXT | PKG | Nms)".into()); }
            Cmd::Wait(rest.join(" "))
        }

        // ─── Input ───────────────────────────────────────────────────
        Some((&"tap", rest)) => parse_tap(rest)?,
        Some((&"type", rest)) => match rest.len() {
            0 => return Err("type needs TEXT (1 arg) or SELECTOR TEXT (2 args)".into()),
            1 => Cmd::Input(format!("text {}", rest[0])),   // focused-field KeyEvents
            2 => Cmd::TypeInto { selector: rest[0].into(), text: rest[1].into() },
            _ => return Err("type takes TEXT or SELECTOR TEXT — quote multi-word arguments".into()),
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
                    return Ok(Opts { host, port, cmd: Cmd::Input(wire) });
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

        // ─── Shell + raw wire ────────────────────────────────────────
        // `shell` and `do` are synonyms: shell is the natural REPL verb,
        // do <wire> is a one-shot raw call.
        Some((&"shell", _)) => Cmd::Shell,
        Some((&"do", rest)) => {
            if rest.is_empty() { Cmd::Shell } else { Cmd::Input(rest.join(" ")) }
        }

        // ─── Internal / low-level (kept, undocumented in --help) ─────
        Some((&"ping", _))      => Cmd::Ping,
        Some((&"snapshot", _))  => Cmd::Snapshot,
        Some((&"screen", _))    => Cmd::Screen,
        Some((&"quit", _))      => Cmd::Quit,
        Some((&"state-daemon", _)) => Cmd::StateDaemon,
        Some((&"bench", rest)) => {
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
            Cmd::Bench { n }
        }

        Some((other, _)) => return Err(suggest(other)),
    };

    Ok(Opts { host, port, cmd })
}

fn run(opts: &Opts) -> io::Result<()> {
    match &opts.cmd {
        Cmd::Bench { n } => bench(&opts.host, opts.port, *n),
        Cmd::TapText(text) => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            tap_text::run(&mut conn, text)
        }
        Cmd::Snapshot => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            snapshot::run(&mut conn)
        }
        Cmd::Screen => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            screen::run(&mut conn)
        }
        Cmd::Mirror(args) => mirror::run(&opts.host, opts.port, *args),
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
        Cmd::Dump { xml: true } => fetch_then_xml(&opts.host, opts.port, "dump"),
        Cmd::DumpActive { xml: true } => fetch_then_xml(&opts.host, opts.port, "dump_active"),
        Cmd::Query(sel) => run_query(&opts.host, opts.port, sel),
        Cmd::See(dest) => run_see(&opts.host, opts.port, dest.as_deref()),
        Cmd::Wait(spec) => run_wait(&opts.host, opts.port, spec),
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
        Cmd::TypeInto { selector, text } => {
            // Translate to a daemon wire command: node_set_text <selector> value=TEXT.
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("node_set_text {selector} value={text:?}");
            let body = conn.call(&wire)?;
            if body.starts_with(b"ERR:") {
                return Err(io::Error::other(String::from_utf8_lossy(&body).into_owned()));
            }
            let mut out = io::stdout().lock();
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
            out.flush()
        }
        _ => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire: String = match &opts.cmd {
                Cmd::Ping => "ping".into(),
                Cmd::Dump { .. } => "dump".into(),
                Cmd::DumpActive { .. } => "dump_active".into(),
                Cmd::Quit => "quit".into(),
                Cmd::Screenshot(s) => s.wire(),
                Cmd::Input(wire) => wire.clone(),
                Cmd::Bench { .. }
                | Cmd::TapText(_)
                | Cmd::Snapshot
                | Cmd::Screen
                | Cmd::Mirror(_)
                | Cmd::Adb(_)
                | Cmd::Connect(_)
                | Cmd::Disconnect(_)
                | Cmd::Devices
                | Cmd::StateDaemon
                | Cmd::Shell
                | Cmd::Query(_)
                | Cmd::See(_)
                | Cmd::Wait(_)
                | Cmd::Cp { .. }
                | Cmd::ShowPkg(_)
                | Cmd::Info
                | Cmd::TypeInto { .. }
                | Cmd::SettingsListAll
                | Cmd::Ui { .. } => unreachable!(),
            };
            let body = conn.call(&wire)?;
            write_response(&opts.cmd, &body)
        }
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

/// Fetch the daemon's JSON dump and transform it to uiautomator-style XML
/// client-side. No daemon-side change required.
fn fetch_then_xml(host: &str, port: u16, wire: &str) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    let body = conn.call(wire)?;
    let text = std::str::from_utf8(&body)
        .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    let json = json::parse(text)
        .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
    let rotation = read_rotation(&json);
    let xml = xml_dump::render(&json, rotation);
    let mut out = io::stdout().lock();
    out.write_all(xml.as_bytes())?;
    out.flush()
}

fn read_rotation(v: &json::Value) -> i64 {
    // Best-effort: the dump payload doesn't currently carry the device
    // rotation; emit 0 so the resulting XML is uiautomator-shaped.
    let _ = v;
    0
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
    if bytes.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&bytes).into_owned()));
    }
    std::fs::write(path, &bytes)?;
    eprintln!("saved {} bytes to {path}", bytes.len());
    Ok(())
}

/// `hs wait <spec>` — smart-dispatch on `spec` shape.
fn run_wait(host: &str, port: u16, spec: &str) -> io::Result<()> {
    let spec = spec.trim();
    // "Nms" / "Ns" — sleep client-side, no daemon hop needed.
    if let Some(ms) = parse_duration_ms(spec) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        return Ok(());
    }
    let mut conn = Conn::connect(host, port)?;
    let wire: String = if let Some(rest) = spec.strip_prefix("idle") {
        let rest = rest.trim();
        if rest.is_empty() {
            "wait_for_idle idle_ms=200 timeout_ms=5000".into()
        } else if let Some(ms) = parse_duration_ms(rest) {
            format!("wait_for_idle idle_ms={ms} timeout_ms=10000")
        } else {
            return Err(io::Error::other(format!("wait idle: bad duration '{rest}'")));
        }
    } else if is_component(spec) || is_pkg(spec) {
        format!("wait_for_activity n={spec} timeout_ms=10000")
    } else {
        format!("wait_for_text text={spec:?} timeout_ms=10000")
    };
    let body = conn.call(&wire)?;
    let mut out = io::stdout().lock();
    out.write_all(&body)?;
    if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
    out.flush()
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

/// Match a CSS-like selector against the current dump_active tree and print
/// one node summary per match (bounds + class + text/id when present).
fn run_query(host: &str, port: u16, sel: &str) -> io::Result<()> {
    let selectors = selector::Selector::parse(sel).map_err(io::Error::other)?;
    let mut conn = Conn::connect(host, port)?;
    let body = conn.call("dump_active")?;
    let text = std::str::from_utf8(&body)
        .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    let json = json::parse(text)
        .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
    // The dump_active payload's tree lives under `.root`. find_all walks
    // any subtree; pass the top object so it descends into `.root`/`.children`.
    let matches = selector::find_all(&json, &selectors);
    let mut out = io::stdout().lock();
    for n in matches {
        let cls = selector::get_str(n, "cls").unwrap_or("");
        let id  = selector::get_str(n, "rid").unwrap_or("");
        let t   = selector::get_str(n, "text").unwrap_or("");
        let d   = selector::get_str(n, "desc").unwrap_or("");
        let b   = selector::bounds(n);
        let bs  = match b {
            Some((x1, y1, x2, y2)) => {
                let cx = (x1 + x2) / 2;
                let cy = (y1 + y2) / 2;
                format!("[{x1},{y1}][{x2},{y2}] center=({cx},{cy})")
            }
            None => "[]".into(),
        };
        writeln!(out, "{bs}\tclass={cls}\tid={id}\ttext={t:?}\tdesc={d:?}")?;
    }
    out.flush()
}

fn write_response(cmd: &Cmd, body: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(body)?;
    // Trailing newline only for text commands on a terminal — never for binary.
    let is_text = matches!(
        cmd,
        Cmd::Ping | Cmd::Dump { .. } | Cmd::DumpActive { .. } | Cmd::Quit
            | Cmd::Input(_) | Cmd::Bench { .. } | Cmd::Query(_)
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
        let sock = TcpStream::connect((host, port))?;
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
    let rows = daemon::devices()?;
    if rows.is_empty() {
        println!("no devices attached");
        return Ok(());
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

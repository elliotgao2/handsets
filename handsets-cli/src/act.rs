// `hs act` — composite "do an action, then wait for a predicate" verb.
//
// Rolled into one call so RPA scripts stop writing the `tap → wait_for_text
// → branch on result` triple by hand. Reuses the shared `ActionFlags`
// surface (timeouts, retries, filters) so it stays consistent with `hs tap`
// / `hs wait` semantics.

use std::io;
use std::time::{Duration, Instant};

use crate::errors::{ErrCode, ErrInfo};
use crate::flags::{ActionFlags, OutFmt};
use crate::json_out::Obj;
use crate::output::Reporter;
use crate::session::Session;

#[derive(Debug, Clone)]
pub struct ActOpts {
    /// Action to perform. Exactly one is required.
    pub action: ActAction,
    /// Predicate to wait for after the action.
    pub until: ActUntil,
    pub flags: ActionFlags,
}

#[derive(Debug, Clone)]
pub enum ActAction {
    TapText(String),
    TapXY(i32, i32),
    TypeText(String),                 // type into focused field
    TypeInto(String, String),         // selector, text (ACTION_SET_TEXT)
    KeyEvent(String),
    SwipeDir(String, Option<i32>),    // direction, optional duration
}

#[derive(Debug, Clone)]
pub enum ActUntil {
    Text(String),
    Activity(String),
    Selector(String),
    Idle,
}

pub fn parse(rest: &[&str]) -> Result<ActOpts, String> {
    // Strip the shared ActionFlags surface (--timeout/--retries/...) in a
    // single pre-pass so per-flag arg lookahead works correctly. The
    // surviving tokens are then walked for act-specific verbs (--tap,
    // --type, --until …).
    let mut flags = ActionFlags::default();
    let scoped = flags.take(rest)?;
    let scoped: Vec<&str> = scoped;
    let mut action: Option<ActAction> = None;
    let mut until:  Option<ActUntil>  = None;
    let mut i = 0;
    while i < scoped.len() {
        match scoped[i] {
            "--tap" => {
                i += 1;
                let target = scoped.get(i).ok_or("--tap needs TEXT or X")?;
                if let (Some(x_str), Some(y_str)) = (scoped.get(i), scoped.get(i + 1)) {
                    if let (Ok(x), Ok(y)) = (x_str.parse::<i32>(), y_str.parse::<i32>()) {
                        action = Some(ActAction::TapXY(x, y));
                        i += 2;
                        continue;
                    }
                }
                action = Some(ActAction::TapText(target.to_string()));
            }
            "--type" => {
                i += 1;
                let t = scoped.get(i).ok_or("--type needs TEXT")?;
                action = Some(ActAction::TypeText(t.to_string()));
            }
            "--fill" => {
                i += 1;
                let sel  = scoped.get(i).ok_or("--fill needs SELECTOR TEXT")?;
                let text = scoped.get(i + 1).ok_or("--fill needs SELECTOR TEXT")?;
                action = Some(ActAction::TypeInto(sel.to_string(), text.to_string()));
                i += 1;
            }
            "--key" => {
                i += 1;
                let k = scoped.get(i).ok_or("--key needs KEYNAME")?;
                action = Some(ActAction::KeyEvent(k.to_uppercase()));
            }
            "--swipe" => {
                i += 1;
                let d = scoped.get(i).ok_or("--swipe needs DIR")?.to_lowercase();
                if !matches!(d.as_str(), "left" | "right" | "up" | "down") {
                    return Err(format!("--swipe DIR must be left|right|up|down, got {d}"));
                }
                let dur = scoped.get(i + 1).and_then(|s| s.parse::<i32>().ok());
                if dur.is_some() { i += 1; }
                action = Some(ActAction::SwipeDir(d, dur));
            }
            "--until" => {
                i += 1;
                let target = scoped.get(i).ok_or("--until needs SPEC")?;
                until = Some(parse_until(target));
            }
            "--until-idle" => until = Some(ActUntil::Idle),
            "--until-text" => {
                i += 1;
                let t = scoped.get(i).ok_or("--until-text needs TEXT")?;
                until = Some(ActUntil::Text(t.to_string()));
            }
            "--until-activity" => {
                i += 1;
                let t = scoped.get(i).ok_or("--until-activity needs PKG/.Cls")?;
                until = Some(ActUntil::Activity(t.to_string()));
            }
            "--until-selector" => {
                i += 1;
                let t = scoped.get(i).ok_or("--until-selector needs SELECTOR")?;
                until = Some(ActUntil::Selector(t.to_string()));
            }
            other => return Err(format!("act: unexpected token '{other}'")),
        }
        i += 1;
    }
    let action = action.ok_or("act needs --tap/--type/--key/--swipe")?;
    let until  = until .ok_or("act needs --until (text/PKG/selector) or --until-idle")?;
    Ok(ActOpts { action, until, flags })
}

/// `--until "Login"` → ActUntil::Text; `--until com.foo[/.Class]` → Activity;
/// `--until '[id=foo]'` → Selector (heuristic: leading `[`, `*`, or contains
/// `:` flag syntax / `=`).
fn parse_until(spec: &str) -> ActUntil {
    let s = spec.trim();
    if s.starts_with('[') || s.starts_with('*')
        || s.contains("[") || s.contains(":clickable")
        || s.contains(":visible") || s.contains(":enabled")
    {
        return ActUntil::Selector(s.to_string());
    }
    if crate::is_component(s) || crate::is_pkg(s) {
        return ActUntil::Activity(s.to_string());
    }
    ActUntil::Text(s.to_string())
}

pub fn run(host: &str, port: u16, out_fmt: OutFmt, opts: &ActOpts) -> io::Result<()> {
    let reporter = Reporter::new(opts.flags.out(out_fmt));
    let mut sess = Session::connect(host, port, opts.flags.clone(), out_fmt)?;
    // 1. Perform the action.
    perform_action(&mut sess, &opts.action, &reporter)?;
    // 2. Wait for the predicate (sharing the same warm socket).
    wait_until(&mut sess, &opts.until, &opts.flags, &reporter)?;
    // 3. Success record.
    reporter.ok("act", "act ok", Obj::new()
        .s("action", action_kind(&opts.action))
        .s("until", until_kind(&opts.until)))
}

fn perform_action(sess: &mut Session, a: &ActAction, reporter: &Reporter) -> io::Result<()> {
    match a {
        ActAction::TapText(q) => {
            // Reuse the text-tap path with the session's flags & cache.
            crate::tap_text::run_session(sess, q, reporter, "act")
        }
        ActAction::TapXY(x, y) => {
            let body = sess.conn.call(&format!("tap x={x} y={y}"))?;
            if let Some(e) = crate::errors::parse_err(&body) {
                return Err(reporter.fail("act", e));
            }
            Ok(())
        }
        ActAction::TypeText(t) => {
            let body = sess.conn.call(&format!("text {t}"))?;
            if let Some(e) = crate::errors::parse_err(&body) {
                return Err(reporter.fail("act", e));
            }
            Ok(())
        }
        ActAction::TypeInto(sel, text) => {
            let body = sess.conn.call(&format!("node_set_text {sel} value={text:?}"))?;
            if let Some(e) = crate::errors::parse_err(&body) {
                return Err(reporter.fail("act", e));
            }
            Ok(())
        }
        ActAction::KeyEvent(k) => {
            let body = sess.conn.call(&format!("key {k}"))?;
            if let Some(e) = crate::errors::parse_err(&body) {
                return Err(reporter.fail("act", e));
            }
            Ok(())
        }
        ActAction::SwipeDir(d, dur) => {
            let wire = match dur {
                Some(n) => format!("swipe_dir {d} dur={n}"),
                None => format!("swipe_dir {d}"),
            };
            let body = sess.conn.call(&wire)?;
            if let Some(e) = crate::errors::parse_err(&body) {
                return Err(reporter.fail("act", e));
            }
            Ok(())
        }
    }
}

fn wait_until(
    sess: &mut Session,
    until: &ActUntil,
    flags: &ActionFlags,
    reporter: &Reporter,
) -> io::Result<()> {
    let timeout_ms = flags.timeout_ms.unwrap_or(10_000);
    let wire: String = match until {
        ActUntil::Text(t)     => format!("wait_for_text text={t:?} timeout_ms={timeout_ms}"),
        ActUntil::Activity(a) => format!("wait_for_activity n={a} timeout_ms={timeout_ms}"),
        ActUntil::Idle        => format!("wait_for_idle idle_ms=200 timeout_ms={timeout_ms}"),
        ActUntil::Selector(sel) => {
            // No dedicated wire for "wait for selector"; poll client-side
            // by re-issuing dump_active until it matches. Honours timeout.
            return poll_selector(sess, sel, flags, timeout_ms, reporter);
        }
    };
    let body = sess.conn.call(&wire)?;
    if let Some(e) = crate::errors::parse_err(&body) {
        return Err(reporter.fail("act", e));
    }
    Ok(())
}

fn poll_selector(
    sess: &mut Session,
    sel: &str,
    flags: &ActionFlags,
    timeout_ms: u64,
    reporter: &Reporter,
) -> io::Result<()> {
    let selectors = match crate::selector::Selector::parse(sel) {
        Ok(s) => s,
        Err(e) => return Err(reporter.fail("act", ErrInfo::new(ErrCode::BadArg, e))),
    };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        sess.invalidate_dump();
        let found = sess.with_dump(|dump| {
            let ctx = crate::selector::MatchCtx::new(dump);
            let mut matches = crate::selector::find_all_with(&ctx, &selectors);
            crate::selector::apply_filters(&mut matches, flags);
            !matches.is_empty()
        })?;
        if found { return Ok(()); }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(reporter.fail("act",
        ErrInfo::new(ErrCode::Timeout, format!("act --until [{sel}]"))))
}

fn action_kind(a: &ActAction) -> &'static str {
    match a {
        ActAction::TapText(_)   => "tap_text",
        ActAction::TapXY(_, _)  => "tap_xy",
        ActAction::TypeText(_)  => "type",
        ActAction::TypeInto(..) => "type_into",
        ActAction::KeyEvent(_)  => "key",
        ActAction::SwipeDir(..) => "swipe",
    }
}

fn until_kind(u: &ActUntil) -> &'static str {
    match u {
        ActUntil::Text(_)     => "text",
        ActUntil::Activity(_) => "activity",
        ActUntil::Selector(_) => "selector",
        ActUntil::Idle        => "idle",
    }
}

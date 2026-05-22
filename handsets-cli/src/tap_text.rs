// `hs tap <TEXT>` — dump the active window, find a node whose `text` or
// `desc` matches, tap its bounds centre on the same connection.
//
// The session-aware path (`run_session`) is the canonical one — it honours
// the shared ActionFlags surface (retries, --visible, --unique, --nth) and
// emits its result through the Reporter so `--json` and structured exit
// codes work without touching the call site. The legacy `run(&mut Conn,…)`
// is kept as a thin shim for the snapshot/screen modules that still drive
// a raw Conn.

use std::io;
use std::time::Duration;

use crate::errors::{ErrCode, ErrInfo, parse_err};
use crate::flags::ActionFlags;
use crate::json::{as_arr, as_num, as_str, obj_get, Value};
use crate::json_out::Obj;
use crate::output::Reporter;
use crate::session::Session;
use crate::Conn;

/// Match priority — lower is better. The walk records the best (lowest-
/// priority-number) candidate seen in DFS order; on ties the first node wins
/// (which is closer to the root → usually the visible container rather than
/// a child label).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    ExactText = 0,
    ExactDesc = 1,
    IExactText = 2,
    IExactDesc = 3,
    SubText = 4,
    SubDesc = 5,
}

#[derive(Debug, Clone)]
struct Hit {
    text: String,
    desc: String,
    cls: String,
    flags: String,
    bounds: (i32, i32, i32, i32),
    priority: Priority,
}

/// Legacy entry point used by `snapshot`/`screen`; thin shim around the
/// session-aware path so the find-and-tap heuristic stays in one place.
pub fn run(conn: &mut Conn, query: &str) -> io::Result<()> {
    let body = conn.call("dump_active")?;
    if let Some(e) = parse_err(&body) {
        return Err(io::Error::other(format!("dump_active: ERR:{}", e.detail)));
    }
    let json = std::str::from_utf8(&body)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "dump_active not utf-8"))?;
    let root = crate::json::parse(json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("json: {e}")))?;
    let tree = obj_get(&root, "root").unwrap_or(&root);
    let hit = find(tree, query, &ActionFlags::default()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound,
            format!("no a11y node matched \"{query}\""))
    })?;
    let (l, t, r, b) = hit.bounds;
    let cx = (l + r) / 2;
    let cy = (t + b) / 2;
    let ack = conn.call(&format!("tap x={cx} y={cy}"))?;
    let ack_s = String::from_utf8_lossy(&ack);
    let label = if !hit.text.is_empty() { hit.text.as_str() } else { hit.desc.as_str() };
    eprintln!(
        "tapped {label:?} cls={} flags={} bounds=[{},{},{},{}] at ({},{}) → {}",
        hit.cls, hit.flags, l, t, r, b, cx, cy, ack_s
    );
    if ack_s.starts_with("ERR:") { return Err(io::Error::other(ack_s.into_owned())); }
    Ok(())
}

/// Session-aware tap-by-text. Honours retries/--visible/--clickable/--unique
/// from the session defaults and reports through `Reporter` so JSON output
/// stays consistent across action verbs. `verb` is the logical CLI verb
/// used in the report record (so `hs act --tap` can stamp `"act"` rather
/// than `"tap"` if it wants to).
pub fn run_session(
    sess: &mut Session,
    query: &str,
    reporter: &Reporter,
    verb: &'static str,
) -> io::Result<()> {
    let flags = sess.defaults.clone();
    let total = flags.total_attempts();
    let mut last_err: Option<ErrInfo> = None;
    for attempt in 0..total {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(flags.retry_delay_ms));
        }
        if flags.fresh || attempt > 0 {
            sess.invalidate_dump();
        }
        let hits = match sess.with_dump(|tree| {
            // dump_active envelopes as { ts: ..., root: { ... } }; descend
            // into the inner node so the walker hits the actual a11y root.
            let root = obj_get(tree, "root").unwrap_or(tree);
            find_all(root, query, &flags)
        }) {
            Ok(h) => h,
            Err(e) => {
                if let Some(info) = io_err_to_info(&e) { last_err = Some(info); continue; }
                return Err(e);
            }
        };
        match candidate(&hits, &flags) {
            Ok(hit) => {
                let (l, t, r, b) = hit.bounds;
                let cx = (l + r) / 2;
                let cy = (t + b) / 2;
                let ack = sess.conn.call(&format!("tap x={cx} y={cy}"))?;
                if let Some(e) = parse_err(&ack) {
                    last_err = Some(e);
                    continue;
                }
                let label = if !hit.text.is_empty() { hit.text.as_str() } else { hit.desc.as_str() };
                let human = format!(
                    "tapped {label:?} cls={} flags={} bounds=[{l},{t},{r},{b}] at ({cx},{cy}) → ok",
                    hit.cls, hit.flags,
                );
                return reporter.ok(verb, &human, Obj::new()
                    .s("matched", label)
                    .s("class", &hit.cls)
                    .s("flags", &hit.flags)
                    .n("x", cx as i64).n("y", cy as i64)
                    .n("x1", l as i64).n("y1", t as i64)
                    .n("x2", r as i64).n("y2", b as i64));
            }
            Err(info) => last_err = Some(info),
        }
    }
    Err(reporter.fail(verb, last_err.unwrap_or_else(|| ErrInfo::new(
        ErrCode::NotFound, format!("no a11y node matched \"{query}\"")))))
}

fn candidate(hits: &[Hit], flags: &ActionFlags) -> Result<Hit, ErrInfo> {
    if hits.is_empty() {
        return Err(ErrInfo::new(ErrCode::NotFound, "no matching node"));
    }
    if flags.require_unique && hits.len() > 1 {
        return Err(ErrInfo::new(ErrCode::Ambiguous,
            format!("--unique requires 1 match, got {}", hits.len())));
    }
    let idx = match flags.nth { Some(n) => n - 1, None => 0 };
    if idx >= hits.len() {
        return Err(ErrInfo::new(ErrCode::NotFound,
            format!("--nth {} out of range (have {})", idx + 1, hits.len())));
    }
    Ok(hits[idx].clone())
}

fn io_err_to_info(e: &io::Error) -> Option<ErrInfo> {
    if let Some(inner) = e.get_ref() {
        if let Some(r) = inner.downcast_ref::<crate::output::ReportedError>() {
            return Some(r.info.clone());
        }
    }
    None
}

/// Single best match — legacy callers that don't need the multi-match
/// disambiguation surface. Honours --visible/--clickable/--enabled but
/// not --unique/--nth (those are caller-side concerns).
fn find(root: &Value, query: &str, flags: &ActionFlags) -> Option<Hit> {
    let mut hits = find_all(root, query, flags);
    if hits.is_empty() { None } else { Some(hits.remove(0)) }
}

/// Walk the tree, classify every node whose text/desc matches `query`,
/// then return the matches sorted by priority (best first). Filters apply
/// upstream of priority so a visible substring match still beats an
/// invisible exact one when --visible is set.
fn find_all(root: &Value, query: &str, flags: &ActionFlags) -> Vec<Hit> {
    let q_low = query.to_ascii_lowercase();
    let mut hits: Vec<Hit> = Vec::new();
    walk(root, &mut |node| {
        let bounds = bounds_of(node)?;
        let text = obj_get(node, "text").and_then(as_str).unwrap_or("");
        let desc = obj_get(node, "desc").and_then(as_str).unwrap_or("");
        let pri = classify(text, desc, query, &q_low)?;
        let cls = obj_get(node, "cls").and_then(as_str).unwrap_or("").to_string();
        let flag_str = obj_get(node, "flags").and_then(as_str).unwrap_or("").to_string();
        if flags.require_clickable && !flag_str.contains('c') { return Some(()); }
        if flags.require_enabled   && !flag_str.contains('e') { return Some(()); }
        if flags.require_visible {
            if !flag_str.contains('v') { return Some(()); }
            if bounds.2 <= bounds.0 || bounds.3 <= bounds.1 { return Some(()); }
        }
        hits.push(Hit {
            text: text.to_string(),
            desc: desc.to_string(),
            cls,
            flags: flag_str,
            bounds,
            priority: pri,
        });
        Some(())
    });
    hits.sort_by_key(|h| h.priority);
    hits
}

fn classify(text: &str, desc: &str, q: &str, q_low: &str) -> Option<Priority> {
    if text == q {
        return Some(Priority::ExactText);
    }
    if desc == q {
        return Some(Priority::ExactDesc);
    }
    let text_low = text.to_ascii_lowercase();
    let desc_low = desc.to_ascii_lowercase();
    if text_low == q_low {
        return Some(Priority::IExactText);
    }
    if desc_low == q_low {
        return Some(Priority::IExactDesc);
    }
    if !q_low.is_empty() && text_low.contains(q_low) {
        return Some(Priority::SubText);
    }
    if !q_low.is_empty() && desc_low.contains(q_low) {
        return Some(Priority::SubDesc);
    }
    None
}

fn walk<F: FnMut(&Value) -> Option<()>>(node: &Value, f: &mut F) {
    if matches!(node, Value::Obj(_)) {
        let _ = f(node);
        if let Some(Value::Arr(children)) = obj_get(node, "children") {
            for c in children {
                walk(c, f);
            }
        }
    }
}

fn bounds_of(node: &Value) -> Option<(i32, i32, i32, i32)> {
    let arr = obj_get(node, "bounds").and_then(as_arr)?;
    if arr.len() != 4 {
        return None;
    }
    Some((
        as_num(&arr[0])? as i32,
        as_num(&arr[1])? as i32,
        as_num(&arr[2])? as i32,
        as_num(&arr[3])? as i32,
    ))
}

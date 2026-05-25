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
    rid: String,
    flags: String,
    bounds: (i32, i32, i32, i32),
    priority: Priority,
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
            // `#id` short-circuits the text/desc walk so anonymous
            // clickable widgets (the ImageView icons that `hs ui`
            // surfaces with a `#name` label) are reachable from `hs tap`.
            if let Some(id_query) = query.strip_prefix('#') {
                find_by_id(root, id_query, &flags)
            } else {
                find_all(root, query, &flags)
            }
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
                // ACTION_CLICK first when the node has a resource-id — it
                // bypasses the input dispatcher (no DOWN/UP gesture, no 16ms
                // sleep, no waiting on a busy UI thread) and shaves ~35ms
                // off the typical tap with much less variance.
                //
                // We compose the selector with the matched text/desc as a
                // discriminator. Many UI scaffolds share one resource-id
                // across siblings — every launcher icon is `:id/icon`, every
                // RecyclerView row is `:id/title`, etc. — so `id=…` alone
                // would match the first DFS hit (Calendar, when tapping
                // 闲鱼). Adding `text=…` / `desc=…` narrows the selector to
                // the exact node the CLI already picked.
                //
                // Custom views that only register OnTouchListener return
                // `click-rejected`; selectors that fail to re-match (live
                // text changed between dump and click) return `not-found`.
                // Both fall through to the gesture path on the matched
                // bounds, so the verb stays universally applicable.
                let mut method: &'static str = "tap";
                let mut acked = false;
                if !hit.rid.is_empty() && hit.flags.contains('c') {
                    let sel = build_click_selector(&hit);
                    let ack = sess.conn.call(&format!("node_click {sel}"))?;
                    match parse_err(&ack) {
                        None => { method = "click"; acked = true; }
                        Some(e) if e.detail.contains("rejected")
                                || e.detail.contains("not-found") => {
                            // Fall through to gesture tap on the matched bounds.
                        }
                        Some(e) => { last_err = Some(e); continue; }
                    }
                }
                if !acked {
                    let ack = sess.conn.call(&format!("tap x={cx} y={cy}"))?;
                    if let Some(e) = parse_err(&ack) {
                        last_err = Some(e);
                        continue;
                    }
                }
                let label = if !hit.text.is_empty() { hit.text.as_str() } else { hit.desc.as_str() };
                let human = format!(
                    "tapped {label:?} cls={} flags={} bounds=[{l},{t},{r},{b}] at ({cx},{cy}) via {method} → ok",
                    hit.cls, hit.flags,
                );
                return reporter.ok(verb, &human, Obj::new()
                    .s("matched", label)
                    .s("class", &hit.cls)
                    .s("flags", &hit.flags)
                    .s("method", method)
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

/// Build the daemon selector for `node_click` against a matched hit. Adds
/// the exact text or content-desc as a discriminator so we land on *this*
/// node rather than any sibling that shares the same resource-id (the
/// launcher-icon collision that caused `hs tap 闲鱼` to land on Calendar).
fn build_click_selector(hit: &Hit) -> String {
    let mut s = format!("id={}", hit.rid);
    if !hit.text.is_empty() {
        // {:?} quote+escape format — the daemon's tokenize() respects
        // double-quoted values, so this round-trips even for labels with
        // spaces. Internal quote characters in labels are rare in practice;
        // the same pattern is used by `node_set_text value={text:?}`.
        s.push_str(&format!(" text={:?}", hit.text));
    } else if !hit.desc.is_empty() {
        s.push_str(&format!(" desc={:?}", hit.desc));
    }
    s
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


/// `#id` lookup. Matches the local part of `rid` (so `#back_btn` finds
/// `com.foo:id/back_btn`) and also accepts the full form when the user
/// pastes a fully-qualified id. Used by `hs tap #name` to reach widgets
/// that `hs ui` lists with a `#name` label (anonymous clickable icons).
fn find_by_id(root: &Value, id_query: &str, flags: &ActionFlags) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    if id_query.is_empty() { return hits; }
    walk(root, &mut |node, _ancestors| {
        let rid = obj_get(node, "rid").and_then(as_str).unwrap_or("");
        if rid.is_empty() { return Some(()); }
        let short = rid.rsplit('/').next().unwrap_or(rid);
        if rid != id_query && short != id_query { return Some(()); }
        let bounds = bounds_of(node)?;
        let text = obj_get(node, "text").and_then(as_str).unwrap_or("").to_string();
        let desc = obj_get(node, "desc").and_then(as_str).unwrap_or("").to_string();
        let cls  = obj_get(node, "cls").and_then(as_str).unwrap_or("").to_string();
        let flag_str = obj_get(node, "flags").and_then(as_str).unwrap_or("").to_string();
        if flags.require_clickable && !flag_str.contains('c') { return Some(()); }
        if flags.require_enabled   && !flag_str.contains('e') { return Some(()); }
        if flags.require_visible {
            if !flag_str.contains('v') { return Some(()); }
            if bounds.2 <= bounds.0 || bounds.3 <= bounds.1 { return Some(()); }
        }
        // Priority is uniform across id matches — they're already as
        // specific as a selector gets, no text/desc tie-break needed.
        hits.push(Hit {
            text, desc, cls, rid: rid.to_string(),
            flags: flag_str, bounds,
            priority: Priority::ExactText,
        });
        Some(())
    });
    hits
}

/// Walk the tree, classify every node whose text/desc matches `query`,
/// then return the matches sorted by priority (best first). Filters apply
/// upstream of priority so a visible substring match still beats an
/// invisible exact one when --visible is set.
fn find_all(root: &Value, query: &str, flags: &ActionFlags) -> Vec<Hit> {
    let q_low = query.to_ascii_lowercase();
    let screen_area = bounds_of(root)
        .map(|(l, t, r, b)| ((r - l).max(0) as i64) * ((b - t).max(0) as i64))
        .unwrap_or(0);
    let mut hits: Vec<Hit> = Vec::new();
    walk(root, &mut |node, ancestors| {
        let raw_bounds = bounds_of(node)?;
        let text = obj_get(node, "text").and_then(as_str).unwrap_or("");
        let desc = obj_get(node, "desc").and_then(as_str).unwrap_or("");
        let pri = classify(text, desc, query, &q_low)?;
        let raw_cls = obj_get(node, "cls").and_then(as_str).unwrap_or("");
        let raw_rid = obj_get(node, "rid").and_then(as_str).unwrap_or("");
        let raw_flags = obj_get(node, "flags").and_then(as_str).unwrap_or("");

        // Reject "aggregator" matches — nodes whose own bounds already cover
        // most of the screen. Android launchers commonly attach an aggregating
        // content-description to the app-grid root that concatenates every
        // visible app label ("Apps: Calendar, Chrome, 闲鱼, …"). A substring
        // match against that giant container would tap the centre of the
        // screen — whatever app happens to sit there — instead of the
        // intended icon. Returning NOT_FOUND in that case is the right
        // behaviour: the matched text *exists* on screen but only as part of
        // a list, not as a specific tappable target.
        if is_aggregator_bounds(raw_bounds, screen_area) { return Some(()); }

        // Promote to the nearest clickable ancestor when the matched node
        // is a non-clickable label — see promote_to_clickable for why.
        // Filters check the post-promotion (actual tap-target) flags so
        // --clickable still picks up label-inside-button rows.
        let (bounds, rid, cls, flag_str) =
            promote_to_clickable(raw_bounds, raw_rid, raw_cls, raw_flags, ancestors, screen_area);
        if flags.require_clickable && !flag_str.contains('c') { return Some(()); }
        if flags.require_enabled   && !flag_str.contains('e') { return Some(()); }
        if flags.require_visible {
            if !flag_str.contains('v') { return Some(()); }
            if bounds.2 <= bounds.0 || bounds.3 <= bounds.1 { return Some(()); }
        }
        hits.push(Hit {
            text: text.to_string(),
            desc: desc.to_string(),
            cls, rid,
            flags: flag_str,
            bounds,
            priority: pri,
        });
        Some(())
    });
    hits.sort_by_key(|h| h.priority);
    hits
}

/// A node whose bounds cover most of the screen is almost certainly a
/// container, not a specific tap target. 50% of screen area is the
/// threshold — bigger than that and tapping the centre is meaningless.
fn is_aggregator_bounds(b: (i32, i32, i32, i32), screen_area: i64) -> bool {
    if screen_area <= 0 { return false; }
    let (l, t, r, bot) = b;
    let area = ((r - l).max(0) as i64) * ((bot - t).max(0) as i64);
    area * 2 > screen_area  // > 50% of screen
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

fn walk<'a, F>(node: &'a Value, f: &mut F)
where
    F: FnMut(&'a Value, &[&'a Value]) -> Option<()>,
{
    let mut ancestors: Vec<&'a Value> = Vec::new();
    walk_inner(node, &mut ancestors, f);
}

fn walk_inner<'a, F>(node: &'a Value, ancestors: &mut Vec<&'a Value>, f: &mut F)
where
    F: FnMut(&'a Value, &[&'a Value]) -> Option<()>,
{
    if matches!(node, Value::Obj(_)) {
        let _ = f(node, ancestors);
        if let Some(Value::Arr(children)) = obj_get(node, "children") {
            ancestors.push(node);
            for c in children {
                walk_inner(c, ancestors, f);
            }
            ancestors.pop();
        }
    }
}

/// Lift a non-clickable text match onto the nearest clickable ancestor.
///
/// Apps routinely structure tappable rows as `Button > TextView "Sign in"`,
/// where the matched node is the label (flags=ev) but the actual onClick is
/// registered on the parent container (flags=cev). Tapping the label's
/// centre happens to work — Android dispatches the touch up the view tree —
/// but the daemon can't use the faster `node_click id=…` shortcut and the
/// reported flags lie about what got tapped. Walking up to the first
/// clickable ancestor gives both: accurate logs and the click-by-id path.
fn promote_to_clickable<'a>(
    bounds: (i32, i32, i32, i32),
    rid: &str,
    cls: &str,
    flag_str: &str,
    ancestors: &[&'a Value],
    screen_area: i64,
) -> ((i32, i32, i32, i32), String, String, String) {
    if flag_str.contains('c') {
        return (bounds, rid.to_string(), cls.to_string(), flag_str.to_string());
    }
    for anc in ancestors.iter().rev() {
        let aflags = obj_get(anc, "flags").and_then(as_str).unwrap_or("");
        if !aflags.contains('c') { continue; }
        let abounds = match bounds_of(anc) {
            Some(b) => b,
            None => continue,
        };
        if abounds.2 <= abounds.0 || abounds.3 <= abounds.1 { continue; }
        // Same aggregator guard as in find_all: don't promote onto a
        // clickable ancestor that covers the whole screen. Launchers often
        // expose a single big clickable grid container — tapping its centre
        // would land on whatever app sits in the middle instead of the
        // matched label. Keep the matched node's bounds in that case;
        // Android's touch dispatcher bubbles the tap to the parent's
        // onClick anyway.
        if is_aggregator_bounds(abounds, screen_area) { break; }
        let arid = obj_get(anc, "rid").and_then(as_str).unwrap_or("").to_string();
        let acls = obj_get(anc, "cls").and_then(as_str).unwrap_or("").to_string();
        return (abounds, arid, acls, aflags.to_string());
    }
    (bounds, rid.to_string(), cls.to_string(), flag_str.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    /// Regression: `hs tap 闲鱼` used to silently tap whatever sat at the
    /// screen centre (Calendar on the home screen) when the launcher root
    /// carried an aggregating content-description that listed every visible
    /// app label. Substring matching hit the giant root; tapping the centre
    /// of its bounds landed on Calendar.
    ///
    /// The fix rejects any matched node whose bounds cover >50% of the screen
    /// — the matched text exists, but only as part of a list, not as a
    /// specific tappable target. Caller gets NOT_FOUND, which is honest.
    #[test]
    fn rejects_aggregator_match_for_chinese_label() {
        // Screen 1080x2400. Launcher root has a content-description that
        // mentions 闲鱼 alongside other app names — its bounds cover the
        // full screen. There's no per-icon node for 闲鱼 in this dump
        // (simulating the bug condition where the actual icon isn't
        // surfaced in the a11y tree).
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.FrameLayout",
                "rid": "",
                "text": "",
                "desc": "Apps: Calendar, Chrome, 闲鱼, Maps",
                "flags": "cev",
                "bounds": [0, 0, 1080, 2400],
                "children": [
                    {
                        "cls": "android.widget.TextView",
                        "rid": "com.foo:id/calendar",
                        "text": "Calendar",
                        "desc": "",
                        "flags": "cev",
                        "bounds": [100, 1100, 300, 1300],
                        "children": []
                    }
                ]
            }
        }"#).unwrap();

        let flags = ActionFlags::default();
        let hits = find_all(obj_get(&dump, "root").unwrap(), "闲鱼", &flags);
        assert!(hits.is_empty(),
            "aggregator root should not match — would tap screen centre. got: {:?}",
            hits.iter().map(|h| (&h.desc, h.bounds)).collect::<Vec<_>>());
    }

    /// Sanity: a specific small per-icon node still matches normally.
    /// Without this check the aggregator guard would be too aggressive.
    #[test]
    fn still_matches_specific_icon() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.FrameLayout",
                "bounds": [0, 0, 1080, 2400],
                "children": [
                    {
                        "cls": "android.widget.TextView",
                        "rid": "com.foo:id/xianyu",
                        "text": "闲鱼",
                        "flags": "cev",
                        "bounds": [400, 1500, 600, 1700],
                        "children": []
                    }
                ]
            }
        }"#).unwrap();
        let flags = ActionFlags::default();
        let hits = find_all(obj_get(&dump, "root").unwrap(), "闲鱼", &flags);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "闲鱼");
        assert_eq!(hits[0].bounds, (400, 1500, 600, 1700));
    }
}

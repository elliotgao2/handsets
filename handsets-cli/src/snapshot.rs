// `hs snapshot` — dump the active window and print the labels of every
// clickable region in reading order (top → bottom, left → right).
//
// One line per region. Pipe directly into `hs tap "<label>"` to act on
// any entry. A clickable region's "label" is the first non-empty `text` or
// `desc` found in its subtree (the node's own first, then a DFS into
// descendants). Nested clickables under a clickable parent are skipped so
// each visible touch target appears once.

use std::io::{self, Write};

use crate::json::{as_arr, as_num, as_str, obj_get, parse, Value};
use crate::Conn;

struct Region {
    label: String,
    bounds: (i32, i32, i32, i32),
}

pub fn run(conn: &mut Conn) -> io::Result<()> {
    let body = conn.call("dump_active")?;
    let json = std::str::from_utf8(&body)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "dump_active not utf-8"))?;
    if json.starts_with("ERR:") {
        return Err(io::Error::other(format!("dump_active: {json}")));
    }
    let root = parse(json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("json: {e}")))?;
    let tree = obj_get(&root, "root").unwrap_or(&root);

    let mut regions: Vec<Region> = Vec::new();
    collect(tree, &mut regions);

    // Reading order: smaller `top` first; within the same row, smaller `left`
    // first. Using row-bucketing here would be over-engineered — the raw
    // (top, left) sort tracks visual order well enough for tap targets,
    // which rarely straddle row boundaries.
    regions.sort_by_key(|r| (r.bounds.1, r.bounds.0));

    let mut out = io::stdout().lock();
    for r in &regions {
        writeln!(out, "{}", sanitize_label(&r.label))?;
    }
    Ok(())
}

/// Walk the tree. When a clickable node is reached, emit it with its first
/// available label and DO NOT descend into its children — Android's tap is
/// hit-tested against the topmost clickable view, but for snapshot purposes
/// the user wants one entry per visible touch target, not per nested layer.
fn collect(node: &Value, out: &mut Vec<Region>) {
    let flags = obj_get(node, "flags").and_then(as_str).unwrap_or("");
    if flags.contains('c') {
        if let Some(bounds) = bounds_of(node) {
            if let Some(label) = label_of(node) {
                out.push(Region { label, bounds });
            }
        }
        return;
    }
    if let Some(children) = obj_get(node, "children").and_then(as_arr) {
        for c in children {
            collect(c, out);
        }
    }
}

/// First non-empty text or content-description found in the node or any of
/// its descendants. Direct fields beat descendants — a button's own text is
/// preferred over a label gathered from a deeply-nested child.
fn label_of(node: &Value) -> Option<String> {
    if let Some(t) = obj_get(node, "text").and_then(as_str) {
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(d) = obj_get(node, "desc").and_then(as_str) {
        if !d.is_empty() {
            return Some(d.to_string());
        }
    }
    if let Some(children) = obj_get(node, "children").and_then(as_arr) {
        for c in children {
            if let Some(l) = label_of(c) {
                return Some(l);
            }
        }
    }
    None
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

/// Collapse internal newlines/tabs so each region stays on one output line.
fn sanitize_label(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

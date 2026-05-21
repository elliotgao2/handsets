// `hs tap <TEXT>` — dump the active window, find a node whose `text` or
// `desc` matches, tap its bounds center on the same connection.

use std::io;

use crate::json::{as_arr, as_num, as_str, obj_get, parse, Value};
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

#[derive(Debug)]
struct Hit {
    text: String,
    desc: String,
    cls: String,
    flags: String,
    bounds: (i32, i32, i32, i32),
    priority: Priority,
}

pub fn run(conn: &mut Conn, query: &str) -> io::Result<()> {
    let body = conn.call("dump_active")?;
    let json = std::str::from_utf8(&body)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "dump_active not utf-8"))?;
    if json.starts_with("ERR:") {
        return Err(io::Error::other(format!("dump_active: {json}")));
    }
    let root = parse(json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("json: {e}")))?;
    // `dump_active` wraps the tree as { "ts": ..., "root": { ... } }. Drop
    // into the inner node so the walk starts at the actual a11y root.
    let tree = obj_get(&root, "root").unwrap_or(&root);
    let hit = find(tree, query).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no a11y node matched \"{query}\""),
        )
    })?;
    let (l, t, r, b) = hit.bounds;
    let cx = (l + r) / 2;
    let cy = (t + b) / 2;

    let ack = conn.call(&format!("tap x={cx} y={cy}"))?;
    let ack_s = String::from_utf8_lossy(&ack);
    let label = if !hit.text.is_empty() {
        hit.text.as_str()
    } else {
        hit.desc.as_str()
    };
    eprintln!(
        "tapped {label:?} cls={} flags={} bounds=[{},{},{},{}] at ({},{}) → {}",
        hit.cls, hit.flags, l, t, r, b, cx, cy, ack_s
    );
    if ack_s.starts_with("ERR:") {
        return Err(io::Error::other(ack_s.into_owned()));
    }
    Ok(())
}

fn find(root: &Value, query: &str) -> Option<Hit> {
    let q_low = query.to_ascii_lowercase();
    let mut best: Option<Hit> = None;
    walk(root, &mut |node| {
        let bounds = bounds_of(node)?;
        let text = obj_get(node, "text").and_then(as_str).unwrap_or("");
        let desc = obj_get(node, "desc").and_then(as_str).unwrap_or("");
        let pri = classify(text, desc, query, &q_low)?;
        let cls = obj_get(node, "cls").and_then(as_str).unwrap_or("").to_string();
        let flags = obj_get(node, "flags").and_then(as_str).unwrap_or("").to_string();
        let candidate = Hit {
            text: text.to_string(),
            desc: desc.to_string(),
            cls,
            flags,
            bounds,
            priority: pri,
        };
        match &best {
            None => best = Some(candidate),
            Some(b) if candidate.priority < b.priority => best = Some(candidate),
            _ => {}
        }
        Some(())
    });
    best
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

// `hs ui` — human-readable UI tree, walked client-side from the daemon's
// compact JSON dump. Each node prints on one indented line with the most
// useful fields (class short-name, id short-name, text, content-desc,
// flags). Cluttery container nodes still appear so layout is preserved,
// but only the bits a tester needs to spot a selector show up.

use crate::json::Value;
use crate::selector;

/// Flat list of "interactive or content-bearing" nodes — skips pure layout
/// containers (FrameLayout, LinearLayout, ConstraintLayout, etc. with no
/// text/desc and no click/scroll affordance). One node per line, columnar:
/// `@(cx,cy)  [tags]  ShortClass  #shortId  "text" / desc`.
pub(crate) fn render_interactive(dump: &Value) -> String {
    let mut rows: Vec<(String, String, String, String)> = Vec::new();   // (coords, tags, class+id, label)
    for root in collect_roots(dump) {
        collect_interactive(root, &mut rows);
    }
    // Column-align: pad coords to widest, tags to widest.
    let coords_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let tags_w   = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
    let cid_w    = rows.iter().map(|r| r.2.len()).max().unwrap_or(0).min(48);
    let mut out = String::with_capacity(8 * 1024);
    for (coords, tags, cid, label) in rows {
        out.push_str(&format!("{coords:<coords_w$}  {tags:<tags_w$}  {cid:<cid_w$}  {label}\n"));
    }
    out
}

/// `-i` shows only nodes a human can identify by reading the screen:
/// non-empty text OR content-desc. Empty interactive widgets stay in too
/// (an unfilled `EditText` / `SeekBar` etc. still matters for automation).
/// Nameless clickable / scrollable containers are dropped — their
/// children carry the actual labels, and tapping the label coords still
/// triggers the parent's onClick via bubbling.
fn is_interactive(node: &Value) -> bool {
    let text = selector::get_str(node, "text").unwrap_or("");
    let desc = selector::get_str(node, "desc").unwrap_or("");
    if !text.is_empty() || !desc.is_empty() { return true; }

    // Inherent input/widget classes that matter even when empty.
    let cls_full = selector::get_str(node, "cls").unwrap_or("");
    let simple = cls_full.rsplit('.').next().unwrap_or(cls_full);
    matches!(simple,
        "EditText" | "Button" | "ImageButton" | "Switch" | "CheckBox"
        | "RadioButton" | "ToggleButton" | "Spinner" | "SeekBar"
        | "RatingBar" | "WebView"
        | "AutoCompleteTextView" | "MultiAutoCompleteTextView"
        | "DatePicker" | "TimePicker" | "NumberPicker")
}

fn collect_interactive(node: &Value, rows: &mut Vec<(String, String, String, String)>) {
    let cls_full = selector::get_str(node, "cls").unwrap_or("");
    let cls_short = cls_full.rsplit('.').next().unwrap_or(cls_full);
    let id_full = selector::get_str(node, "rid").unwrap_or("");
    let id_short = id_full.rsplit('/').next().unwrap_or(id_full);

    if is_interactive(node) {
        let coords = match selector::bounds(node) {
            Some((x1, y1, x2, y2)) => format!("@({},{})", (x1+x2)/2, (y1+y2)/2),
            None => "@(?,?)".to_string(),
        };
        let flags = selector::get_str(node, "flags").unwrap_or("");
        let mut tags = String::new();
        if flags.contains('c') { push_tag(&mut tags, "click"); }
        if flags.contains('L') { push_tag(&mut tags, "long"); }
        if flags.contains('s') { push_tag(&mut tags, "scroll"); }
        if flags.contains('k') { push_tag(&mut tags, "check"); }
        if flags.contains('K') { push_tag(&mut tags, "checked"); }
        if flags.contains('p') { push_tag(&mut tags, "password"); }
        let cid = if id_short.is_empty() {
            cls_short.to_string()
        } else {
            format!("{cls_short} #{id_short}")
        };
        let text = selector::get_str(node, "text").unwrap_or("");
        let desc = selector::get_str(node, "desc").unwrap_or("");
        let label = if !text.is_empty() {
            truncate_quoted(text, 80)
        } else if !desc.is_empty() {
            format!("desc={}", truncate_quoted(desc, 80))
        } else {
            String::new()
        };
        rows.push((coords, tags, cid, label));
    }

    if let Some(kids) = selector::children(node) {
        for c in kids { collect_interactive(c, rows); }
    }
}

fn push_tag(dst: &mut String, tag: &str) {
    if !dst.is_empty() { dst.push(','); }
    dst.push_str(tag);
}

pub(crate) fn render_human(dump: &Value) -> String {
    let mut out = String::with_capacity(8 * 1024);
    for root in collect_roots(dump) {
        walk(root, 0, &mut out);
    }
    out
}

fn walk(node: &Value, depth: usize, out: &mut String) {
    let cls_full = selector::get_str(node, "cls").unwrap_or("");
    let cls_short = cls_full.rsplit('.').next().unwrap_or(cls_full);
    let id_full = selector::get_str(node, "rid").unwrap_or("");
    let id_short = id_full.rsplit('/').next().unwrap_or(id_full);
    let text = selector::get_str(node, "text").unwrap_or("");
    let desc = selector::get_str(node, "desc").unwrap_or("");
    let flags = selector::get_str(node, "flags").unwrap_or("");

    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}{cls_short}");
    if !id_short.is_empty() {
        line.push_str(" #");
        line.push_str(id_short);
    }
    if !text.is_empty() {
        line.push_str("  ");
        line.push_str(&truncate_quoted(text, 80));
    } else if !desc.is_empty() {
        line.push_str("  desc=");
        line.push_str(&truncate_quoted(desc, 80));
    }

    // Single-letter flag tags: c=clickable, s=scrollable, f=focusable,
    // F=focused, e=enabled (omitted, it's the default), L=long-clickable,
    // k=checkable, K=checked, p=password, S=selected, v=visible.
    let mut tags = String::new();
    for (c, tag) in [
        ('c', "click"),
        ('L', "long"),
        ('s', "scroll"),
        ('f', "focusable"),
        ('F', "focused"),
        ('k', "check"),
        ('K', "checked"),
        ('p', "password"),
        ('S', "selected"),
    ] {
        if flags.contains(c) {
            if !tags.is_empty() { tags.push(','); }
            tags.push_str(tag);
        }
    }
    if !tags.is_empty() {
        line.push_str("  [");
        line.push_str(&tags);
        line.push(']');
    }

    if let Some((x1, y1, x2, y2)) = selector::bounds(node) {
        let cx = (x1 + x2) / 2;
        let cy = (y1 + y2) / 2;
        line.push_str(&format!("  @({cx},{cy})"));
    }

    out.push_str(&line);
    out.push('\n');

    if let Some(kids) = selector::children(node) {
        for c in kids { walk(c, depth + 1, out); }
    }
}

fn truncate_quoted(s: &str, max: usize) -> String {
    let escaped: String = s.chars().map(|c| match c {
        '"'  => "\\\"".to_string(),
        '\n' => "↵".to_string(),
        '\t' => " ".to_string(),
        c    => c.to_string(),
    }).collect();
    if escaped.chars().count() > max {
        let t: String = escaped.chars().take(max).collect();
        format!("\"{t}…\"")
    } else {
        format!("\"{escaped}\"")
    }
}

/// Unwrap the dump envelope (dump_active = `{"root": …}`, dump = `{"windows": [...]}`).
fn collect_roots(v: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    if let Value::Obj(fields) = v {
        for (k, val) in fields {
            if k == "root" {
                out.push(val);
            } else if k == "windows" {
                if let Value::Arr(arr) = val {
                    for w in arr {
                        if let Value::Obj(wf) = w {
                            for (k2, v2) in wf {
                                if k2 == "root" { out.push(v2); }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

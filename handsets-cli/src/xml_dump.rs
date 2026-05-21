// JSON-to-XML transform for the dump tree.
//
// Output format mirrors `uiautomator dump` so existing tools that consume
// uiautomator XML (XPath snippets, parsers, IDE plugins) keep working.
//
//   <?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
//   <hierarchy rotation="0">
//     <node index="0" text="" resource-id="" class="..." package="..."
//           content-desc="" clickable="false" enabled="true"
//           focusable="false" focused="false" scrollable="false"
//           long-clickable="false" password="false" selected="false"
//           checkable="false" checked="false"
//           bounds="[0,0,1440,3120]">
//       ...
//     </node>
//   </hierarchy>

use crate::json::Value;
use crate::selector;

pub(crate) fn render(dump_json: &Value, rotation: i64) -> String {
    let mut out = String::with_capacity(16 * 1024);
    out.push_str("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n");
    out.push_str(&format!("<hierarchy rotation=\"{rotation}\">\n"));

    // The dump_active payload is `{ "root": <node> }`. The dump (all windows)
    // payload is `{ "windows": [ { "id": …, "root": <node> }, … ] }`. We
    // unwrap both shapes into a sequence of root nodes.
    let roots = collect_roots(dump_json);
    for (i, root) in roots.iter().enumerate() {
        emit_node(root, i as i64, 1, &mut out);
    }
    out.push_str("</hierarchy>\n");
    out
}

fn collect_roots(v: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    if let Value::Obj(fields) = v {
        for (k, v) in fields {
            if k == "root" { out.push(v); }
            if k == "windows" {
                if let Value::Arr(arr) = v {
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

fn emit_node(n: &Value, index: i64, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    out.push_str(&pad);
    out.push_str("<node");

    attr(out, "index", &index.to_string());
    attr(out, "text",         selector::get_str(n, "text").unwrap_or(""));
    attr(out, "resource-id",  selector::get_str(n, "rid").unwrap_or(""));
    attr(out, "class",        selector::get_str(n, "cls").unwrap_or(""));
    attr(out, "package",      selector::get_str(n, "pkg").unwrap_or(""));
    attr(out, "content-desc", selector::get_str(n, "desc").unwrap_or(""));

    let flags = selector::get_str(n, "flags").unwrap_or("");
    let f = |c: char| if flags.contains(c) { "true" } else { "false" };
    attr(out, "checkable",      f('k'));
    attr(out, "checked",        f('K'));
    attr(out, "clickable",      f('c'));
    attr(out, "enabled",        f('e'));
    attr(out, "focusable",      f('f'));
    attr(out, "focused",        f('F'));
    attr(out, "scrollable",     f('s'));
    attr(out, "long-clickable", f('L'));
    attr(out, "password",       f('p'));
    attr(out, "selected",       f('S'));

    if let Some((x1, y1, x2, y2)) = selector::bounds(n) {
        attr(out, "bounds", &format!("[{x1},{y1}][{x2},{y2}]"));
    }

    // Optional hint (only present on API 26+ when set).
    if let Some(h) = selector::get_str(n, "hint") {
        if !h.is_empty() { attr(out, "hint", h); }
    }

    let kids = selector::children(n);
    if kids.map_or(true, |k| k.is_empty()) {
        out.push_str(" />\n");
    } else {
        out.push_str(">\n");
        for (i, child) in kids.unwrap().iter().enumerate() {
            emit_node(child, i as i64, depth + 1, out);
        }
        out.push_str(&pad);
        out.push_str("</node>\n");
    }
}

fn attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    for c in value.chars() {
        match c {
            '<'  => out.push_str("&lt;"),
            '>'  => out.push_str("&gt;"),
            '&'  => out.push_str("&amp;"),
            '"'  => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c    => out.push(c),
        }
    }
    out.push('"');
}

// CSS-like selector engine for the daemon's JSON dump tree.
//
// Supported syntax (all client-side — no daemon round-trip per match):
//
//   Tag                  match by class name (simple-name or fully-qualified)
//   *                    any node
//   [attr=value]         exact attribute match
//   [attr~=value]        substring
//   [attr^=value]        prefix
//   [attr$=value]        suffix
//   [attr]               attribute present (non-empty)
//   :flag                node has the given a11y flag (clickable, enabled, …)
//   Tag[a=v][b=v]:flag   AND-combined
//
// Multiple comma-separated selectors run as OR (any matches).
//
// The matcher walks the JSON tree produced by `dump` / `dump_active` (the
// nested {cls, pkg, rid, text, desc, bounds, flags, children} objects).

use crate::json::Value;

#[derive(Debug, Clone)]
pub(crate) struct Selector {
    pub class: ClassMatch,
    pub attrs: Vec<AttrPred>,
    pub flags: Vec<char>,
}

#[derive(Debug, Clone)]
pub(crate) enum ClassMatch {
    Any,
    Exact(String),       // full match, e.g. "android.widget.EditText"
    Simple(String),      // last segment only, e.g. "EditText"
}

#[derive(Debug, Clone)]
pub(crate) struct AttrPred {
    pub key: String,     // normalised to the JSON field name ("rid", "text", …)
    pub op: AttrOp,
    pub val: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttrOp { Eq, Has, Substr, Prefix, Suffix }

impl Selector {
    pub fn parse(input: &str) -> Result<Vec<Selector>, String> {
        let mut out = Vec::new();
        for part in input.split(',') {
            let trimmed = part.trim();
            if trimmed.is_empty() { continue; }
            out.push(parse_one(trimmed)?);
        }
        if out.is_empty() { return Err("empty selector".into()); }
        Ok(out)
    }

    pub fn matches(&self, node: &Value) -> bool {
        // class
        let cls = get_str(node, "cls").unwrap_or("");
        match &self.class {
            ClassMatch::Any => {}
            ClassMatch::Exact(s) => if cls != s { return false; },
            ClassMatch::Simple(s) => {
                let simple = cls.rsplit('.').next().unwrap_or(cls);
                if simple != s { return false; }
            }
        }
        // attributes
        for p in &self.attrs {
            let v = get_str(node, &p.key).unwrap_or("");
            let ok = match p.op {
                AttrOp::Eq     => v == p.val,
                AttrOp::Substr => v.contains(&p.val),
                AttrOp::Prefix => v.starts_with(&p.val),
                AttrOp::Suffix => v.ends_with(&p.val),
                AttrOp::Has    => !v.is_empty(),
            };
            if !ok { return false; }
        }
        // flags (single-letter codes from Traverse.java's encoded string)
        if !self.flags.is_empty() {
            let f = get_str(node, "flags").unwrap_or("");
            for c in &self.flags {
                if !f.contains(*c) { return false; }
            }
        }
        true
    }
}

fn parse_one(s: &str) -> Result<Selector, String> {
    let mut i = 0;
    let bytes = s.as_bytes();
    // 1. Optional tag prefix: ident (.ident)* or '*'
    let (class, advance) = parse_class(s, i)?;
    i += advance;
    let mut attrs = Vec::new();
    let mut flags = Vec::new();

    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '[' {
            let (pred, adv) = parse_attr(&s[i..])?;
            attrs.push(pred);
            i += adv;
        } else if c == ':' {
            let (flag_codes, adv) = parse_pseudo(&s[i..])?;
            for f in flag_codes { flags.push(f); }
            i += adv;
        } else if c.is_whitespace() {
            i += 1;
        } else {
            return Err(format!("unexpected '{c}' at position {i} in selector"));
        }
    }
    Ok(Selector { class, attrs, flags })
}

fn parse_class(s: &str, start: usize) -> Result<(ClassMatch, usize), String> {
    let bytes = s.as_bytes();
    let mut i = start;
    if i >= bytes.len() { return Ok((ClassMatch::Any, 0)); }
    if bytes[i] == b'[' || bytes[i] == b':' { return Ok((ClassMatch::Any, 0)); }
    if bytes[i] == b'*' { return Ok((ClassMatch::Any, 1)); }
    // ident chars: [A-Za-z0-9_.$]
    let begin = i;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'$' { i += 1; }
        else { break; }
    }
    if i == begin {
        return Err(format!("expected class name or '*' at position {start}"));
    }
    let name = &s[begin..i];
    let cls = if name.contains('.') {
        ClassMatch::Exact(name.to_string())
    } else {
        ClassMatch::Simple(name.to_string())
    };
    Ok((cls, i - start))
}

fn parse_attr(s: &str) -> Result<(AttrPred, usize), String> {
    // s starts with '['
    let bytes = s.as_bytes();
    assert_eq!(bytes[0], b'[');
    let close = s.find(']').ok_or("unterminated [attribute]")?;
    let inner = &s[1..close];
    // possible operators: =, ~=, ^=, $=
    let (key, op, val) = if let Some(p) = inner.find("~=") {
        (&inner[..p], AttrOp::Substr, unquote(&inner[p + 2..]))
    } else if let Some(p) = inner.find("^=") {
        (&inner[..p], AttrOp::Prefix, unquote(&inner[p + 2..]))
    } else if let Some(p) = inner.find("$=") {
        (&inner[..p], AttrOp::Suffix, unquote(&inner[p + 2..]))
    } else if let Some(p) = inner.find('=') {
        (&inner[..p], AttrOp::Eq, unquote(&inner[p + 1..]))
    } else {
        (inner, AttrOp::Has, String::new())
    };
    let key = normalise_key(key.trim());
    Ok((AttrPred { key, op, val }, close + 1))
}

/// CSS-like attribute names → JSON field names used by the daemon.
fn normalise_key(k: &str) -> String {
    match k {
        "class"        => "cls".into(),
        "package"      => "pkg".into(),
        "id" | "resource-id" => "rid".into(),
        "content-desc" | "desc" => "desc".into(),
        "hint"         => "hint".into(),
        "text"         => "text".into(),
        "package-name" => "pkg".into(),
        other => other.to_string(),
    }
}

fn parse_pseudo(s: &str) -> Result<(Vec<char>, usize), String> {
    // ":flag" — flag is one of clickable, long-clickable, scrollable, checkable,
    // checked, focusable, focused, enabled, selected, password, visible.
    let bytes = s.as_bytes();
    assert_eq!(bytes[0], b':');
    let mut i = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() || c == b'-' { i += 1; } else { break; }
    }
    let name = &s[1..i];
    let flag = match name {
        "clickable" => 'c',
        "long-clickable" => 'L',
        "scrollable" => 's',
        "checkable" => 'k',
        "checked" => 'K',
        "focusable" => 'f',
        "focused" => 'F',
        "enabled" => 'e',
        "selected" => 'S',
        "password" => 'p',
        "visible" => 'v',
        other => return Err(format!("unknown pseudo-class :{other}")),
    };
    Ok((vec![flag], i))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

// ---------- tree walking ----------

pub(crate) fn get_str<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == key {
                if let Value::Str(s) = v { return Some(s); }
            }
        }
    }
    None
}

pub(crate) fn children(node: &Value) -> Option<&Vec<Value>> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == "children" {
                if let Value::Arr(a) = v { return Some(a); }
            }
        }
    }
    None
}

pub(crate) fn bounds(node: &Value) -> Option<(i64, i64, i64, i64)> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == "bounds" {
                if let Value::Arr(a) = v {
                    if a.len() == 4 {
                        let n = |i| if let Value::Num(n) = a[i] { Some(n) } else { None };
                        return Some((n(0)?, n(1)?, n(2)?, n(3)?));
                    }
                }
            }
        }
    }
    None
}

/// BFS over the JSON tree calling `f` on every node. Stops early when `f`
/// returns false.
pub(crate) fn walk<F: FnMut(&Value) -> bool>(root: &Value, mut f: F) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if !f(n) { return; }
        if let Some(kids) = children(n) {
            // iterate in document order
            for c in kids.iter().rev() { stack.push(c); }
        }
    }
}

/// Find every node in `dump` that matches any of `selectors` (OR).
///
/// `dump` may be the raw daemon payload — i.e. the outer envelope
/// `{ "ts": …, "root": <node> }` (dump_active) or
/// `{ "ts": …, "windows": [{ "root": <node> }, …] }` (dump). We unwrap
/// those envelopes into the real per-window root nodes before walking.
pub(crate) fn find_all<'a>(dump: &'a Value, selectors: &[Selector]) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for root in collect_roots(dump) {
        let mut stack: Vec<&'a Value> = vec![root];
        while let Some(n) = stack.pop() {
            for s in selectors {
                if s.matches(n) { out.push(n); break; }
            }
            if let Some(kids) = children(n) {
                for c in kids.iter().rev() { stack.push(c); }
            }
        }
    }
    out
}

fn collect_roots<'a>(v: &'a Value) -> Vec<&'a Value> {
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
            } else if k == "cls" {
                // The value passed in *is* a node already (rare path —
                // someone passing a sub-tree directly).
                return vec![v];
            }
        }
    }
    out
}

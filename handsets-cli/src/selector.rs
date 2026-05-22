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
//   :has-text("foo")     substring text match (Playwright sugar for [text~=foo])
//   :text-is("foo")      exact text match    (Playwright sugar for [text=foo])
//   :in(SEL)             descendant of any node matching SEL
//   :below(SEL)          top edge ≥ anchor's bottom edge
//   :right-of(SEL)       left edge ≥ anchor's right edge
//   :near(SEL, PX)       centre-to-centre distance ≤ PX
//   Tag[a=v][b=v]:flag   AND-combined
//
// Multiple comma-separated selectors run as OR (any matches). The
// pseudo-class vocabulary intentionally mirrors Playwright's locator API
// (`getByText`, `near()`, `below()`) so muscle memory transfers from web
// test code.
//
// The matcher walks the JSON tree produced by `dump` / `dump_active` (the
// nested {cls, pkg, rid, text, desc, bounds, flags, children} objects).

use crate::json::Value;

#[derive(Debug, Clone)]
pub(crate) struct Selector {
    pub class: ClassMatch,
    pub attrs: Vec<AttrPred>,
    pub flags: Vec<char>,
    /// Relational pseudo-classes that need access to the surrounding tree
    /// (`:in(SEL)`, `:near(SEL, PX)`, `:below(SEL)`, `:right-of(SEL)`).
    /// Evaluated by `MatchCtx::matches` rather than `Selector::matches`.
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone)]
pub(crate) enum Relation {
    In(Vec<Selector>),                       // descendant of any matching ancestor
    Near(Vec<Selector>, i64),                // centre distance ≤ PX
    Below(Vec<Selector>),                    // top edge ≥ anchor's bottom
    RightOf(Vec<Selector>),                  // left edge ≥ anchor's right
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
        for part in split_top_level(input, ',') {
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
    let mut relations: Vec<Relation> = Vec::new();

    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '[' {
            let (pred, adv) = parse_attr(&s[i..])?;
            attrs.push(pred);
            i += adv;
        } else if c == ':' {
            let (psd, adv) = parse_pseudo(&s[i..])?;
            match psd {
                Pseudo::Flag(c) => flags.push(c),
                Pseudo::Rel(r)  => relations.push(r),
                Pseudo::Attr(p) => attrs.push(p),
            }
            i += adv;
        } else if c.is_whitespace() {
            i += 1;
        } else {
            return Err(format!("unexpected '{c}' at position {i} in selector"));
        }
    }
    Ok(Selector { class, attrs, flags, relations })
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

enum Pseudo {
    Flag(char),
    Rel(Relation),
    /// `:has-text("foo")` / `:text-is("foo")` — Playwright-style sugar that
    /// compiles to an `[text~=foo]` / `[text=foo]` attribute predicate so
    /// the matcher path stays unchanged.
    Attr(AttrPred),
}

/// Split `input` on `delim` only at paren-depth 0 / bracket-depth 0 / not
/// inside quotes. Keeps `,` available as the OR separator for top-level
/// selectors while letting `:near(EditText[a="x,y"], 60)` keep its commas.
fn split_top_level(input: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut in_dq = false;
    let mut in_sq = false;
    for c in input.chars() {
        if in_dq {
            buf.push(c);
            if c == '"' { in_dq = false; }
            continue;
        }
        if in_sq {
            buf.push(c);
            if c == '\'' { in_sq = false; }
            continue;
        }
        match c {
            '"'  => { in_dq = true; buf.push(c); }
            '\'' => { in_sq = true; buf.push(c); }
            '('  => { paren += 1; buf.push(c); }
            ')'  => { paren -= 1; buf.push(c); }
            '['  => { bracket += 1; buf.push(c); }
            ']'  => { bracket -= 1; buf.push(c); }
            ch if ch == delim && paren == 0 && bracket == 0 => {
                out.push(std::mem::take(&mut buf));
            }
            ch => buf.push(ch),
        }
    }
    out.push(buf);
    out
}

fn parse_pseudo(s: &str) -> Result<(Pseudo, usize), String> {
    let bytes = s.as_bytes();
    assert_eq!(bytes[0], b':');
    let mut i = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() || c == b'-' { i += 1; } else { break; }
    }
    let name = &s[1..i];
    // Relational pseudo-classes take a `(...)` argument. `:has-text` and
    // `:text-is` also take an argument but compile to attribute predicates,
    // not relations — they're Playwright-flavoured sugar over `[text~=…]`
    // and `[text=…]` so users coming from web automation can keep their
    // muscle memory.
    if bytes.get(i) == Some(&b'(') {
        let close = find_matching_paren(s, i)
            .ok_or_else(|| format!("unterminated :{name}( argument"))?;
        let inner = &s[i + 1..close];
        match name {
            "has-text" => {
                let val = unquote(inner);
                if val.is_empty() {
                    return Err(":has-text needs (\"TEXT\")".into());
                }
                return Ok((
                    Pseudo::Attr(AttrPred { key: "text".into(), op: AttrOp::Substr, val }),
                    close + 1,
                ));
            }
            "text-is" => {
                let val = unquote(inner);
                if val.is_empty() {
                    return Err(":text-is needs (\"TEXT\")".into());
                }
                return Ok((
                    Pseudo::Attr(AttrPred { key: "text".into(), op: AttrOp::Eq, val }),
                    close + 1,
                ));
            }
            _ => {}
        }
        let rel = match name {
            "in"       => Relation::In(Selector::parse(inner)?),
            "below"    => Relation::Below(Selector::parse(inner)?),
            "right-of" => Relation::RightOf(Selector::parse(inner)?),
            "near" => {
                // `:near(SEL, PX)` — split on the last *top-level* comma so
                // SEL can itself contain commas (OR groups, attribute
                // values) without ambiguity.
                let parts = split_top_level(inner, ',');
                if parts.len() < 2 {
                    return Err(":near needs (SELECTOR, PX)".into());
                }
                let px_src = parts.last().unwrap().trim();
                let px: i64 = px_src.parse()
                    .map_err(|_| format!(":near distance must be an int, got '{px_src}'"))?;
                let sel_src = parts[..parts.len() - 1].join(",");
                Relation::Near(Selector::parse(&sel_src)?, px)
            }
            other => return Err(format!("unknown relational pseudo-class :{other}(…)")),
        };
        return Ok((Pseudo::Rel(rel), close + 1));
    }
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
    Ok((Pseudo::Flag(flag), i))
}

/// `s[start]` is `(`. Return the index of the matching `)` allowing one
/// level of nesting (good enough for `:near(EditText[id~=email], 60)`).
fn find_matching_paren(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    debug_assert_eq!(bytes[start], b'(');
    let mut depth = 0i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => { depth -= 1; if depth == 0 { return Some(i); } }
            _ => {}
        }
        i += 1;
    }
    None
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

/// Re-usable context for selector evaluation — caches the per-window roots
/// so relational pseudo-classes (:in / :near / :below / :right-of) can do
/// quick ancestor/anchor lookups without re-walking the envelope each time.
pub(crate) struct MatchCtx<'a> {
    pub roots: Vec<&'a Value>,
    /// Parent map keyed by node pointer-identity (raw `*const Value`),
    /// computed lazily on first relational evaluation.
    parent: std::cell::OnceCell<std::collections::HashMap<usize, &'a Value>>,
}

impl<'a> MatchCtx<'a> {
    pub fn new(dump: &'a Value) -> Self {
        Self {
            roots: collect_roots(dump),
            parent: std::cell::OnceCell::new(),
        }
    }

    /// Build (or fetch) the parent-pointer map needed by `:in()`.
    fn parents(&self) -> &std::collections::HashMap<usize, &'a Value> {
        self.parent.get_or_init(|| {
            let mut map = std::collections::HashMap::new();
            for root in &self.roots {
                build_parents(root, &mut map);
            }
            map
        })
    }

    /// Does `node` satisfy `sel`, including relational pseudo-classes?
    pub fn matches(&self, sel: &Selector, node: &'a Value) -> bool {
        if !sel.matches(node) { return false; }
        for rel in &sel.relations {
            if !self.relation_holds(rel, node) { return false; }
        }
        true
    }

    fn relation_holds(&self, rel: &Relation, node: &'a Value) -> bool {
        match rel {
            Relation::In(sels) => {
                let parents = self.parents();
                let mut cursor = node;
                while let Some(&p) = parents.get(&(cursor as *const Value as usize)) {
                    for s in sels { if self.matches(s, p) { return true; } }
                    cursor = p;
                }
                false
            }
            Relation::Near(sels, px)    => self.has_anchor_within(sels, node, *px),
            Relation::Below(sels)       => self.has_anchor_directional(sels, node, Dir::Below),
            Relation::RightOf(sels)     => self.has_anchor_directional(sels, node, Dir::RightOf),
        }
    }

    fn has_anchor_within(&self, sels: &[Selector], node: &'a Value, px: i64) -> bool {
        let n = match bounds(node) { Some(b) => b, None => return false };
        let cx = (n.0 + n.2) / 2;
        let cy = (n.1 + n.3) / 2;
        let anchors = find_all_with(self, sels);
        for a in anchors {
            if let Some(b) = bounds(a) {
                let acx = (b.0 + b.2) / 2;
                let acy = (b.1 + b.3) / 2;
                let dx = cx - acx;
                let dy = cy - acy;
                if dx * dx + dy * dy <= px * px { return true; }
            }
        }
        false
    }

    fn has_anchor_directional(&self, sels: &[Selector], node: &'a Value, dir: Dir) -> bool {
        let n = match bounds(node) { Some(b) => b, None => return false };
        let anchors = find_all_with(self, sels);
        for a in anchors {
            if let Some(b) = bounds(a) {
                let ok = match dir {
                    Dir::Below   => n.1 >= b.3,   // node top ≥ anchor bottom
                    Dir::RightOf => n.0 >= b.2,   // node left ≥ anchor right
                };
                if ok { return true; }
            }
        }
        false
    }

}

#[derive(Copy, Clone)]
enum Dir { Below, RightOf }

fn build_parents<'a>(
    node: &'a Value,
    map: &mut std::collections::HashMap<usize, &'a Value>,
) {
    if let Some(kids) = children(node) {
        for c in kids {
            map.insert(c as *const Value as usize, node);
            build_parents(c, map);
        }
    }
}

/// Like `find_all` but reuses a pre-built `MatchCtx`. Internal — public
/// callers use `find_all`.
pub(crate) fn find_all_with<'a>(ctx: &MatchCtx<'a>, selectors: &[Selector]) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for &root in &ctx.roots {
        let mut stack: Vec<&'a Value> = vec![root];
        while let Some(n) = stack.pop() {
            for s in selectors {
                if ctx.matches(s, n) { out.push(n); break; }
            }
            if let Some(kids) = children(n) {
                for c in kids.iter().rev() { stack.push(c); }
            }
        }
    }
    out
}

/// Apply `--visible`/`--clickable`/`--enabled` filters in-place on a match
/// list. Cheap and lossless: callers can chain it after `find_all`.
pub(crate) fn apply_filters(matches: &mut Vec<&Value>, f: &crate::flags::ActionFlags) {
    if !f.require_visible && !f.require_clickable && !f.require_enabled { return; }
    matches.retain(|n| {
        let flags = get_str(n, "flags").unwrap_or("");
        if f.require_clickable && !flags.contains('c') { return false; }
        if f.require_enabled   && !flags.contains('e') { return false; }
        if f.require_visible {
            // `:visible` here means "has bounds and at least one of the
            // a11y visibility flags". The daemon emits 'v' for nodes that
            // pass AccessibilityNodeInfo.isVisibleToUser().
            if !flags.contains('v') { return false; }
            match bounds(n) {
                Some((x1, y1, x2, y2)) if x2 > x1 && y2 > y1 => {}
                _ => return false,
            }
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_has_text_to_substring_attr() {
        let sels = Selector::parse("Button:has-text(\"Sign in\")").unwrap();
        assert_eq!(sels.len(), 1);
        let s = &sels[0];
        match &s.class {
            ClassMatch::Simple(n) => assert_eq!(n, "Button"),
            other => panic!("expected Simple(\"Button\"), got {other:?}"),
        }
        assert_eq!(s.attrs.len(), 1);
        assert_eq!(s.attrs[0].key, "text");
        assert_eq!(s.attrs[0].op, AttrOp::Substr);
        assert_eq!(s.attrs[0].val, "Sign in");
    }

    #[test]
    fn parses_text_is_to_eq_attr() {
        let sels = Selector::parse("Button:text-is(\"OK\")").unwrap();
        let s = &sels[0];
        assert_eq!(s.attrs.len(), 1);
        assert_eq!(s.attrs[0].op, AttrOp::Eq);
        assert_eq!(s.attrs[0].val, "OK");
    }

    #[test]
    fn rejects_empty_has_text_argument() {
        let err = Selector::parse("Button:has-text(\"\")").unwrap_err();
        assert!(err.contains("has-text"), "got {err}");
    }

    #[test]
    fn keeps_relational_pseudo_classes_intact() {
        // Ensure adding :has-text / :text-is didn't break the relational path.
        let sels = Selector::parse("EditText:below(TextView[text=Email])").unwrap();
        let s = &sels[0];
        assert_eq!(s.relations.len(), 1);
        match &s.relations[0] {
            Relation::Below(_) => {}
            other => panic!("expected Below(..), got {other:?}"),
        }
    }
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

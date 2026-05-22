// Tiny JSON-line writer for `--json` output.
//
// The CLI never needs to round-trip JSON — we only emit small, schema-
// controlled records like `{"verb":"tap","ok":true,"result":{...}}`. So
// this writer trades flexibility for zero allocations beyond the final
// String: build with `Obj::new().s(...).b(...).n(...)` and call
// `.finish()` to get a single-line JSON string.
//
// The escaper handles the same characters as the json.rs parser
// (`"`, `\`, control bytes 0x00–0x1F via `\u00XX`).

use std::fmt::Write;

pub struct Obj {
    buf: String,
    first: bool,
}

impl Obj {
    pub fn new() -> Self {
        Self { buf: String::from("{"), first: true }
    }

    pub fn s(mut self, key: &str, val: &str) -> Self {
        self.sep(key);
        encode_str(&mut self.buf, val);
        self
    }

    pub fn opt_s(self, key: &str, val: Option<&str>) -> Self {
        match val {
            Some(v) => self.s(key, v),
            None => self,
        }
    }

    pub fn n(mut self, key: &str, val: i64) -> Self {
        self.sep(key);
        let _ = write!(self.buf, "{val}");
        self
    }

    pub fn b(mut self, key: &str, val: bool) -> Self {
        self.sep(key);
        self.buf.push_str(if val { "true" } else { "false" });
        self
    }

    /// Embed a pre-rendered JSON value (object, array, number, etc).
    pub fn raw(mut self, key: &str, raw: &str) -> Self {
        self.sep(key);
        self.buf.push_str(raw);
        self
    }

    pub fn obj(mut self, key: &str, child: Obj) -> Self {
        self.sep(key);
        self.buf.push_str(&child.finish());
        self
    }

    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }

    fn sep(&mut self, key: &str) {
        if !self.first { self.buf.push(','); }
        self.first = false;
        encode_str(&mut self.buf, key);
        self.buf.push(':');
    }
}

pub fn arr_of_str<I: IntoIterator<Item = S>, S: AsRef<str>>(items: I) -> String {
    let mut buf = String::from("[");
    let mut first = true;
    for s in items {
        if !first { buf.push(','); }
        first = false;
        encode_str(&mut buf, s.as_ref());
    }
    buf.push(']');
    buf
}

pub fn encode_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object() {
        assert_eq!(Obj::new().finish(), "{}");
    }

    #[test]
    fn basic_fields() {
        let s = Obj::new()
            .s("verb", "tap")
            .b("ok", true)
            .n("x", 540)
            .finish();
        assert_eq!(s, r#"{"verb":"tap","ok":true,"x":540}"#);
    }

    #[test]
    fn escapes_quotes_and_newlines() {
        let s = Obj::new().s("t", "he said \"hi\"\nbye").finish();
        assert_eq!(s, r#"{"t":"he said \"hi\"\nbye"}"#);
    }

    #[test]
    fn nested_obj() {
        let inner = Obj::new().n("x", 1).n("y", 2);
        let s = Obj::new().s("verb", "tap").obj("result", inner).finish();
        assert_eq!(s, r#"{"verb":"tap","result":{"x":1,"y":2}}"#);
    }
}

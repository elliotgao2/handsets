// Minimal JSON parser, copied from handsets-cli/src/json.rs. The two crates
// keep separate copies on purpose — same reason handsets-viewer doesn't share
// modules with hs: each binary in this workspace owns its own world so
// ratatui's deps don't bleed into the zero-dep `hs` build.

#[derive(Debug)]
#[allow(dead_code)] // Bool/Float aren't inspected by the TUI; we still parse them for completeness.
pub enum Value {
    Null,
    Bool(bool),
    Num(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

pub fn parse(s: &str) -> Result<Value, String> {
    let mut p = Parser { src: s.as_bytes(), pos: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    Ok(v)
}

pub fn get_str<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == key {
                if let Value::Str(s) = v { return Some(s); }
            }
        }
    }
    None
}

pub fn children(node: &Value) -> Option<&Vec<Value>> {
    if let Value::Obj(fields) = node {
        for (k, v) in fields {
            if k == "children" {
                if let Value::Arr(a) = v { return Some(a); }
            }
        }
    }
    None
}

pub fn bounds(node: &Value) -> Option<(i64, i64, i64, i64)> {
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

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> { self.src.get(self.pos).copied() }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r') { self.pos += 1; } else { break; }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        match self.peek() {
            Some(x) if x == c => { self.pos += 1; Ok(()) }
            Some(x) => Err(format!("expected '{}' got '{}' at byte {}", c as char, x as char, self.pos)),
            None => Err(format!("expected '{}' at eof", c as char)),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') | Some(b'f') => self.boolean(),
            Some(b'n') => self.null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected '{}' at byte {}", c as char, self.pos)),
            None => Err("unexpected eof".into()),
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') { self.pos += 1; return Ok(Value::Obj(out)); }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let val = self.value()?;
            out.push((key, val));
            self.skip_ws();
            match self.peek() {
                Some(b',') => { self.pos += 1; }
                Some(b'}') => { self.pos += 1; return Ok(Value::Obj(out)); }
                Some(c) => return Err(format!("expected ',' or '}}' got '{}' at byte {}", c as char, self.pos)),
                None => return Err("eof in object".into()),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') { self.pos += 1; return Ok(Value::Arr(out)); }
        loop {
            let v = self.value()?;
            out.push(v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => { self.pos += 1; }
                Some(b']') => { self.pos += 1; return Ok(Value::Arr(out)); }
                Some(c) => return Err(format!("expected ',' or ']' got '{}' at byte {}", c as char, self.pos)),
                None => return Err("eof in array".into()),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            let c = self.peek().ok_or_else(|| "eof in string".to_string())?;
            self.pos += 1;
            match c {
                b'"' => return Ok(s),
                b'\\' => {
                    let esc = self.peek().ok_or_else(|| "eof in escape".to_string())?;
                    self.pos += 1;
                    match esc {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'b' => s.push('\u{08}'),
                        b'f' => s.push('\u{0c}'),
                        b'u' => {
                            if self.pos + 4 > self.src.len() { return Err("eof in \\u escape".into()); }
                            let hex = std::str::from_utf8(&self.src[self.pos..self.pos + 4])
                                .map_err(|_| "bad \\u escape".to_string())?;
                            let cp = u32::from_str_radix(hex, 16)
                                .map_err(|_| "bad \\u hex".to_string())?;
                            self.pos += 4;
                            if let Some(ch) = char::from_u32(cp) { s.push(ch); }
                        }
                        other => return Err(format!("bad escape \\{}", other as char)),
                    }
                }
                _ if c < 0x80 => s.push(c as char),
                _ => {
                    let start = self.pos - 1;
                    let extra = if c >= 0xF0 { 3 } else if c >= 0xE0 { 2 } else { 1 };
                    let end = (start + 1 + extra).min(self.src.len());
                    self.pos = end;
                    if let Ok(t) = std::str::from_utf8(&self.src[start..end]) { s.push_str(t); }
                }
            }
        }
    }

    fn boolean(&mut self) -> Result<Value, String> {
        if self.src[self.pos..].starts_with(b"true") { self.pos += 4; Ok(Value::Bool(true)) }
        else if self.src[self.pos..].starts_with(b"false") { self.pos += 5; Ok(Value::Bool(false)) }
        else { Err(format!("bad bool at byte {}", self.pos)) }
    }

    fn null(&mut self) -> Result<Value, String> {
        if self.src[self.pos..].starts_with(b"null") { self.pos += 4; Ok(Value::Null) }
        else { Err(format!("bad null at byte {}", self.pos)) }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') { self.pos += 1; }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() { self.pos += 1; } else { break; }
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() { self.pos += 1; } else { break; }
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) { self.pos += 1; }
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() { self.pos += 1; } else { break; }
            }
        }
        let s = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| "bad number utf8".to_string())?;
        if !is_float {
            if let Ok(n) = s.parse::<i64>() { return Ok(Value::Num(n)); }
        }
        if let Ok(f) = s.parse::<f64>() { Ok(Value::Float(f)) }
        else { Err(format!("bad number '{s}' at byte {start}")) }
    }
}

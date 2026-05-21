// Renderer for the daemon's ContentProvider verbs (sms / calls /
// contacts / calendar). The daemon answers in NDJSON — first line is
// the column-name array, then one row-value array per line. We
// either pretty-print as an aligned table (default) or re-emit the
// rows as a single JSON array of objects (`--json`).

use std::io::{self, Write};

use crate::{json, Conn};

pub(crate) struct TypeMap<'a> {
    pub column: &'a str,
    pub map: &'a [(i64, &'a str)],
}

pub(crate) fn run(
    conn: &mut Conn,
    wire: &str,
    json_out: bool,
    type_maps: &[TypeMap],
    col_rename: &[(&str, &str)],
) -> io::Result<()> {
    let body = conn.call(wire)?;
    if body.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&body).into_owned()));
    }
    let text = std::str::from_utf8(&body)
        .map_err(|e| io::Error::other(format!("non-utf8 payload: {e}")))?;

    let mut lines = text.lines();
    let header_line = lines.next().ok_or_else(|| io::Error::other("empty response"))?;
    let mut header = parse_string_array(header_line)?;
    if !col_rename.is_empty() {
        for h in header.iter_mut() {
            if let Some(&(_, to)) = col_rename.iter().find(|(from, _)| *from == h) {
                *h = to.to_string();
            }
        }
    }

    let mut rows: Vec<Vec<json::Value>> = Vec::new();
    for line in lines {
        if line.is_empty() { continue; }
        let v = json::parse(line)
            .map_err(|e| io::Error::other(format!("row parse error: {e}")))?;
        if let json::Value::Arr(a) = v {
            rows.push(a);
        } else {
            return Err(io::Error::other("row payload not an array"));
        }
    }

    if json_out { write_json(&header, &rows) }
    else        { write_table(&header, &rows, type_maps) }
}

fn parse_string_array(s: &str) -> io::Result<Vec<String>> {
    let v = json::parse(s)
        .map_err(|e| io::Error::other(format!("header parse error: {e}")))?;
    match v {
        json::Value::Arr(arr) => arr.into_iter().map(|x| match x {
            json::Value::Str(s) => Ok(s),
            _ => Err(io::Error::other("header entry not a string")),
        }).collect(),
        _ => Err(io::Error::other("header not an array")),
    }
}

// ---------- JSON output ----------

fn write_json(header: &[String], rows: &[Vec<json::Value>]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(b"[")?;
    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 { out.write_all(b",")?; }
        out.write_all(b"{")?;
        for (ci, val) in row.iter().enumerate() {
            if ci > 0 { out.write_all(b",")?; }
            let key = header.get(ci).map(String::as_str).unwrap_or("?");
            out.write_all(b"\"")?;
            write_escaped(&mut out, key)?;
            out.write_all(b"\":")?;
            write_value(&mut out, val)?;
        }
        out.write_all(b"}")?;
    }
    out.write_all(b"]\n")?;
    out.flush()
}

fn write_value(w: &mut impl Write, v: &json::Value) -> io::Result<()> {
    match v {
        json::Value::Null => w.write_all(b"null"),
        json::Value::Bool(b) => write!(w, "{b}"),
        json::Value::Num(n) => write!(w, "{n}"),
        json::Value::Str(s) => {
            w.write_all(b"\"")?;
            write_escaped(w, s)?;
            w.write_all(b"\"")
        }
        _ => w.write_all(b"null"),
    }
}

fn write_escaped(w: &mut impl Write, s: &str) -> io::Result<()> {
    for c in s.chars() {
        match c {
            '"'  => w.write_all(b"\\\"")?,
            '\\' => w.write_all(b"\\\\")?,
            '\n' => w.write_all(b"\\n")?,
            '\r' => w.write_all(b"\\r")?,
            '\t' => w.write_all(b"\\t")?,
            c if (c as u32) < 0x20 => write!(w, "\\u{:04x}", c as u32)?,
            c => write!(w, "{c}")?,
        }
    }
    Ok(())
}

// ---------- Table output ----------

fn write_table(
    header: &[String],
    rows: &[Vec<json::Value>],
    type_maps: &[TypeMap],
) -> io::Result<()> {
    let n_cols = header.len();
    let mut grid: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
    grid.push(header.to_vec());
    for row in rows {
        let mut line = Vec::with_capacity(n_cols);
        for ci in 0..n_cols {
            let col = header.get(ci).map(String::as_str).unwrap_or("");
            let cell = row.get(ci).map(|v| format_cell(col, v, type_maps))
                .unwrap_or_default();
            line.push(cell);
        }
        grid.push(line);
    }

    // Column widths — capped per column so a long SMS body doesn't push
    // every other column off-screen.
    const COL_CAP: usize = 60;
    let mut widths = vec![0_usize; n_cols];
    for row in &grid {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count().min(COL_CAP));
        }
    }

    let mut out = io::stdout().lock();
    for row in &grid {
        for (i, cell) in row.iter().enumerate() {
            let s = truncate_chars(cell, widths[i]);
            if i + 1 < row.len() {
                write!(out, "{s:<width$}  ", width = widths[i])?;
            } else {
                write!(out, "{s}")?;
            }
        }
        writeln!(out)?;
    }
    out.flush()
}

fn format_cell(col: &str, v: &json::Value, type_maps: &[TypeMap]) -> String {
    match v {
        json::Value::Null => String::new(),
        json::Value::Bool(b) => b.to_string(),
        json::Value::Str(s) => {
            // Some integer-valued columns (Phone.TYPE = data2 etc.)
            // come back as strings because the underlying SQLite
            // column is TEXT. Apply the type-map if one matches.
            if let Some(label) = lookup_type(col, s.parse::<i64>().ok(), type_maps) {
                return label.to_string();
            }
            s.replace('\n', " ").replace('\r', " ")
        }
        json::Value::Num(n) => {
            if is_date_col(col) && *n > 0 {
                return fmt_unix_ms(*n);
            }
            if let Some(label) = lookup_type(col, Some(*n), type_maps) {
                return label.to_string();
            }
            n.to_string()
        }
        _ => "?".into(),
    }
}

fn lookup_type<'a>(col: &str, n: Option<i64>, type_maps: &'a [TypeMap]) -> Option<&'a str> {
    let n = n?;
    for tm in type_maps {
        if tm.column == col {
            if let Some(&(_, label)) = tm.map.iter().find(|(c, _)| *c == n) {
                return Some(label);
            }
        }
    }
    None
}

fn is_date_col(c: &str) -> bool {
    matches!(c, "date" | "begin" | "end" | "last_time_contacted")
}

fn truncate_chars(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count <= w { return s.to_string(); }
    if w == 0 { return String::new(); }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `unix-ms → "YYYY-MM-DD HH:MM:SS"` in UTC. Pure std, no chrono.
fn fmt_unix_ms(ms: i64) -> String {
    let secs = ms / 1000;
    let day_s = 86_400_i64;
    let days = secs.div_euclid(day_s);
    let secs_of_day = secs.rem_euclid(day_s);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}:{second:02}")
}

/// Howard Hinnant's algorithm — days since the Unix epoch (1970-01-01)
/// → proleptic Gregorian (year, month [1-12], day [1-31]).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

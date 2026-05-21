// `hs screen` — dump the active window and render the layout as a text
// grid in the terminal. Aspect-preserving fit; every labeled node has its
// text/desc placed centered within its bounds, and the whole render is
// wrapped in a rounded-corner device bezel. Plain text — no ANSI styling.
//
// For a *live* mirror of the device screen, use `hs mirror` instead;
// `screen` is a one-shot snapshot bounded by the on-device a11y traversal
// latency (~150 ms).

use std::io::{self, Write};

use crate::json::{as_arr, as_num, as_str, obj_get, parse, Value};
use crate::term::term_size;
use crate::Conn;

/// Glyph cell aspect ratio (height / width). Terminals render monospace
/// glyphs roughly twice as tall as wide; using 2.0 makes a 100×100 square in
/// device pixels render as ~100×50 in cells, which matches what the eye
/// sees on the device.
const CELL_ASPECT: f64 = 2.0;

/// Marker placed in the trailing cell of a 2-cell wide glyph so the printer
/// skips it (the terminal itself advances the cursor past wide chars).
const SENTINEL: char = '\u{0001}';

struct Item {
    bounds: (i32, i32, i32, i32),
    label: Option<String>,
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

    // Device coordinate range comes from the root's own bounds — works even
    // if the active window doesn't cover the whole display.
    let dev = bounds_of(tree).unwrap_or((0, 0, 1440, 3120));
    let dev_w = (dev.2 - dev.0).max(1);
    let dev_h = (dev.3 - dev.1).max(1);

    let (t_cols, t_rows) = term_size().unwrap_or((80, 24));
    // Reserve one row at the bottom for the shell prompt after the render.
    let avail_rows = t_rows.saturating_sub(1).max(3);
    // The bezel eats one cell on each side; everything device-related is
    // rendered into the *interior* and then wrapped after.
    let inner_cols_budget = t_cols.saturating_sub(2).max(4) as u32;
    let inner_rows_budget = avail_rows.saturating_sub(2).max(3) as u32;
    let (inner_cols, inner_rows) = fit_aspect(
        dev_w as u32,
        dev_h as u32,
        inner_cols_budget,
        inner_rows_budget,
    );
    let inner_cols = inner_cols as usize;
    let inner_rows = inner_rows as usize;

    // Outer grid = bezel + interior. We paint the interior into the inner
    // region, then stamp the rounded-corner bezel around it.
    let total_cols = inner_cols + 2;
    let total_rows = inner_rows + 2;
    let mut grid: Vec<Vec<char>> = vec![vec![' '; total_cols]; total_rows];

    let mut items: Vec<Item> = Vec::new();
    collect_all(tree, &mut items);

    // Draw larger regions first so their labels are overwritten by any
    // smaller children that follow. Stable on equal area keeps DFS order
    // as a tie-breaker.
    items.sort_by_key(|it| {
        let w = (it.bounds.2 - it.bounds.0).max(0) as i64;
        let h = (it.bounds.3 - it.bounds.1).max(0) as i64;
        -(w * h)
    });

    for it in &items {
        // Only labels are rendered now — no per-clickable inner frames.
        // The "device-like" look comes from the outer bezel below.
        if let Some(label) = &it.label {
            let m = map_bounds(it.bounds, dev_w, dev_h, inner_cols as i32, inner_rows as i32);
            // Offset the mapped bounds by (+1, +1) so the interior sits
            // inside the bezel without overprinting it.
            let m = (m.0 + 1, m.1 + 1, m.2 + 1, m.3 + 1);
            place_centered(&mut grid, m, label);
        }
    }

    draw_bezel(&mut grid);

    let mut out = io::stdout().lock();
    for row in &grid {
        let s: String = row.iter().filter(|&&c| c != SENTINEL).collect();
        let trimmed = s.trim_end();
        out.write_all(trimmed.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Rounded-corner device bezel around the outermost row/col of the grid.
fn draw_bezel(grid: &mut [Vec<char>]) {
    if grid.is_empty() {
        return;
    }
    let rows = grid.len();
    let cols = grid[0].len();
    if rows < 2 || cols < 2 {
        return;
    }
    let last_col = cols - 1;
    let last_row = rows - 1;
    grid[0][0] = '╭';
    grid[0][last_col] = '╮';
    grid[last_row][0] = '╰';
    grid[last_row][last_col] = '╯';
    for c in 1..last_col {
        grid[0][c] = '─';
        grid[last_row][c] = '─';
    }
    for r in 1..last_row {
        grid[r][0] = '│';
        grid[r][last_col] = '│';
    }
}

// ---------- fit / map ----------

fn fit_aspect(dev_w: u32, dev_h: u32, t_cols: u32, t_rows: u32) -> (u32, u32) {
    // Effective device aspect ratio in *cells* (after correcting for the
    // glyph cell being CELL_ASPECT× as tall as wide).
    let aspect_cells = dev_w as f64 * CELL_ASPECT / dev_h as f64;
    // Width that fits the available height; pick the smaller of that and
    // the terminal's own column count.
    let cols_from_h = (t_rows as f64 * aspect_cells) as u32;
    let cols = cols_from_h.min(t_cols).max(8);
    let rows = ((cols as f64 / aspect_cells) as u32).max(4);
    (cols, rows)
}

fn map_bounds(
    b: (i32, i32, i32, i32),
    dev_w: i32,
    dev_h: i32,
    cols: i32,
    rows: i32,
) -> (i32, i32, i32, i32) {
    let sx = cols as f64 / dev_w as f64;
    let sy = rows as f64 / dev_h as f64;
    (
        (b.0 as f64 * sx).round().clamp(0.0, cols as f64) as i32,
        (b.1 as f64 * sy).round().clamp(0.0, rows as f64) as i32,
        (b.2 as f64 * sx).round().clamp(0.0, cols as f64) as i32,
        (b.3 as f64 * sy).round().clamp(0.0, rows as f64) as i32,
    )
}

// ---------- drawing ----------

fn place_centered(grid: &mut [Vec<char>], b: (i32, i32, i32, i32), label: &str) {
    let (cl, ct, cr, cbo) = b;
    if cr <= cl || cbo <= ct {
        return;
    }
    let row = (ct + cbo) / 2;
    let avail = (cr - cl) as usize;
    let label_clean: String = label
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect();
    let label_w: usize = label_clean.chars().map(char_width).sum();
    let pad = avail.saturating_sub(label_w) / 2;
    place_label(grid, cl + pad as i32, row, avail.saturating_sub(pad), &label_clean);
}

fn place_label(grid: &mut [Vec<char>], col: i32, row: i32, max_cells: usize, label: &str) {
    if max_cells == 0 || row < 0 || (row as usize) >= grid.len() {
        return;
    }
    let row_vec = &mut grid[row as usize];
    let row_len = row_vec.len();
    let mut used = 0usize;
    let mut c = col;
    for ch in label.chars() {
        let w = char_width(ch);
        if w == 0 {
            continue;
        }
        if used + w > max_cells {
            break;
        }
        if c >= 0 && (c as usize) < row_len {
            row_vec[c as usize] = ch;
            if w == 2 && ((c + 1) as usize) < row_len {
                row_vec[(c + 1) as usize] = SENTINEL;
            }
        }
        c += w as i32;
        used += w;
    }
}

/// Approximate East Asian Width: 2 for the common CJK / fullwidth ranges,
/// 0 for control chars, 1 for everything else. Good enough for placing
/// labels — doesn't handle every Unicode wide char (emoji presentation
/// sequences, etc.) but covers Chinese/Japanese/Korean / fullwidth ASCII.
fn char_width(c: char) -> usize {
    let n = c as u32;
    if n < 0x20 {
        return 0;
    }
    if (0x1100..=0x115F).contains(&n)        // Hangul Jamo
        || (0x2E80..=0x303E).contains(&n)    // CJK Radicals etc.
        || (0x3041..=0x33FF).contains(&n)    // Hiragana / Katakana / CJK Symbols
        || (0x3400..=0x4DBF).contains(&n)    // CJK Extension A
        || (0x4E00..=0x9FFF).contains(&n)    // CJK Unified Ideographs
        || (0xA000..=0xA4CF).contains(&n)    // Yi
        || (0xAC00..=0xD7A3).contains(&n)    // Hangul Syllables
        || (0xF900..=0xFAFF).contains(&n)    // CJK Compatibility Ideographs
        || (0xFE30..=0xFE4F).contains(&n)    // CJK Compatibility Forms
        || (0xFF00..=0xFF60).contains(&n)    // Fullwidth Latin
        || (0xFFE0..=0xFFE6).contains(&n)    // Fullwidth Signs
    {
        2
    } else {
        1
    }
}

// ---------- tree walk ----------

fn collect_all(node: &Value, out: &mut Vec<Item>) {
    let bounds = bounds_of(node);
    let text = obj_get(node, "text").and_then(as_str).unwrap_or("");
    let desc = obj_get(node, "desc").and_then(as_str).unwrap_or("");
    let label = if !text.is_empty() {
        Some(text.to_string())
    } else if !desc.is_empty() {
        Some(desc.to_string())
    } else {
        None
    };
    if let (Some(b), Some(_)) = (bounds, &label) {
        out.push(Item { bounds: b, label });
    }
    if let Some(children) = obj_get(node, "children").and_then(as_arr) {
        for c in children {
            collect_all(c, out);
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

// Terminal size now comes from `crate::term::term_size`, shared with the
// `mirror` command.

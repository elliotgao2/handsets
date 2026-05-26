// ratatui rendering. One screen, four regions:
//   - title bar     (top, 1 line)
//   - element list  (centre, fills available height)
//   - status line   (one line above the help bar; shows last action / error)
//   - help bar      (bottom, 1 line)
// Plus an optional centred input modal when in `Inputting` mode.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect, Alignment},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Mode};
use crate::model::{Element, Verb};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),       // title
            Constraint::Min(1),          // list
            Constraint::Length(1),       // status
            Constraint::Length(1),       // help
        ])
        .split(area);

    draw_title(f, chunks[0], app);
    draw_list(f, chunks[1], app);
    draw_status(f, chunks[2], app);
    draw_help(f, chunks[3], app);

    if matches!(app.mode, Mode::Inputting { .. }) {
        draw_input_modal(f, area, app);
    }
}

fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let n = app.elements.len();
    let title = format!(
        " hs tui   {}:{}   {} element{} ",
        app.host, app.port, n, if n == 1 { "" } else { "s" }
    );
    let p = Paragraph::new(title).style(Style::default()
        .fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(p, area);
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    if app.elements.is_empty() {
        let p = Paragraph::new("(no interactive elements — press 'r' to refresh)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }

    let verb_w  = app.elements.iter().map(|e| e.verb.as_str().len()).max().unwrap_or(0);
    let class_w = app.elements.iter().map(|e| e.cls_short.len()).max().unwrap_or(0);
    let label_w = app.elements.iter().map(|e| display_width(&e.label).min(40)).max().unwrap_or(0);
    let id_w    = app.elements.iter().map(|e| e.rid_short.len()).max().unwrap_or(0);
    // Fixed coord widths so the comma column stays put even between screens
    // (and even across different elements lists). 4 digits each covers any
    // Android device up to 9999 px on either axis — every phone and most
    // tablets. Larger displays expand the cell locally without disturbing
    // the layout of other rows.
    const CX_W: usize = 4;
    const CY_W: usize = 4;

    let items: Vec<ListItem> = app.elements.iter().enumerate().map(|(i, el)| {
        let spans = format_row(el, verb_w, class_w, label_w, id_w, CX_W, CY_W);
        let mut item = ListItem::new(Line::from(spans));
        if Some(i) == app.cursor {
            item = item.style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
        }
        item
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_symbol("");

    let mut state = ListState::default();
    state.select(app.cursor);
    f.render_stateful_widget(list, area, &mut state);
}

fn format_row(
    el: &Element,
    verb_w: usize, class_w: usize, label_w: usize, id_w: usize,
    cx_w: usize, cy_w: usize,
) -> Vec<Span<'_>> {
    let verb_style = match el.verb {
        Verb::Tap  => Style::default().fg(Color::Green),
        Verb::Fill => Style::default().fg(Color::Yellow),
        Verb::Info => Style::default().fg(Color::DarkGray),
    };

    let label_disp = truncate(&el.label, 40);
    let label_padded = format!("\"{label_disp}\"");

    let id_field = if el.rid_short.is_empty() || el.label == format!("#{}", el.rid_short) {
        String::new()
    } else {
        format!("#{}", el.rid_short)
    };

    let flags_field = compact_flags(&el.flags);

    // Coords first. Moving them to the leftmost column makes alignment
    // trivial — they're not downstream of any label whose East Asian width
    // is 2 columns per character (Chinese / Japanese / Korean) but only
    // counts as 1 in `str::chars().count()`. With coords at col 0 the eye
    // gets a clean vertical column of (cx,cy) regardless of label content.
    let mut spans = vec![
        Span::styled(
            format!("{cx:>cx_w$},{cy:>cy_w$}",
                cx = el.cx, cy = el.cy, cx_w = cx_w, cy_w = cy_w),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled(format!("{:<verb_w$}", el.verb.as_str(), verb_w = verb_w), verb_style),
        Span::raw("  "),
        Span::styled(format!("{:<class_w$}", el.cls_short, class_w = class_w),
            Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(pad_display(&label_padded, label_w + 2),
            Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(format!("{:<id_w$}", id_field, id_w = id_w),
            Style::default().fg(Color::Magenta)),
    ];

    if !flags_field.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("[{flags_field}]"),
            Style::default().fg(Color::Red)));
    }

    spans
}

fn compact_flags(flags: &str) -> String {
    let mut tags = Vec::new();
    for (c, tag) in [
        ('L', "long"), ('s', "scroll"), ('k', "check"),
        ('K', "checked"), ('p', "password"),
    ] {
        if flags.contains(c) { tags.push(tag); }
    }
    tags.join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max { s.to_string() }
    else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn display_width(s: &str) -> usize { s.chars().count() + 2 /* surrounding quotes */ }

fn pad_display(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count >= w { s.to_string() }
    else {
        let pad: String = std::iter::repeat(' ').take(w - count).collect();
        format!("{s}{pad}")
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = match &app.status {
        Some(s) if s.is_error => (s.text.clone(), Style::default().fg(Color::Red)),
        Some(s)               => (s.text.clone(), Style::default().fg(Color::Green)),
        None => (String::new(), Style::default()),
    };
    let p = Paragraph::new(text).style(style);
    f.render_widget(p, area);
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let help = match app.mode {
        Mode::Browse => "↑↓/jk move   PgDn/PgUp/JK swipe   Enter act   ←/Esc back   q quit",
        Mode::Inputting { .. } => "Enter submit   Esc cancel",
    };
    let p = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    f.render_widget(p, area);
}

fn draw_input_modal(f: &mut Frame, area: Rect, app: &App) {
    let Mode::Inputting { buffer, target_label, .. } = &app.mode else { return };

    let w = (area.width as i32 - 8).clamp(30, 80) as u16;
    let h: u16 = 5;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect { x, y, width: w, height: h };

    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" fill: {target_label} "))
        .style(Style::default().fg(Color::Yellow));

    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let p = Paragraph::new(format!("{buffer}▏"))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::White));
    f.render_widget(p, inner);
}

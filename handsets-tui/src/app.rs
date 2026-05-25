// Event loop. Two threads:
//
//   - Main thread       owns the terminal + a "main_conn" socket for issuing
//                       actions (tap, fill, BACK). Spins a 20 fps render loop;
//                       on each tick it drains the watcher channel, handles
//                       any pending key, and redraws.
//
//   - Watcher thread    owns its own daemon socket. Loops calling `dump_active`
//                       at ~10 fps, parses the JSON, and posts a snapshot of
//                       interactive elements to the main thread via mpsc.
//                       This is the video-stream model — we never block the
//                       UI waiting for idle, the latest snapshot is always on
//                       screen, and animations don't freeze anything.
//
// Two daemon sockets keep dump_active off the action socket; otherwise a
// long-tail dump would block a tap and vice versa.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::conn::Conn;
use crate::json;
use crate::model::{self, Element};
use crate::ui;

pub struct App {
    pub host:       String,
    pub port:       u16,
    pub main_conn:  Conn,
    pub watcher_rx: Receiver<WatcherMsg>,

    pub elements:   Vec<Element>,
    pub cursor:     Option<usize>,
    pub mode:       Mode,
    pub status:     Option<Status>,
    pub last_fp:    u64,

    pub should_quit: bool,
}

pub struct Status {
    pub text: String,
    pub is_error: bool,
    pub at: Instant,
}

pub enum Mode {
    Browse,
    Inputting {
        target_label: String,
        target_rid:   String,    // empty when no resource-id
        target_cx:    i32,
        target_cy:    i32,
        buffer:       String,
    },
}

pub enum WatcherMsg {
    Snapshot(Vec<Element>),
    Error(String),
}

impl App {
    pub fn new(host: String, port: u16) -> io::Result<Self> {
        // Open the action socket on the main thread so a connect failure
        // surfaces before we enter the alternate screen.
        let main_conn = Conn::connect(&host, port)?;

        // Watcher gets its own socket; we hand the thread the address so it
        // can reconnect if the daemon restarts.
        let (tx, rx) = mpsc::channel();
        let host_clone = host.clone();
        thread::spawn(move || run_watcher(host_clone, port, tx));

        Ok(Self {
            host, port, main_conn, watcher_rx: rx,
            elements: Vec::new(),
            cursor:   None,
            mode:     Mode::Browse,
            status:   None,
            last_fp:  0,
            should_quit: false,
        })
    }

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        // 50ms tick = 20 fps redraw budget. Watcher fires at ~10 fps so we
        // always have a fresh snapshot to render against.
        const TICK: Duration = Duration::from_millis(50);

        loop {
            self.drain_watcher();
            self.expire_status();
            terminal.draw(|f| ui::draw(f, self))?;
            if self.should_quit { return Ok(()); }

            if event::poll(TICK)? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        self.handle_key(k)?;
                    }
                }
            }
        }
    }

    fn drain_watcher(&mut self) {
        // Drain all pending messages; keep only the latest snapshot. Older
        // snapshots are useless once a newer one is sitting behind them.
        let mut latest: Option<Vec<Element>> = None;
        loop {
            match self.watcher_rx.try_recv() {
                Ok(WatcherMsg::Snapshot(e)) => latest = Some(e),
                Ok(WatcherMsg::Error(msg))  => self.set_status(&format!("dump: {msg}"), true),
                Err(TryRecvError::Empty)    => break,
                Err(TryRecvError::Disconnected) => {
                    self.set_status("watcher disconnected", true);
                    break;
                }
            }
        }
        if let Some(els) = latest {
            let fp = elements_fingerprint(&els);
            let preserve = self.cursor.and_then(|i| self.elements.get(i).map(|e| e.key()));
            self.cursor   = pick_cursor(&els, preserve, self.cursor);
            // The user explicitly asked for the warning to disappear on UI
            // change — same trigger we use for "tap succeeded" affordance.
            if fp != self.last_fp { self.status = None; }
            self.last_fp  = fp;
            self.elements = els;
        }
    }

    fn expire_status(&mut self) {
        if let Some(s) = &self.status {
            if s.at.elapsed() > Duration::from_secs(4) { self.status = None; }
        }
    }

    fn handle_key(&mut self, k: KeyEvent) -> io::Result<()> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }
        match self.mode {
            Mode::Browse        => self.handle_browse_key(k),
            Mode::Inputting { .. } => self.handle_input_key(k),
        }
    }

    fn handle_browse_key(&mut self, k: KeyEvent) -> io::Result<()> {
        match k.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('r') => { /* watcher already auto-refreshes; this just clears status */
                self.status = None;
            }
            KeyCode::Up   | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Char('g') => self.cursor = if self.elements.is_empty() { None } else { Some(0) },
            KeyCode::Char('G') => self.cursor = self.elements.len().checked_sub(1),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.do_back()?,
            KeyCode::Enter     => self.activate_selected()?,
            // Swipe gestures on the device. swipe_dir up = finger moves up the
            // screen → content scrolls forward (advance). swipe_dir down =
            // finger moves down → content scrolls back. dur=400 ms reads as
            // a deliberate drag rather than a fling.
            KeyCode::PageDown | KeyCode::Char('J') =>
                self.dispatch("swipe_dir up dur=400", "swiped up")?,
            KeyCode::PageUp   | KeyCode::Char('K') =>
                self.dispatch("swipe_dir down dur=400", "swiped down")?,
            _ => {}
        }
        Ok(())
    }

    fn handle_input_key(&mut self, k: KeyEvent) -> io::Result<()> {
        let Mode::Inputting { buffer, .. } = &mut self.mode else { return Ok(()); };
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.set_status("cancelled", false);
            }
            KeyCode::Enter     => self.submit_input()?,
            KeyCode::Backspace => { buffer.pop(); }
            KeyCode::Char(c)   => buffer.push(c),
            _ => {}
        }
        Ok(())
    }

    fn move_cursor(&mut self, delta: i32) {
        if self.elements.is_empty() { self.cursor = None; return; }
        let len = self.elements.len() as i32;
        let cur = self.cursor.map(|i| i as i32).unwrap_or(0);
        let next = (cur + delta).rem_euclid(len);
        self.cursor = Some(next as usize);
    }

    fn activate_selected(&mut self) -> io::Result<()> {
        let Some(i) = self.cursor else { return Ok(()); };
        let el = self.elements[i].clone();
        if el.is_fill() {
            self.mode = Mode::Inputting {
                target_label: el.label.clone(),
                target_rid:   el.rid_full.clone(),
                target_cx:    el.cx,
                target_cy:    el.cy,
                buffer:       el.text.clone(),
            };
            self.status = None;
            return Ok(());
        }
        if el.cx < 0 || el.cy < 0 {
            self.set_status(&format!("'{}' has no bounds", el.label), true);
            return Ok(());
        }
        // node_click via AccessibilityAction.ACTION_CLICK when we have an rid
        // (faster, more reliable). Otherwise coordinate-tap — same trick `hs
        // tap "label"` uses; non-clickable TextViews bubble to clickable parents.
        let cmd = if el.is_tap() && !el.rid_full.is_empty() {
            format!("node_click id={}", el.rid_full)
        } else {
            format!("tap x={} y={}", el.cx, el.cy)
        };
        self.dispatch(&cmd, &format!("tapped \"{}\"", el.label))
    }

    fn submit_input(&mut self) -> io::Result<()> {
        let (target_rid, target_label, target_cx, target_cy, buffer) = match &self.mode {
            Mode::Inputting { target_rid, target_label, target_cx, target_cy, buffer } =>
                (target_rid.clone(), target_label.clone(), *target_cx, *target_cy, buffer.clone()),
            _ => return Ok(()),
        };

        // Wire grammar matches handsets-cli main.rs:1241 ("node_set_text {sel}
        // value={text:?}"). `{:?}` quotes + escapes the value the way the
        // daemon's extractKey parser expects.
        if !target_rid.is_empty() {
            let cmd = format!("node_set_text id={target_rid} value={buffer:?}");
            self.dispatch(&cmd, &format!("filled \"{target_label}\""))?;
        } else {
            // No resource-id — tap into the field first, then push raw text.
            let _ = self.main_conn.call_str(&format!("tap x={target_cx} y={target_cy}"));
            self.dispatch(&format!("text {buffer}"), &format!("typed into \"{target_label}\""))?;
        }
        self.mode = Mode::Browse;
        Ok(())
    }

    fn do_back(&mut self) -> io::Result<()> {
        self.dispatch("key BACK", "back")
    }

    /// Fire-and-forget: send the wire command, optionally surface daemon
    /// errors. We don't wait for idle or re-dump here — the watcher thread
    /// is already polling at ~10 fps and will pick up the new screen.
    fn dispatch(&mut self, cmd: &str, ok_status: &str) -> io::Result<()> {
        match self.main_conn.call_str(cmd) {
            Ok(resp) if resp.starts_with("ERR:") => {
                self.set_status(&format!("daemon: {}", resp.trim_end()), true);
            }
            Ok(_) => self.set_status(ok_status, false),
            Err(e) => self.set_status(&format!("send: {e}"), true),
        }
        Ok(())
    }

    fn set_status(&mut self, text: &str, is_error: bool) {
        self.status = Some(Status { text: text.into(), is_error, at: Instant::now() });
    }
}

fn run_watcher(host: String, port: u16, tx: Sender<WatcherMsg>) {
    // Target ~10 fps. dump_active itself usually takes 5-30ms; we sleep the
    // remainder of the 100ms slot. If a dump runs long (heavy app, many
    // nodes) we just emit fewer fps — never blocks the UI thread.
    const FRAME: Duration = Duration::from_millis(100);

    let mut conn = match Conn::connect(&host, port) {
        Ok(c) => c,
        Err(e) => { let _ = tx.send(WatcherMsg::Error(format!("connect: {e}"))); return; }
    };

    loop {
        let start = Instant::now();
        match conn.call_str("dump_active") {
            Ok(body) if body.starts_with("ERR:") => {
                if tx.send(WatcherMsg::Error(body.trim_end().to_string())).is_err() { return; }
            }
            Ok(body) => match json::parse(&body) {
                Ok(v) => {
                    let els = model::parse_dump(&v);
                    if tx.send(WatcherMsg::Snapshot(els)).is_err() { return; }
                }
                Err(e) => {
                    if tx.send(WatcherMsg::Error(format!("parse: {e}"))).is_err() { return; }
                }
            },
            Err(_) => {
                // Daemon died or socket got reset — try to reconnect.
                // Don't spam errors; just back off and retry.
                thread::sleep(Duration::from_millis(500));
                if let Ok(c) = Conn::connect(&host, port) { conn = c; }
            }
        }
        let used = start.elapsed();
        if used < FRAME { thread::sleep(FRAME - used); }
    }
}

/// Cheap content hash of the element list — used to detect "the dump changed"
/// without comparing every field.
fn elements_fingerprint(elements: &[Element]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    elements.len().hash(&mut h);
    for e in elements {
        e.rid_full.hash(&mut h);
        e.label.hash(&mut h);
        e.cx.hash(&mut h);
        e.cy.hash(&mut h);
    }
    h.finish()
}

fn pick_cursor(
    elements: &[Element],
    preserve_key: Option<(String, String, i32, i32)>,
    fallback: Option<usize>,
) -> Option<usize> {
    if elements.is_empty() { return None; }
    if let Some(key) = preserve_key {
        // 1. Exact match (rid + label + position).
        if let Some(i) = elements.iter().position(|e| e.key() == key) {
            return Some(i);
        }
        // 2. Rid-only match. Labels may have changed (e.g. a counter ticked)
        //    but the widget identity is stable. Without this, the cursor
        //    flickers on screens that have any live-updating field.
        let (rid, _lbl, cx, cy) = &key;
        if !rid.is_empty() {
            if let Some(i) = elements.iter().position(|e| &e.rid_full == rid) {
                return Some(i);
            }
        }
        // 3. Coordinate match — for anonymous nodes that don't have a rid
        //    but haven't moved.
        if let Some(i) = elements.iter().position(|e| e.cx == *cx && e.cy == *cy) {
            return Some(i);
        }
    }
    // Cursor entry vanished — clamp to the prior index, or fall back to 0.
    match fallback {
        Some(i) if i < elements.len() => Some(i),
        _ => Some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_el(verb: &str, label: &str, rid: &str, cx: i32, cy: i32) -> Element {
        let rid_short = rid.rsplit('/').next().unwrap_or(rid).to_string();
        Element {
            verb: match verb { "tap" => model::Verb::Tap, "fill" => model::Verb::Fill, _ => model::Verb::Info },
            cls_short: "Stub".into(),
            label: label.into(),
            rid_full: rid.into(),
            rid_short,
            cx, cy,
            text: String::new(),
            flags: "ce".into(),
        }
    }

    #[test]
    fn cursor_preserves_key_across_refresh() {
        let before = vec![
            stub_el("tap",  "Continue", "com.foo:id/continue", 540, 860),
            stub_el("fill", "Email",    "com.foo:id/email",    540, 540),
            stub_el("fill", "Password", "com.foo:id/password", 540, 640),
        ];
        let key = before[1].key();
        let after = vec![
            stub_el("fill", "Email",    "com.foo:id/email",    540, 540),
            stub_el("fill", "Password", "com.foo:id/password", 540, 640),
        ];
        assert_eq!(pick_cursor(&after, Some(key), Some(1)), Some(0));
    }

    #[test]
    fn cursor_falls_back_when_key_missing() {
        let before = vec![stub_el("tap", "OK", "com.foo:id/ok", 100, 100)];
        let key = before[0].key();
        let after = vec![stub_el("tap", "Cancel", "com.foo:id/cancel", 200, 200)];
        assert_eq!(pick_cursor(&after, Some(key), Some(0)), Some(0));
    }

    #[test]
    fn cursor_follows_rid_when_label_changed() {
        // A live counter widget whose label updates between snapshots but
        // whose resource-id stays put. Without rid-only fallback the cursor
        // would snap back to row 0 every frame.
        let before = vec![
            stub_el("tap", "Item A", "com.foo:id/item_a", 100, 100),
            stub_el("-",   "Count: 41", "com.foo:id/counter", 100, 200),
        ];
        let key = before[1].key();
        let after = vec![
            stub_el("tap", "Item A", "com.foo:id/item_a", 100, 100),
            stub_el("-",   "Count: 42", "com.foo:id/counter", 100, 200),
        ];
        assert_eq!(pick_cursor(&after, Some(key), Some(1)), Some(1));
    }
}

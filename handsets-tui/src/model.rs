// Extract the flat list of interactive nodes from a daemon dump.
//
// This is the data-extraction half of handsets-cli/src/ui_dump.rs ported
// over: same `is_interactive` predicate, same column derivation, but
// instead of formatting into one big string we return structured rows
// that the TUI can render and dispatch actions against.

use crate::json::{self, Value};

#[derive(Debug, Clone)]
pub struct Element {
    pub verb:      Verb,        // tap / fill / info
    pub cls_short: String,      // EditText / Button / TextView / …
    pub label:     String,      // text → desc → "#rid", never quoted
    pub rid_full:  String,      // "com.foo:id/email" (empty when missing)
    pub rid_short: String,      // "email"
    pub cx:        i32,         // bounds-centre x
    pub cy:        i32,         // bounds-centre y
    pub text:      String,      // raw text (for EditText prefill, click discriminator)
    pub desc:      String,      // raw content-description (click discriminator fallback)
    pub flags:     String,      // single-letter codes from the daemon
}

impl Element {
    /// Stable identity across re-dumps so the cursor can stick to "the
    /// thing you just acted on" instead of an index that shifted.
    pub fn key(&self) -> (String, String, i32, i32) {
        (self.rid_full.clone(), self.label.clone(), self.cx, self.cy)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_password(&self) -> bool { self.flags.contains('p') }
    pub fn is_fill(&self)     -> bool { matches!(self.verb, Verb::Fill) }
    pub fn is_tap(&self)      -> bool { matches!(self.verb, Verb::Tap) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb { Tap, Fill, Info }

impl Verb {
    pub fn as_str(&self) -> &'static str {
        match self { Verb::Tap => "tap", Verb::Fill => "fill", Verb::Info => "-" }
    }
}

pub fn parse_dump(dump: &Value) -> Vec<Element> {
    let mut out = Vec::new();
    for root in collect_roots(dump) {
        walk(root, &mut out);
    }
    out
}

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

fn walk(node: &Value, out: &mut Vec<Element>) {
    // Off-screen / negative-bounds nodes get skipped — they're noise in the
    // list (can't be tapped; the daemon rejects off-screen taps anyway) and
    // would confuse the cursor on recycled views. Still descend into
    // children: a recycler with degenerate own-bounds can wrap valid rows.
    if is_interactive(node) && has_visible_bounds(node) {
        out.push(build(node));
    }
    if let Some(kids) = json::children(node) {
        for c in kids { walk(c, out); }
    }
}

fn has_visible_bounds(node: &Value) -> bool {
    match json::bounds(node) {
        Some((x1, y1, x2, y2)) => x1 >= 0 && y1 >= 0 && x2 > x1 && y2 > y1,
        None => false,
    }
}

fn is_interactive(node: &Value) -> bool {
    let text = json::get_str(node, "text").unwrap_or("");
    let desc = json::get_str(node, "desc").unwrap_or("");
    if !text.is_empty() || !desc.is_empty() { return true; }

    let cls_full = json::get_str(node, "cls").unwrap_or("");
    let simple = cls_full.rsplit('.').next().unwrap_or(cls_full);
    if matches!(simple,
        "EditText" | "Button" | "ImageButton" | "Switch" | "CheckBox"
        | "RadioButton" | "ToggleButton" | "Spinner" | "SeekBar"
        | "RatingBar" | "WebView"
        | "AutoCompleteTextView" | "MultiAutoCompleteTextView"
        | "DatePicker" | "TimePicker" | "NumberPicker") {
        return true;
    }

    let rid = json::get_str(node, "rid").unwrap_or("");
    let flags = json::get_str(node, "flags").unwrap_or("");
    !rid.is_empty() && flags.contains('c')
}

fn build(node: &Value) -> Element {
    let cls_full = json::get_str(node, "cls").unwrap_or("");
    let cls_short = cls_full.rsplit('.').next().unwrap_or(cls_full).to_string();
    let rid_full  = json::get_str(node, "rid").unwrap_or("").to_string();
    let rid_short = rid_full.rsplit('/').next().unwrap_or(&rid_full).to_string();
    let text_raw  = json::get_str(node, "text").unwrap_or("").to_string();
    let desc_raw  = json::get_str(node, "desc").unwrap_or("").to_string();
    let text  = strip_invisible(&text_raw);
    let desc  = strip_invisible(&desc_raw);
    let flags = json::get_str(node, "flags").unwrap_or("").to_string();

    let (cx, cy) = match json::bounds(node) {
        Some((x1, y1, x2, y2)) => (((x1 + x2) / 2) as i32, ((y1 + y2) / 2) as i32),
        None => (-1, -1),
    };

    let label = if !text.is_empty()       { text.clone() }
                else if !desc.is_empty()  { desc.clone() }
                else if !rid_short.is_empty() { format!("#{rid_short}") }
                else                      { String::new() };

    let verb = if is_input_widget(&cls_short)     { Verb::Fill }
               else if flags.contains('c')        { Verb::Tap }
               else                               { Verb::Info };

    Element { verb, cls_short, label, rid_full, rid_short, cx, cy, text, desc, flags }
}

fn is_input_widget(simple: &str) -> bool {
    matches!(simple, "EditText" | "AutoCompleteTextView" | "MultiAutoCompleteTextView")
}

fn strip_invisible(s: &str) -> String {
    s.chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| match c { '\n' => '↵', '\t' => ' ', c => c })
        .collect()
}

fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}'              |
        '\u{200B}'..='\u{200F}' |
        '\u{2028}'..='\u{202E}' |
        '\u{2060}'..='\u{206F}' |
        '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn login_dump() -> Value {
        json::parse(r#"{
            "root": {
                "cls": "android.widget.LinearLayout",
                "children": [
                    {"cls":"android.widget.EditText","rid":"com.foo:id/email","desc":"Email","flags":"ce","bounds":[0,500,1080,580],"children":[]},
                    {"cls":"android.widget.EditText","rid":"com.foo:id/password","desc":"Password","flags":"cep","bounds":[0,600,1080,680],"children":[]},
                    {"cls":"android.widget.Button","rid":"com.foo:id/continue","text":"Continue","flags":"ce","bounds":[0,820,1080,900],"children":[]}
                ]
            }
        }"#).unwrap()
    }

    #[test]
    fn parses_login_screen_into_three_elements() {
        let els = parse_dump(&login_dump());
        assert_eq!(els.len(), 3);
        assert_eq!(els[0].verb, Verb::Fill);
        assert_eq!(els[0].cls_short, "EditText");
        assert_eq!(els[0].label, "Email");
        assert_eq!(els[0].rid_short, "email");
        assert!(els[1].is_password());
        assert_eq!(els[2].verb, Verb::Tap);
        assert_eq!(els[2].label, "Continue");
    }

    #[test]
    fn drops_offscreen_and_zero_area_nodes() {
        let v = json::parse(r#"{
            "root": {
                "cls": "android.widget.FrameLayout",
                "bounds": [0, 0, 1080, 2400],
                "children": [
                    {"cls":"android.widget.Button","rid":"com.foo:id/ok","text":"OK","flags":"ce","bounds":[100,200,300,400],"children":[]},
                    {"cls":"android.widget.Button","rid":"com.foo:id/scrolled","text":"Off","flags":"ce","bounds":[100,-300,300,-100],"children":[]},
                    {"cls":"android.widget.Button","rid":"com.foo:id/zero","text":"Zero","flags":"ce","bounds":[0,0,0,0],"children":[]}
                ]
            }
        }"#).unwrap();
        let els = parse_dump(&v);
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].label, "OK");
    }

    #[test]
    fn anonymous_clickable_uses_id_as_label() {
        let v = json::parse(r#"{
            "root": {
                "cls": "android.widget.LinearLayout",
                "children": [
                    {"cls":"android.widget.ImageView","rid":"com.foo:id/back","text":"","desc":"","flags":"ce","bounds":[40,200,160,320],"children":[]}
                ]
            }
        }"#).unwrap();
        let els = parse_dump(&v);
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].label, "#back");
        assert_eq!(els[0].verb, Verb::Tap);
    }
}

// `hs init [PATH]` — scaffold a starter script.hs for `hs run`.
//
// Writes a small annotated template demonstrating the common RPA verbs and
// flag patterns (timeouts, retries, --visible, --unique). Refuses to
// overwrite an existing file so authors don't lose work; pass `--force` to
// allow overwrite.

use std::fs;
use std::io;
use std::path::Path;

const TEMPLATE: &str = r#"# hs run starter script — pipe to `hs run -` or save and run `hs run script.hs`.
#
# Each non-comment line is one CLI verb invocation, executed over a single
# warm socket. Failures abort the script unless `set continue-on-error` is
# in effect.

# Session-level defaults: bump every wait/retry budget without repeating
# the flag on every line.
set timeout=8s
set retries=2

# Boot a screen if needed and let it settle before tapping.
wait idle 200ms

# Tap an element by text, requiring a unique visible+clickable match.
tap "Continue" --visible --clickable --unique

# Fill an input field by selector (atomic, no IME keypresses).
type [resource-id=com.example:id/email] "you@example.com"

# Submit the form via IME action and wait for the next screen.
submit
wait "Welcome"

# Capture a screenshot if anything looks off (use `hs see screen.jpg`).
# see /tmp/handsets-after.jpg
"#;

pub fn run(path: Option<&str>) -> io::Result<()> {
    let dest = path.unwrap_or("script.hs");
    if Path::new(dest).exists() {
        return Err(io::Error::other(format!(
            "{dest} already exists; remove it or pick another path"
        )));
    }
    fs::write(dest, TEMPLATE)?;
    eprintln!("wrote {dest} ({} bytes). Run with: hs run {dest}", TEMPLATE.len());
    Ok(())
}

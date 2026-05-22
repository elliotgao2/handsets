// Shared action-flag set for the RPA-friendly verbs (tap, type, find, wait,
// submit, paste, act). Lets every action verb honour the same
// --timeout / --retries / --visible / --clickable / --enabled / --unique /
// --nth contract so RPA authors stop reimplementing the dump-find-filter-
// retry loop inline.
//
// Parsing is intentionally tolerant: unknown flags fall through to the
// caller's positional collector so verb-specific flags (`--3rd`, `--json`,
// `--limit`) still work. Each verb decides which subset of the fields is
// meaningful — `wait` ignores `--unique`, `find` ignores `--retries`, etc.
//
// Global output mode (`--json` / `HS_FORMAT=json`) lives in OutFmt so the
// flag-stripper here doesn't have to thread it through every verb.

use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutFmt {
    Human,
    Json,
}

impl OutFmt {
    pub fn from_env() -> Self {
        match env::var("HS_FORMAT").ok().as_deref() {
            Some("json") | Some("JSON") => OutFmt::Json,
            _ => OutFmt::Human,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionFlags {
    /// Per-attempt wait budget surfaced to wait_for_* commands and used as
    /// the overall retry deadline for action verbs. `None` falls back to
    /// the daemon's hardcoded default.
    pub timeout_ms: Option<u64>,

    /// Extra attempts beyond the first. `--retries 3` means 1 + 3 = 4
    /// total attempts. Defaults to 0 (single attempt, current behaviour).
    pub retries: u32,
    pub retry_delay_ms: u64,

    /// Filter the selector match set. Each true requires the matching
    /// node to be visible / clickable / enabled respectively.
    pub require_visible: bool,
    pub require_clickable: bool,
    pub require_enabled: bool,

    /// Disambiguation. `unique` fails with AMBIGUOUS if >1 node matches.
    /// `nth` picks 1-indexed match (incompatible with `unique`).
    pub require_unique: bool,
    pub nth: Option<usize>,

    /// Force a fresh dump for the next selector lookup. No-op in one-shot
    /// mode (which never caches); meaningful inside `hs run` / `hs shell`.
    pub fresh: bool,

    /// Output format override. `None` lets the global default win.
    pub out_fmt: Option<OutFmt>,
}

impl Default for ActionFlags {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            retries: 0,
            retry_delay_ms: 200,
            require_visible: false,
            require_clickable: false,
            require_enabled: false,
            require_unique: false,
            nth: None,
            fresh: false,
            out_fmt: None,
        }
    }
}

impl ActionFlags {
    /// Extract recognised flags from `rest` into `self`; return the
    /// surviving positional tokens for the caller to interpret.
    pub fn take<'a>(&mut self, rest: &[&'a str]) -> Result<Vec<&'a str>, String> {
        let mut positional = Vec::with_capacity(rest.len());
        let mut i = 0;
        while i < rest.len() {
            let a = rest[i];
            match a {
                "--timeout" => {
                    i += 1;
                    let v = rest.get(i).ok_or("--timeout needs MS")?;
                    self.timeout_ms = Some(parse_ms(v)?);
                }
                "--retries" => {
                    i += 1;
                    let v = rest.get(i).ok_or("--retries needs N")?;
                    self.retries = v.parse().map_err(|_| format!("bad --retries: {v}"))?;
                }
                "--retry-delay" => {
                    i += 1;
                    let v = rest.get(i).ok_or("--retry-delay needs MS")?;
                    self.retry_delay_ms = parse_ms(v)?;
                }
                "--visible"   => self.require_visible = true,
                "--clickable" => self.require_clickable = true,
                "--enabled"   => self.require_enabled = true,
                "--unique"    => self.require_unique = true,
                "--nth" => {
                    i += 1;
                    let v = rest.get(i).ok_or("--nth needs INDEX (1-based)")?;
                    let n: usize = v.parse().map_err(|_| format!("bad --nth: {v}"))?;
                    if n == 0 { return Err("--nth is 1-based".into()); }
                    self.nth = Some(n);
                }
                "--fresh"     => self.fresh = true,
                "--json"      => self.out_fmt = Some(OutFmt::Json),
                "--no-json"   => self.out_fmt = Some(OutFmt::Human),
                other => positional.push(other),
            }
            i += 1;
        }
        if self.require_unique && self.nth.is_some() {
            return Err("--unique and --nth are mutually exclusive".into());
        }
        Ok(positional)
    }

    /// Resolved output format, honouring per-verb override then env default.
    pub fn out(&self, default: OutFmt) -> OutFmt {
        self.out_fmt.unwrap_or(default)
    }

    pub fn total_attempts(&self) -> u32 {
        self.retries.saturating_add(1)
    }
}

/// Parse a `Nms` / `Ns` / plain integer-ms duration.
pub fn parse_ms(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        n.trim().parse::<u64>().map_err(|_| format!("bad duration {s}"))
    } else if let Some(n) = s.strip_suffix('s') {
        n.trim().parse::<u64>().map(|n| n * 1000)
            .map_err(|_| format!("bad duration {s}"))
    } else {
        s.parse::<u64>().map_err(|_| format!("bad duration {s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_flags() {
        let mut f = ActionFlags::default();
        let pos = f.take(&["foo", "--timeout", "500", "bar", "--retries", "3"]).unwrap();
        assert_eq!(pos, vec!["foo", "bar"]);
        assert_eq!(f.timeout_ms, Some(500));
        assert_eq!(f.retries, 3);
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_ms("500").unwrap(), 500);
        assert_eq!(parse_ms("500ms").unwrap(), 500);
        assert_eq!(parse_ms("2s").unwrap(), 2000);
    }

    #[test]
    fn unique_xor_nth() {
        let mut f = ActionFlags::default();
        let err = f.take(&["--unique", "--nth", "2"]).err().unwrap();
        assert!(err.contains("mutually exclusive"));
    }
}

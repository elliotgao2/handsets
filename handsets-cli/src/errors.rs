// Structured error codes parsed from the daemon's `ERR:` response body.
//
// Wire compatibility: the daemon historically emits `ERR:<tail>` where
// `<tail>` is a free-form `dump-failed:NPE:msg` / `timeout` / `unknown-cmd:foo`
// / `pm_path-needs-pkg` / `secure-window:com.foo` string. New daemons may
// prefix `<tail>` with `CODE:` (e.g. `TIMEOUT:wait_for_text:Login`); the
// client honours the explicit prefix when present and falls back to a small
// inference table for older daemons.
//
// Each `ErrCode` maps to a distinct process exit code so unattended scripts
// can branch on `$?` without parsing strings.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrCode {
    NotFound,
    Timeout,
    DaemonError,
    DeviceGone,
    Ambiguous,
    Precondition,
    BadArg,
    SecureWindow,
    UnknownCmd,
    Internal,
}

impl ErrCode {
    pub fn as_short(self) -> &'static str {
        match self {
            ErrCode::NotFound      => "NOT_FOUND",
            ErrCode::Timeout       => "TIMEOUT",
            ErrCode::DaemonError   => "DAEMON_ERROR",
            ErrCode::DeviceGone    => "DEVICE_GONE",
            ErrCode::Ambiguous     => "AMBIGUOUS",
            ErrCode::Precondition  => "PRECONDITION",
            ErrCode::BadArg        => "BAD_ARG",
            ErrCode::SecureWindow  => "SECURE_WINDOW",
            ErrCode::UnknownCmd    => "UNKNOWN_CMD",
            ErrCode::Internal      => "INTERNAL",
        }
    }

    /// Process exit code surfaced to the shell. `0` is reserved for success
    /// and `1` for general/unclassified failures, so structured codes start
    /// at 2.
    pub fn exit_code(self) -> u8 {
        match self {
            ErrCode::NotFound     => 2,
            ErrCode::Timeout      => 3,
            ErrCode::DaemonError  => 4,
            ErrCode::DeviceGone   => 5,
            ErrCode::Ambiguous    => 6,
            ErrCode::Precondition => 7,
            ErrCode::BadArg       => 8,
            ErrCode::SecureWindow => 9,
            ErrCode::UnknownCmd   => 10,
            ErrCode::Internal     => 11,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ErrInfo {
    pub code: ErrCode,
    pub detail: String,    // free-form tail (everything after `ERR:` and any `CODE:` prefix)
}

impl ErrInfo {
    pub fn new(code: ErrCode, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }

    pub fn message(&self) -> String {
        format!("{}: {}", self.code.as_short(), self.detail)
    }
}

impl std::fmt::Display for ErrInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// Parse the daemon's response body. Returns `None` for non-error bodies.
pub fn parse_err(body: &[u8]) -> Option<ErrInfo> {
    if !body.starts_with(b"ERR:") { return None; }
    let s = std::str::from_utf8(body).ok()?.trim();
    let tail = &s[4..];
    if let Some((code, rest)) = split_code_prefix(tail) {
        Some(ErrInfo { code, detail: rest.to_string() })
    } else {
        Some(ErrInfo { code: infer_code(tail), detail: tail.to_string() })
    }
}

/// If `tail` begins with `UPPER_SNAKE:`, return the parsed code and the
/// rest of the string. The all-caps requirement prevents false positives
/// against the legacy `verb-needs-arg` style tails.
fn split_code_prefix(tail: &str) -> Option<(ErrCode, &str)> {
    let colon = tail.find(':')?;
    let head  = &tail[..colon];
    if head.is_empty() { return None; }
    if !head.chars().all(|c| c.is_ascii_uppercase() || c == '_') { return None; }
    let code = match head {
        "NOT_FOUND"      => ErrCode::NotFound,
        "TIMEOUT"        => ErrCode::Timeout,
        "DAEMON_ERROR"   => ErrCode::DaemonError,
        "DEVICE_GONE"    => ErrCode::DeviceGone,
        "AMBIGUOUS"      => ErrCode::Ambiguous,
        "PRECONDITION"   => ErrCode::Precondition,
        "BAD_ARG"        => ErrCode::BadArg,
        "SECURE_WINDOW"  => ErrCode::SecureWindow,
        "UNKNOWN_CMD"    => ErrCode::UnknownCmd,
        "INTERNAL"       => ErrCode::Internal,
        _                => return None,
    };
    Some((code, &tail[colon + 1..]))
}

/// Best-effort inference for legacy daemons that emit free-form tails.
fn infer_code(tail: &str) -> ErrCode {
    // Exact / common timeout patterns from Server.java's wait_for_* paths.
    if tail == "timeout" || tail.starts_with("timeout:")
        || tail.ends_with(":timeout") || tail.contains("-then-timeout")
    {
        return ErrCode::Timeout;
    }
    if tail.starts_with("unknown-cmd:") { return ErrCode::UnknownCmd; }
    if tail.starts_with("secure-window:") { return ErrCode::SecureWindow; }
    if tail.starts_with("bad-")
        || tail.contains("-needs-")
        || tail.starts_with("invalid-")
    {
        return ErrCode::BadArg;
    }
    if tail.contains("not-found") || tail.contains("no-such-") {
        return ErrCode::NotFound;
    }
    if tail.contains("no-focus") || tail.contains("not-focused")
        || tail.starts_with("ime-")
    {
        return ErrCode::Precondition;
    }
    if tail.contains("-failed:") { return ErrCode::Internal; }
    ErrCode::DaemonError
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_code() {
        let e = parse_err(b"ERR:TIMEOUT:wait_for_text:Login").unwrap();
        assert_eq!(e.code, ErrCode::Timeout);
        assert_eq!(e.detail, "wait_for_text:Login");
    }

    #[test]
    fn infers_timeout_from_legacy_tail() {
        assert_eq!(parse_err(b"ERR:timeout").unwrap().code, ErrCode::Timeout);
        assert_eq!(parse_err(b"ERR:tap-then-timeout").unwrap().code, ErrCode::Timeout);
    }

    #[test]
    fn infers_bad_arg_from_needs_pattern() {
        assert_eq!(parse_err(b"ERR:pm_path-needs-pkg").unwrap().code, ErrCode::BadArg);
        assert_eq!(parse_err(b"ERR:bad-length").unwrap().code, ErrCode::BadArg);
    }

    #[test]
    fn infers_unknown_cmd() {
        assert_eq!(parse_err(b"ERR:unknown-cmd:foo").unwrap().code, ErrCode::UnknownCmd);
    }

    #[test]
    fn infers_secure_window() {
        assert_eq!(parse_err(b"ERR:secure-window:com.x").unwrap().code, ErrCode::SecureWindow);
    }

    #[test]
    fn returns_none_for_ok_body() {
        assert!(parse_err(b"ok").is_none());
        assert!(parse_err(b"").is_none());
    }
}

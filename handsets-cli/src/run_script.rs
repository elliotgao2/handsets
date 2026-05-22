// `hs run [SCRIPT|-]` — batch mode that reads CLI-verb lines from a file or
// stdin and executes them over a single warm socket.
//
// Why this exists: every `hs <verb>` invocation in one-shot mode opens a
// fresh TCP socket. ~5–10 ms of churn × 100 chained actions = a measurable
// drag on RPA scripts. Running the verbs inside one session also unlocks
// the `dump_active` cache (see Session::get_dump) and shared `set
// timeout=…` defaults so authors stop repeating the same flags.
//
// Script syntax:
//   # comment lines and blank lines are skipped
//   set timeout=8s           # raise the per-line wait budget
//   set retries=3
//   set retry-delay=250ms
//   set continue-on-error    # don't abort on the first non-zero exit
//   set dump-ttl=300ms       # raise/lower the cached-dump window
//
//   tap "Continue" --visible --unique
//   wait "Welcome"
//
// Any verb the one-shot CLI understands is allowed; the parser shells out
// to the same dispatch table.

use std::fs;
use std::io::{self, BufRead, Write};

use crate::flags::{ActionFlags, OutFmt};
use crate::json_out::Obj;
use crate::session::Session;

pub fn run(host: &str, port: u16, out_fmt: OutFmt, script: Option<&str>, flags: ActionFlags)
    -> io::Result<()>
{
    let source: Box<dyn BufRead> = match script {
        None | Some("-") => Box::new(io::BufReader::new(io::stdin())),
        Some(path) => Box::new(io::BufReader::new(fs::File::open(path)?)),
    };

    let mut sess = Session::connect(host, port, flags, out_fmt)?;
    let mut line_no = 0u32;
    let mut last_err: Option<io::Error> = None;
    for line in source.lines() {
        line_no += 1;
        let line = line?;
        let cmd = line.trim();
        if cmd.is_empty() || cmd.starts_with('#') { continue; }

        if let Some(directive) = cmd.strip_prefix("set ") {
            if let Err(e) = apply_directive(&mut sess, directive.trim()) {
                writeln!(io::stderr(), "hs run: line {line_no}: {e}")?;
                return Err(io::Error::other(e));
            }
            continue;
        }

        let argv = match shlex_split(cmd) {
            Ok(v) => v,
            Err(e) => {
                writeln!(io::stderr(), "hs run: line {line_no}: {e}")?;
                if sess.continue_on_error { continue; }
                return Err(io::Error::other(e));
            }
        };
        if argv.is_empty() { continue; }

        match crate::dispatch_session_verb(&mut sess, &argv) {
            Ok(()) => {}
            Err(e) => {
                emit_line_error(out_fmt, line_no, cmd, &e)?;
                if sess.continue_on_error {
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    if let Some(e) = last_err {
        // continue-on-error preserved every failure; exit with the last
        // error's structured code so the shell still sees a non-zero exit.
        return Err(e);
    }
    Ok(())
}

fn apply_directive(sess: &mut Session, directive: &str) -> Result<(), String> {
    // Forms supported: `key=val`, `key val`, bare `flag`.
    let (key, val) = if let Some((k, v)) = directive.split_once('=') {
        (k.trim(), Some(v.trim()))
    } else if let Some((k, v)) = directive.split_once(char::is_whitespace) {
        (k.trim(), Some(v.trim()))
    } else {
        (directive.trim(), None)
    };
    match key {
        "timeout" => {
            let v = val.ok_or("set timeout needs a value")?;
            sess.defaults.timeout_ms = Some(crate::flags::parse_ms(v)?);
        }
        "retries" => {
            let v = val.ok_or("set retries needs a value")?;
            sess.defaults.retries = v.parse().map_err(|_| format!("bad retries: {v}"))?;
        }
        "retry-delay" => {
            let v = val.ok_or("set retry-delay needs a value")?;
            sess.defaults.retry_delay_ms = crate::flags::parse_ms(v)?;
        }
        "dump-ttl" => {
            let v = val.ok_or("set dump-ttl needs a value")?;
            sess.dump_ttl_ms = crate::flags::parse_ms(v)?;
        }
        "continue-on-error" => {
            sess.continue_on_error = match val {
                None | Some("on") | Some("1") | Some("true") => true,
                Some("off") | Some("0") | Some("false") => false,
                Some(other) => return Err(format!("set continue-on-error: bad value {other}")),
            };
        }
        "json" => {
            sess.default_out = match val {
                None | Some("on") | Some("1") | Some("true") => OutFmt::Json,
                Some("off") | Some("0") | Some("false") => OutFmt::Human,
                Some(other) => return Err(format!("set json: bad value {other}")),
            };
        }
        other => return Err(format!("unknown directive: set {other}")),
    }
    Ok(())
}

fn emit_line_error(out_fmt: OutFmt, line_no: u32, cmd: &str, e: &io::Error) -> io::Result<()> {
    match out_fmt {
        OutFmt::Json => {
            let mut o = io::stdout().lock();
            let line = Obj::new()
                .s("verb", "run")
                .b("ok", false)
                .n("line", line_no as i64)
                .s("cmd", cmd)
                .s("error", &e.to_string())
                .finish();
            writeln!(o, "{line}")?;
            o.flush()
        }
        OutFmt::Human => {
            let mut o = io::stderr().lock();
            writeln!(o, "hs run: line {line_no}: {e}")?;
            o.flush()
        }
    }
}

/// Tiny POSIX-ish word splitter. Recognises `"…"` and `'…'` quoting plus
/// `\<char>` escapes. Doesn't try to be a complete shell — just enough for
/// `tap "Login Button" --visible` and `type [resource-id=foo] "you@x.com"`.
pub fn shlex_split(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut have_token = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' { in_single = false; } else { cur.push(c); }
            continue;
        }
        if in_double {
            if c == '"' { in_double = false; }
            else if c == '\\' {
                if let Some(&nxt) = chars.peek() {
                    if matches!(nxt, '"' | '\\' | '$' | '`') {
                        cur.push(nxt); chars.next();
                    } else {
                        cur.push('\\');
                    }
                }
            } else { cur.push(c); }
            continue;
        }
        match c {
            '\'' => { in_single = true; have_token = true; }
            '"'  => { in_double = true; have_token = true; }
            '\\' => {
                if let Some(nxt) = chars.next() { cur.push(nxt); have_token = true; }
            }
            ' ' | '\t' => {
                if have_token { out.push(std::mem::take(&mut cur)); have_token = false; }
            }
            other => { cur.push(other); have_token = true; }
        }
    }
    if in_single || in_double { return Err("unterminated quote".into()); }
    if have_token { out.push(cur); }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_simple() {
        assert_eq!(shlex_split("tap Login").unwrap(), vec!["tap", "Login"]);
    }
    #[test]
    fn splits_quoted() {
        assert_eq!(
            shlex_split("tap \"Login Button\" --visible").unwrap(),
            vec!["tap", "Login Button", "--visible"],
        );
    }
    #[test]
    fn handles_single_quotes() {
        assert_eq!(
            shlex_split("type 'hello world'").unwrap(),
            vec!["type", "hello world"],
        );
    }
    #[test]
    fn errors_on_unterminated() {
        assert!(shlex_split("tap \"oops").is_err());
    }
}

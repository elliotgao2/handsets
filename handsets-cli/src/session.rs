// `Session` — warm-socket execution context for `hs run`, `hs act`, and any
// other verb that wants to amortise the daemon connection + reuse a cached
// `dump_active` across multiple lookups.
//
// One-shot CLI calls deliberately do *not* go through Session: each
// invocation is a fresh process, and a 150 ms stale dump in a 200 ms-long
// command is a bug waiting to happen. Inside `hs run` it's the opposite —
// chained `tap "A"; tap "B"` on the same screen pays the dump cost twice
// without a cache, and the cache window is short enough that drift is
// negligible in practice.

use std::io;
use std::time::Instant;

use crate::Conn;
use crate::flags::{ActionFlags, OutFmt};

pub struct Session {
    pub conn: Conn,
    /// Default flags applied to every action verb in the session. Per-verb
    /// flags layer on top via `merge_session_defaults`.
    pub defaults: ActionFlags,
    pub default_out: OutFmt,
    /// Cached `dump_active` payload (raw bytes from the daemon, JSON).
    cached_dump: Option<(Instant, Vec<u8>)>,
    /// Cached parsed tree. Lazy.
    cached_tree: Option<crate::json::Value>,
    /// Cache TTL in ms. 0 disables caching.
    pub dump_ttl_ms: u64,
    /// Whether `continue-on-error` is active for hs-run-style loops.
    pub continue_on_error: bool,
    /// Connection coordinates so verbs that need a fresh per-call socket
    /// (`hs cp`, streaming) can still find the daemon without juggling
    /// extra arguments.
    pub peer_host: String,
    pub peer_port: u16,
}

impl Session {
    pub fn connect(
        host: &str,
        port: u16,
        defaults: ActionFlags,
        default_out: OutFmt,
    ) -> io::Result<Self> {
        let conn = Conn::connect(host, port)?;
        Ok(Self {
            conn,
            defaults,
            default_out,
            cached_dump: None,
            cached_tree: None,
            dump_ttl_ms: 150,
            continue_on_error: false,
            peer_host: host.to_string(),
            peer_port: port,
        })
    }

    pub fn peer_host(&self) -> String { self.peer_host.clone() }
    pub fn peer_port(&self) -> u16    { self.peer_port }

    /// Discard cached dump so the next selector lookup re-fetches.
    pub fn invalidate_dump(&mut self) {
        self.cached_dump = None;
        self.cached_tree = None;
    }

    /// Fetch a fresh dump from the daemon. Use sparingly — prefer
    /// `get_dump` so the cache amortises chained selectors.
    pub fn fetch_dump(&mut self) -> io::Result<crate::json::Value> {
        let body = self.conn.call("dump_active")?;
        if let Some(e) = crate::errors::parse_err(&body) {
            return Err(io::Error::other(crate::output::ReportedError {
                verb: "dump_active".into(),
                info: e,
            }));
        }
        let text = std::str::from_utf8(&body)
            .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
        let v = crate::json::parse(text)
            .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
        self.cached_dump = Some((Instant::now(), body));
        self.cached_tree = None; // re-parse on first access
        Ok(v)
    }

    /// Inspect the (freshening if needed) dump without holding a borrow on
    /// `self`. Lets the caller turn around and use `self.conn` to issue
    /// further wire commands without fighting the borrow checker.
    pub fn with_dump<R>(&mut self, f: impl FnOnce(&crate::json::Value) -> R) -> io::Result<R> {
        self.ensure_dump()?;
        Ok(f(self.cached_tree.as_ref().unwrap()))
    }

    fn ensure_dump(&mut self) -> io::Result<()> {
        let fresh = match &self.cached_dump {
            Some((when, _)) => when.elapsed().as_millis() as u64 <= self.dump_ttl_ms,
            None => false,
        };
        if !fresh {
            let _ = self.fetch_dump()?;
        }
        if self.cached_tree.is_none() {
            let (_, body) = self.cached_dump.as_ref().unwrap();
            let text = std::str::from_utf8(body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            self.cached_tree = Some(crate::json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?);
        }
        Ok(())
    }
}

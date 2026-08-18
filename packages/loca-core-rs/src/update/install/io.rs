//! The two streams an install verb writes to, and the two frozen stdout lines.
//!
//! `install/io.ts` exists as its own file for one reason: the split. Human
//! progress goes to **stderr**, and the ONE machine-readable result a caller
//! parses goes to **stdout**, so nothing a verb prints for a person can
//! contaminate what the Client reads back.
//!
//! # Who is on the other end
//!
//! The Client parses `start-service`'s line
//! (`loca-client/src/tauri/src/commands/pairing.rs:245-253`) by taking the LAST
//! non-empty line of stdout and running `serde_json::from_str` over it. Two
//! consequences that are easy to lose in a port and expensive to lose:
//!
//! - a progress line printed on stdout AFTER the JSON becomes the line the
//!   Client parses, and the parse fails; and
//! - the reader keys off `supervisor == "background"`, so the four kind strings
//!   are a wire contract, not an internal name.
//!
//! The frozen shapes ([`start_service_result`], [`mint_code_result`]) live here
//! with their tests rather than beside the verbs that emit them, because the
//! contract is the LINE, and the two verbs that write one are in different
//! files.
//!
//! Those tests assert the parsed KEYS and the object's key count, not the
//! serialised bytes. Key order is not part of the contract, since both readers
//! parse, and pinning it would pin `serde_json`'s map implementation instead,
//! which a feature flag on any crate in the workspace could flip. The key count
//! is what makes the assertions closed rather than open: it catches an added
//! key as well as a missing one.
//!
//! # Why this is a struct with two writers rather than two free functions
//!
//! A test that captures the process's own file descriptors is process-global
//! state in a test binary that runs ~1800 tests on shared threads, which is the
//! class of fixture that already cost this crate a fleet outage once. So
//! [`Streams`] is generic over its two sinks and the tests drive **the same
//! `Io` implementation production runs**, over two buffers instead of the two
//! handles. D-100's question ("if I break the production path, does this go
//! red?") is answered for every line of the split except one.
//!
//! **The limit, stated rather than implied:** [`Streams::std`] binds
//! `stdout()` to `out` and `stderr()` to `err`, and no test here can see that
//! binding. It is the one line of this module a mutation would survive.

use std::io::Write;
use std::sync::Mutex;

/// What an install verb is allowed to print.
///
/// Injected rather than reached for, so a verb's test asserts on both streams
/// separately: "this went to stdout" and "this did not" are different claims
/// and both matter.
pub trait Io: Send + Sync {
    /// A line for a human watching the install. Never parsed.
    fn progress(&self, msg: &str);

    /// The single structured result of a subcommand, as one line.
    fn emit_json(&self, value: &serde_json::Value);
}

/// The `Io` implementation, over whichever pair of sinks it was built with.
pub struct Streams<O: Write + Send, E: Write + Send> {
    out: Mutex<O>,
    err: Mutex<E>,
}

impl<O: Write + Send, E: Write + Send> Streams<O, E> {
    pub fn new(out: O, err: E) -> Self {
        Self {
            out: Mutex::new(out),
            err: Mutex::new(err),
        }
    }
}

impl Streams<std::io::Stdout, std::io::Stderr> {
    /// Production: the process's own two streams.
    pub fn std() -> Self {
        Self::new(std::io::stdout(), std::io::stderr())
    }
}

impl<O: Write + Send, E: Write + Send> Io for Streams<O, E> {
    fn progress(&self, msg: &str) {
        let mut w = self.err.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{msg}");
        let _ = w.flush();
    }

    fn emit_json(&self, value: &serde_json::Value) {
        // `Display` for `Value` is compact, which is `JSON.stringify`. The
        // trailing newline is part of the contract: the Client splits stdout
        // into lines before parsing.
        let mut w = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{value}");
        let _ = w.flush();
    }
}

/// `start-service`'s line: `{"supervisor":"<kind>","pid":<n>|null}`.
///
/// `pid` is present and null for a supervised core rather than omitted, and
/// that is the load-bearing half: it is how the Client knows the core it just
/// started outlives it and must not be killed on quit. A supervised install
/// reporting a pid would have the Client stop the service every time it exits.
pub fn start_service_result(supervisor: &str, pid: Option<u32>) -> serde_json::Value {
    serde_json::json!({ "supervisor": supervisor, "pid": pid })
}

/// `mint-code`'s line: `{"code":"...","url":"...","port":n}` (`install/pair.ts:52-54`).
pub fn mint_code_result(code: &str, url: &str, port: u16) -> serde_json::Value {
    serde_json::json!({ "code": code, "url": url, "port": port })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod testing {
    use super::*;
    use std::sync::Arc;

    /// A sink the test can read back.
    #[derive(Clone, Default)]
    pub struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        pub fn text(&self) -> String {
            let bytes = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
            String::from_utf8_lossy(&bytes).into_owned()
        }

        /// What the Client reads: the last non-empty line
        /// (`pairing.rs:246`).
        pub fn last_line(&self) -> String {
            self.text()
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .to_string()
        }
    }

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The production `Streams`, over two buffers. Only the two arguments
    /// differ from [`Streams::std`]; every line of the impl under test is the
    /// one that ships.
    pub fn recording() -> (Streams<Captured, Captured>, Captured, Captured) {
        let out = Captured::default();
        let err = Captured::default();
        (Streams::new(out.clone(), err.clone()), out, err)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::testing::recording;
    use super::*;

    #[test]
    fn progress_goes_to_the_stream_the_client_does_not_parse() {
        let (io, out, err) = recording();

        io.progress("placing the binary");
        io.progress("registering the service");

        assert_eq!(err.text(), "placing the binary\nregistering the service\n");
        assert_eq!(
            out.text(),
            "",
            "anything on stdout but the one JSON line breaks the Client's parse"
        );
    }

    #[test]
    fn the_result_is_exactly_one_line_on_stdout() {
        let (io, out, err) = recording();

        io.emit_json(&start_service_result("background", Some(4242)));

        let text = out.text();
        assert_eq!(text.lines().count(), 1, "one line, not two and not zero");
        assert!(
            text.ends_with('\n'),
            "the Client splits stdout into lines before parsing, so the newline is the contract"
        );
        assert_eq!(err.text(), "", "the result is not duplicated to stderr");
    }

    #[test]
    fn a_supervised_core_reports_a_null_pid_rather_than_no_pid() {
        // The key being PRESENT and null is how the Client distinguishes
        // "nothing supervises this, stop it when I quit" from "the OS owns it".
        //
        // `get` rather than `v["pid"]` deliberately: indexing a `Value` yields
        // `Null` for a MISSING key too, so the obvious spelling of this
        // assertion cannot tell the two apart, which is the exact distinction
        // the test exists for.
        let (io, out, _err) = recording();

        io.emit_json(&start_service_result("launchd", None));

        let v: serde_json::Value = serde_json::from_str(&out.last_line()).unwrap();
        assert_eq!(
            v.get("supervisor").and_then(|s| s.as_str()),
            Some("launchd")
        );
        assert_eq!(v.get("pid"), Some(&serde_json::Value::Null));
        assert_eq!(v.as_object().unwrap().len(), 2, "exactly the two keys");
    }

    #[test]
    fn a_background_launch_reports_the_pid_the_client_records() {
        let (io, out, _err) = recording();

        io.emit_json(&start_service_result("background", Some(1234)));

        // The Client's own reader, reproduced from pairing.rs:245-253, so the
        // assertion is what the consumer does rather than what this file
        // hopes it does.
        let v: serde_json::Value = serde_json::from_str(&out.last_line()).unwrap();
        assert_eq!(v.get("supervisor").unwrap().as_str().unwrap(), "background");
        assert_eq!(v.get("pid").unwrap().as_u64().unwrap(), 1234);
    }

    #[test]
    fn a_verb_that_prints_progress_around_its_result_still_parses() {
        // The reason the split exists. The Client takes the LAST non-empty line
        // of stdout, so a progress line landing on stdout after the result is
        // the line it parses, and the parse fails. Interleaved deliberately:
        // progress before AND after, which is what a real verb does.
        let (io, out, err) = recording();

        io.progress("resolving the supervisor");
        io.emit_json(&start_service_result("systemd", None));
        io.progress("started systemd user unit loca-core");

        serde_json::from_str::<serde_json::Value>(&out.last_line())
            .expect("the last line of stdout must be the result");
        assert_eq!(out.text().lines().count(), 1);
        assert_eq!(err.text().lines().count(), 2);
    }

    #[test]
    fn the_mint_code_line_carries_every_key_headless_setup_reads() {
        let (io, out, _err) = recording();

        io.emit_json(&mint_code_result(
            "ABCD-EFGH",
            "https://192.168.1.4:8787",
            8787,
        ));

        let v: serde_json::Value = serde_json::from_str(&out.last_line()).unwrap();
        assert_eq!(v.get("code").and_then(|s| s.as_str()), Some("ABCD-EFGH"));
        assert_eq!(
            v.get("url").and_then(|s| s.as_str()),
            Some("https://192.168.1.4:8787")
        );
        assert_eq!(v.get("port").and_then(|s| s.as_u64()), Some(8787));
        assert_eq!(v.as_object().unwrap().len(), 3, "exactly the three keys");
    }
}

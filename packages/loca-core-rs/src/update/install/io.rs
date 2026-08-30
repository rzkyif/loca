//! The two streams an install verb writes to, and the two frozen stdout lines.
//!
//! Human progress goes to **stderr**; the ONE machine-readable result a caller
//! parses goes to **stdout**. The Client parses `start-service`'s line by
//! taking the LAST non-empty line of stdout, so a progress line printed on
//! stdout after the result becomes the line it parses, and the parse fails. It
//! keys off `supervisor == "background"`, so the kind strings are wire contract.
//!
//! [`Streams`] is generic over its two sinks so the tests drive the production
//! impl over two buffers rather than capturing process-global file descriptors.
//! [`Streams::std`]'s stdout/stderr binding is the one line no test can see.

use std::io::Write;
use std::sync::Mutex;

/// What an install verb is allowed to print.
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
        // The trailing newline is contract: the Client splits stdout into lines.
        let mut w = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{value}");
        let _ = w.flush();
    }
}

/// `start-service`'s line: `{"supervisor":"<kind>","pid":<n>|null}`.
///
/// `pid` is present and null for a supervised core rather than omitted: that is
/// how the Client knows the core outlives it and must not be killed on quit.
pub fn start_service_result(supervisor: &str, pid: Option<u32>) -> serde_json::Value {
    serde_json::json!({ "supervisor": supervisor, "pid": pid })
}

/// `mint-code`'s line: `{"code":"...","url":"...","port":n}`.
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

        /// What the Client reads: the last non-empty line.
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

    /// The production `Streams`, over two buffers.
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
        // `get` rather than indexing: `v["pid"]` yields `Null` for a MISSING
        // key too, and present-and-null vs absent is the whole distinction.
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

        let v: serde_json::Value = serde_json::from_str(&out.last_line()).unwrap();
        assert_eq!(v.get("supervisor").unwrap().as_str().unwrap(), "background");
        assert_eq!(v.get("pid").unwrap().as_u64().unwrap(), 1234);
    }

    #[test]
    fn a_verb_that_prints_progress_around_its_result_still_parses() {
        // Progress before AND after the result, which is what a real verb does.
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

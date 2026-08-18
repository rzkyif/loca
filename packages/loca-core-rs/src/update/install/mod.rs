//! The install and service subcommands: the core binary as its own installer.
//!
//! Work package **D13**. `main.ts` dispatches an install verb before booting
//! the server, so every installer front-end (the unified macOS `.pkg`, the
//! Windows NSIS, the Linux `.deb`/`.rpm`) wraps ONE implementation of service
//! registration, secret bootstrap, and pairing instead of re-implementing it.
//!
//! Landed so far: [`proc`] (the command seam), [`supervisor`] (which owns this
//! core's lifetime and holds every service definition as pure text), and
//! [`io`] (the stderr/stdout split and the two frozen result lines). The verbs
//! themselves follow.

pub mod bootstrap;
pub mod cli;
pub mod fetch_verify;
pub mod io;
pub mod pair;
pub mod proc;
pub mod service;
pub mod supervisor;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod reachability {
    //! D-100, asked of this package: is every definition actually WIRED?
    //!
    //! `supervisor.rs` and `io.rs` landed a run before the verbs that use them,
    //! so for a while every function in them was written, tested and reachable
    //! from nothing. That is exactly the state D-100 describes: a slice can be
    //! complete, fully unit-tested, and unreachable, and its own tests cannot
    //! tell, because they call it directly.
    //!
    //! The unit tests here call these functions too, so they would stay green
    //! with every production call site deleted. This test is the one that
    //! would not.
    //!
    //! It reads the PRODUCTION half of each file, truncated at the first
    //! `#[cfg(test)]`, so a call site that exists only in a test does not
    //! count. That distinction is the whole point.

    /// Definitions that must have a production caller, and are not called from
    /// the file that defines them.
    const MUST_BE_CALLED: &[&str] = &[
        // supervisor.rs, the service definitions
        "plist_xml",
        "systemd_unit(",
        "scheduled_task_script",
        "scheduled_task_teardown_script",
        "uninstall_command",
        "encode_pwsh_command",
        "windows_uninstall_key",
        "windows_display_name",
        "service_label",
        "resolve_supervisor",
        ".kind()",
        // io.rs, the two frozen stdout lines
        "start_service_result",
        "mint_code_result",
        // service.rs
        "parse_pid",
    ];

    /// The files that ship. `supervisor.rs` and `io.rs` are excluded on
    /// purpose: a definition calling itself is not a call site.
    const PRODUCTION: &[&str] = &[
        "src/update/install/service.rs",
        "src/update/install/cli.rs",
        "src/update/install/pair.rs",
        "src/update/install/bootstrap.rs",
        "src/update/install/fetch_verify.rs",
    ];

    /// Everything above the first `#[cfg(test)]`, with comment lines dropped.
    ///
    /// Both halves matter. Without the truncation, a function called only from
    /// a test counts as wired. Without dropping comments, a doc comment that
    /// merely NAMES the function counts as calling it, which is how a mention
    /// gets mistaken for a call.
    fn production_source() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut all = String::new();
        for file in PRODUCTION {
            let body = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|err| panic!("{file}: {err}"));
            let shipping = match body.find("#[cfg(test)]") {
                Some(at) => &body[..at],
                None => &body[..],
            };
            for line in shipping.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                all.push_str(line);
                all.push('\n');
            }
        }
        all
    }

    #[test]
    fn every_definition_has_a_production_caller() {
        let source = production_source();
        let orphans: Vec<&str> = MUST_BE_CALLED
            .iter()
            .copied()
            .filter(|name| !source.contains(name))
            .collect();
        assert!(
            orphans.is_empty(),
            "these are written and tested but nothing SHIPPING calls them, \
             which is D-100's shape: {orphans:?}"
        );
    }

    #[test]
    fn the_check_can_fail() {
        // A test about tests failing to catch things has to prove it catches
        // something, or it is the very thing it warns about. A name no file
        // contains must be reported as an orphan.
        let source = production_source();
        assert!(
            !source.contains("a_definition_nothing_calls"),
            "the orphan check would not have detected a real orphan"
        );
    }

    #[test]
    fn the_production_half_is_not_the_whole_file() {
        // The truncation is what makes a test-only call site not count. If
        // `#[cfg(test)]` were ever missed, this module would silently start
        // accepting test call sites as production wiring, and then
        // `every_definition_has_a_production_caller` would pass for a slice
        // nothing ships calls.
        //
        // Asserted against `production_source()` ITSELF rather than against a
        // second truncation written here. The first version of this test
        // re-read and re-truncated the file, which made it a test of its own
        // copy: deleting the real truncation left it green, and a mutation
        // found that. D-084, inside the D-100 machinery, which is the worst
        // place for it.
        let source = production_source();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let whole: usize = PRODUCTION
            .iter()
            .map(|file| std::fs::read_to_string(root.join(file)).unwrap().len())
            .sum();
        assert!(source.len() < whole, "nothing was truncated");
        // Symbols that exist ONLY in the test halves of these files. If any
        // appears, the truncation is not happening.
        for test_only in ["RecordingRunner", "RecordingKey", "tempfile::tempdir"] {
            assert!(
                !source.contains(test_only),
                "the test half leaked into the production half: {test_only}"
            );
        }
    }
}

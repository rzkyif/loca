//! The install and service subcommands: the core binary as its own installer.
//!
//! An install verb is dispatched before the server boots, so every installer
//! front-end (macOS `.pkg`, Windows NSIS, Linux `.deb`/`.rpm`) wraps ONE
//! implementation of service registration, secret bootstrap, and pairing.

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
    //! Is every definition actually wired? A unit test calls its subject
    //! directly, so it stays green with every production call site deleted.
    //! This reads the production half of each file, truncated at the first
    //! `#[cfg(test)]`, so a test-only call site does not count.

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

    /// The files that ship. `supervisor.rs` and `io.rs` are excluded: a
    /// definition calling itself is not a call site.
    const PRODUCTION: &[&str] = &[
        "src/update/install/service.rs",
        "src/update/install/cli.rs",
        "src/update/install/pair.rs",
        "src/update/install/bootstrap.rs",
        "src/update/install/fetch_verify.rs",
    ];

    /// Everything above the first `#[cfg(test)]`, with comment lines dropped
    /// so a doc comment naming a function is not mistaken for calling it.
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
            "written and tested, but nothing shipping calls them: {orphans:?}"
        );
    }

    #[test]
    fn the_check_can_fail() {
        let source = production_source();
        assert!(
            !source.contains("a_definition_nothing_calls"),
            "the orphan check would not have detected a real orphan"
        );
    }

    #[test]
    fn the_production_half_is_not_the_whole_file() {
        // Asserted against `production_source()` itself, not a second
        // truncation written here: that would only test its own copy.
        let source = production_source();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let whole: usize = PRODUCTION
            .iter()
            .map(|file| std::fs::read_to_string(root.join(file)).unwrap().len())
            .sum();
        assert!(source.len() < whole, "nothing was truncated");
        // Symbols that exist ONLY in the test halves of these files.
        for test_only in ["RecordingRunner", "RecordingKey", "tempfile::tempdir"] {
            assert!(
                !source.contains(test_only),
                "the test half leaked into the production half: {test_only}"
            );
        }
    }
}

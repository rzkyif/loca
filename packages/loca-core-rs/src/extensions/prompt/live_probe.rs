//! The drift tripwire for the pinned `deno`.
//!
//! Ports `packages/loca-core/src/extensions/prompt-live-probe.test.ts`. Every
//! other test in this package feeds the parser text somebody typed into a
//! fixture, which proves the parser matches the fixture. This one drives REAL
//! permission prompts out of the real pinned binary, through a real terminal,
//! and answers them on the real schedule, so a Deno release that changes the
//! prompt wording or the answer handshake fails HERE rather than in a user's
//! tool call.
//!
//! # It is skipped by default and must not be silently skipped in CI
//!
//! `#[ignore]`d, because it costs five to fifteen seconds of wall clock (seven
//! prompts, each with a mandatory 300ms delay before the answer) and because
//! the pinned `deno` is a consent-gated download that a contributor may simply
//! not have. Both are fine locally and neither is fine in CI: a tripwire that
//! quietly skips is a test that cannot fail, which is what
//! `LOCA_REQUIRE_DENO_PROMPTS` exists to prevent. Setting it turns "no binary"
//! from a skip into a failure.
//!
//! # Unix only, and what covers Windows instead
//!
//! The probe fixture reads `/etc/hosts` and dlopens a system library, and it
//! expects PTY-shaped prompt output. The ConPTY path has its own local-only
//! live cover; what CAN be tested without Windows (the DSR handshake, the
//! console newline translation) lives in [`crate::extensions::pty`] and runs
//! everywhere.

/// Set to `1` in CI, after the pinned `deno` has been staged. Then an absent
/// binary FAILS instead of skipping.
#[cfg(test)]
const REQUIRE: &str = "LOCA_REQUIRE_DENO_PROMPTS";

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncBufReadExt as _, BufReader};

    use super::REQUIRE;
    use crate::config::Channel;
    use crate::extensions::prompt::parser::{PromptParser, PromptParserEvent};
    use crate::extensions::pty::{self, PtyEvent, PtySpawn};
    use crate::paths::Paths;
    use crate::sidecars::deno::resolve_worker_deno;

    /// One access per permission kind, and the y/n column below drives the
    /// answer. A granted access must RESUME and produce data, which is what
    /// proves pause-and-resume rather than merely that text was parsed; a
    /// denied one must throw `NotCapable` into a script that keeps running.
    const PROBE_SCRIPT: &str = r#"
const out = (o) => console.log(JSON.stringify(o));
async function probe(label, fn) {
  try {
    const v = await fn();
    out({ label, ok: true, value: v });
  } catch (e) {
    out({ label, ok: false, name: e.name });
  }
}
await probe("read", () => Deno.readTextFile("/etc/hosts").then((t) => t.length));
await probe("env", () => Deno.env.get("LANG") ?? "");
await probe("sys", () => Deno.hostname());
await probe("write", () => Deno.writeTextFile("/tmp/loca-live-probe.txt", "x"));
await probe("run", () => new Deno.Command("ls", { args: ["/"] }).output().then((r) => r.code));
await probe("net", () => Deno.connect({ hostname: "127.0.0.1", port: 1 }));
await probe("ffi", () => Deno.dlopen("/usr/lib/libSystem.B.dylib", {}));
out({ label: "end" });
"#;

    /// Which kinds are answered `y`. The three allowed ones verify resume; the
    /// four denied ones verify `NotCapable`.
    const ANSWERS: &[(&str, bool)] = &[
        ("read", true),
        ("env", true),
        ("sys", true),
        ("write", false),
        ("run", false),
        ("net", false),
        ("ffi", false),
    ];

    /// The answer schedule the production answerer uses, restated here because
    /// this test is what proves those numbers still work against a real
    /// prompter. Deno's `clear_stdin` flushes until stdin has been quiescent
    /// for about 100ms BEFORE it reads, so an immediate write is eaten.
    ///
    /// **The two cover each other and neither is individually observable**,
    /// measured against a real `deno` rather than reasoned about: with the
    /// delay at zero the first write IS eaten and the retry lands, and the
    /// probe passes. Only removing both makes it hang, which the budget below
    /// then reports as a failure. So do not delete the delay on the evidence
    /// that nothing fails without it; what it buys is one prompt answered on
    /// the first write instead of the second, seven times per call.
    const ANSWER_INITIAL_DELAY: Duration = Duration::from_millis(300);
    const ANSWER_RETRY: Duration = Duration::from_millis(600);

    /// The whole probe. Seven prompts at 300ms plus retries, one real process
    /// start. Bounded, because a tripwire that hangs is not a tripwire.
    const PROBE_BUDGET: Duration = Duration::from_secs(60);

    fn required() -> bool {
        std::env::var(REQUIRE).as_deref() == Ok("1")
    }

    /// The pinned `deno`, as a running core would resolve it.
    ///
    /// Not `deno` off the `PATH`: this test exists to pin the version the
    /// workers actually run under, and the developer's own `deno` is whatever
    /// they installed.
    fn pinned_deno() -> Option<PathBuf> {
        let channel = Channel::from_env().ok()?;
        let paths = Paths::from_env(channel).ok()?;
        resolve_worker_deno(&paths).ok()
    }

    #[test]
    fn the_pinned_deno_is_present_when_it_is_required() {
        // The whole purpose of this one is to make the skip LOUD. Without it,
        // a CI runner with no staged binary reports a green suite that
        // exercised nothing.
        if !required() {
            return;
        }
        assert!(
            pinned_deno().is_some(),
            "{REQUIRE} is set but no pinned deno was found; stage it under the channel's bin directory",
        );
    }

    #[tokio::test]
    #[ignore = "drives a real deno through a real terminal; run it explicitly or in the tripwire CI step"]
    async fn real_deno_prompts_parse_pause_and_resume() {
        let Some(deno) = pinned_deno() else {
            assert!(!required(), "{REQUIRE} is set but no pinned deno was found",);
            eprintln!("skipping the live probe: no pinned deno");
            return;
        };

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("probe.ts");
        std::fs::write(&script, PROBE_SCRIPT).unwrap();

        let outcome = tokio::time::timeout(PROBE_BUDGET, run(&deno, &script))
            .await
            .expect("the live probe hung; the prompt handshake never completed");
        let _ = std::fs::remove_file("/tmp/loca-live-probe.txt");

        // 1. Every kind prompted, and the resource was parsed out of the live
        //    text. `env` may legitimately be empty (Deno prompts for the whole
        //    environment when no key is named).
        let mut kinds: Vec<&str> = outcome.prompts.iter().map(|p| p.0.as_str()).collect();
        kinds.sort_unstable();
        let mut expected: Vec<&str> = ANSWERS.iter().map(|(kind, _)| *kind).collect();
        expected.sort_unstable();
        assert_eq!(
            kinds, expected,
            "the prompt kinds drifted; raw prompts: {:?}",
            outcome.prompts
        );
        for (kind, resource) in &outcome.prompts {
            if kind != "env" {
                assert!(!resource.is_empty(), "empty resource for {kind}");
            }
        }

        // 2. Every prompt settled, with the verdict that was answered. A
        //    settle that never arrives is what the give-up kill exists for, and
        //    a settle with the wrong verdict means the answer was misread.
        assert_eq!(
            outcome.settles.len(),
            outcome.prompts.len(),
            "a prompt never settled"
        );
        for (index, (kind, _)) in outcome.prompts.iter().enumerate() {
            assert_eq!(
                outcome.settles[index],
                answer_for(kind),
                "verdict mismatch for {kind}"
            );
        }

        // 3. The part that proves pause-and-resume rather than parsing: the
        //    granted accesses produced data, the denied ones raised
        //    `NotCapable`, and the script kept running through all seven.
        for (kind, allowed) in ANSWERS {
            let result = outcome
                .results
                .get(*kind)
                .unwrap_or_else(|| panic!("the probe never reported {kind}"));
            assert_eq!(result.0, *allowed, "{kind} did not {}", verb(*allowed));
            if !allowed {
                assert_eq!(
                    result.1.as_deref(),
                    Some("NotCapable"),
                    "denied {kind} raised the wrong error"
                );
            }
        }
        assert!(
            outcome.reached_end,
            "the script did not survive its denials to the end"
        );
    }

    fn answer_for(kind: &str) -> bool {
        ANSWERS
            .iter()
            .find(|(name, _)| *name == kind)
            .is_some_and(|(_, allow)| *allow)
    }

    fn verb(allowed: bool) -> &'static str {
        if allowed {
            "resume with data"
        } else {
            "fail"
        }
    }

    #[derive(Default)]
    struct Outcome {
        /// (permission, resource), in the order they were prompted.
        prompts: Vec<(String, String)>,
        settles: Vec<bool>,
        /// label -> (ok, error name)
        results: BTreeMap<String, (bool, Option<String>)>,
        reached_end: bool,
    }

    async fn run(deno: &std::path::Path, script: &std::path::Path) -> Outcome {
        // No `--allow-*` at all, so every access prompts. That is the point:
        // the probe is measuring the prompter, not the sandbox derivation.
        let spawned = pty::spawn(PtySpawn {
            label: "live probe".to_string(),
            program: deno.to_path_buf(),
            args: vec![
                "run".to_string(),
                "--quiet".to_string(),
                script.to_string_lossy().into_owned(),
            ],
            env: BTreeMap::from([
                ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
                ("HOME".into(), std::env::var_os("HOME").unwrap_or_default()),
                ("LANG".into(), "en_US.UTF-8".into()),
            ]),
            cwd: None,
        })
        .expect("the terminal could not be opened");

        let session = spawned.session;
        let mut events = spawned.events;
        let stdout = spawned.stdout.expect("unix always has a protocol pipe");

        // The script's own verdicts ride stdout, exactly as a worker's protocol
        // does; only the prompt is on the terminal.
        let verdicts = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut results = BTreeMap::new();
            let mut reached_end = false;
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                let label = value["label"].as_str().unwrap_or_default().to_string();
                if label == "end" {
                    reached_end = true;
                    continue;
                }
                results.insert(
                    label,
                    (
                        value["ok"].as_bool().unwrap_or(false),
                        value["name"].as_str().map(str::to_string),
                    ),
                );
            }
            (results, reached_end)
        });

        let mut parser = PromptParser::new();
        let mut outcome = Outcome::default();
        let mut pending: Option<(Vec<u8>, Instant)> = None;

        loop {
            // The answer is written on a schedule rather than on arrival, and
            // re-written until the confirmation is parsed. Both numbers are the
            // production ones.
            let next_write = pending
                .as_ref()
                .map(|(_, at)| *at)
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(next_write));
            tokio::select! {
                event = events.recv() => match event {
                    Some(PtyEvent::Output(bytes)) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        for event in parser.feed(&text) {
                            match event {
                                PromptParserEvent::Prompt { permission, resource, .. } => {
                                    let allow = answer_for(&permission);
                                    outcome.prompts.push((permission, resource));
                                    let payload = if allow { b"y\n".to_vec() } else { b"n\n".to_vec() };
                                    pending = Some((payload, Instant::now() + ANSWER_INITIAL_DELAY));
                                }
                                PromptParserEvent::Settled { granted } => {
                                    outcome.settles.push(granted);
                                    pending = None;
                                }
                                PromptParserEvent::StderrLine { .. } => {}
                            }
                        }
                    }
                    Some(PtyEvent::Exited { .. }) | None => break,
                },
                () = sleep, if pending.is_some() => {
                    if let Some((payload, at)) = &mut pending {
                        session.answer(payload);
                        *at = Instant::now() + ANSWER_RETRY;
                    }
                }
            }
        }

        let (results, reached_end) = verdicts.await.unwrap_or_default();
        outcome.results = results;
        outcome.reached_end = reached_end;
        outcome
    }
}

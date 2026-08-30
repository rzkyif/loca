//! The drift tripwire for the pinned `deno`: drives real permission prompts
//! through a real terminal and answers them on the production schedule, so a
//! Deno release that changes the prompt wording or handshake fails here.
//!
//! `#[ignore]`d by default (slow, and the pinned `deno` is a consent-gated
//! download). `LOCA_REQUIRE_DENO_PROMPTS=1` turns "no binary" from a skip into
//! a failure so CI cannot silently skip it. Unix only: the fixture reads
//! `/etc/hosts` and dlopens a system library; the ConPTY path has its own
//! local-only live cover.

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

    /// One access per permission kind; granted ones must resume with data,
    /// denied ones must throw `NotCapable` into a script that keeps running.
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

    const ANSWERS: &[(&str, bool)] = &[
        ("read", true),
        ("env", true),
        ("sys", true),
        ("write", false),
        ("run", false),
        ("net", false),
        ("ffi", false),
    ];

    /// The production answer schedule. Deno's `clear_stdin` eats anything
    /// written before stdin has been quiescent ~100ms, so the first write is
    /// delayed and re-sent until the confirmation parses. Removing only one of
    /// the two still passes (the other covers it); removing both hangs.
    const ANSWER_INITIAL_DELAY: Duration = Duration::from_millis(300);
    const ANSWER_RETRY: Duration = Duration::from_millis(600);

    const PROBE_BUDGET: Duration = Duration::from_secs(60);

    fn required() -> bool {
        std::env::var(REQUIRE).as_deref() == Ok("1")
    }

    /// The pinned `deno` as a running core resolves it, never `deno` off `PATH`.
    fn pinned_deno() -> Option<PathBuf> {
        let channel = Channel::from_env().ok()?;
        let paths = Paths::from_env(channel).ok()?;
        resolve_worker_deno(&paths).ok()
    }

    #[test]
    fn the_pinned_deno_is_present_when_it_is_required() {
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

        // `env` may legitimately be empty (Deno prompts for the whole environment).
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

        // Proves pause-and-resume rather than parsing.
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
        // No `--allow-*` at all, so every access prompts.
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

        // Verdicts ride stdout like a worker's protocol; only the prompt is on the terminal.
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

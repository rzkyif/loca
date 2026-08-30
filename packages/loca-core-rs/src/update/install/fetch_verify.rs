//! Fresh-install fetch of the core binary set from the signed manifest.
//!
//! Reuses the SAME
//! [`fetch_core_manifest`](crate::update::manifest::fetch_core_manifest) and
//! [`download_and_verify`](crate::update::artifact::download_and_verify) as
//! self-update, so a first install and an update share **one** audited trust
//! path. It differs only by absence: no rollback marker, no supervisor handoff,
//! and the running core binary is left alone, since this IS the binary running
//! the verb.
//!
//! Nothing in the repo invokes `self-install` today, but it is the only
//! consumer of `download_and_verify` outside apply, so deleting it would leave
//! the fresh-install path with no audited implementation.
//!
//! Reachable only from the `self-install` CLI verb, which is the install phase
//! `AGENTS.md` sanctions, and every request goes through [`crate::update::net`].

use crate::config::{host_triple, platform_exe, Config};
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::update::artifact::{download_and_verify, Decompress};
use crate::update::install::io::Io;
use crate::update::manifest::fetch_core_manifest;
use crate::update::net::Net;

/// Download, verify and place the workers and per-triple helpers the signed
/// manifest names, plus the core binary itself when it is not already on disk.
pub async fn self_install(config: &Config, paths: &Paths, net: &Net, io: &dyn Io) -> Result<()> {
    let manifest = fetch_core_manifest(net, &config.core_manifest_url()).await?;
    let triple = host_triple();
    let Some(entry) = manifest.binary_for(triple) else {
        // Before anything is written: a manifest that does not cover this
        // machine is a clean refusal, not a half-placed binary set.
        return Err(AppError::new(
            ErrorCode::UpdateFailed,
            format!("no core binary for triple {triple} in the manifest"),
        ));
    };

    tokio::fs::create_dir_all(paths.bin_dir()).await?;
    tokio::fs::create_dir_all(paths.workers_dir()).await?;

    let core_bin = paths.bin_path(&config.channel.core_binary_name("loca-core"));
    if tokio::fs::metadata(&core_bin).await.is_ok() {
        io.progress("core binary already present; leaving it in place");
    } else {
        download_and_verify(net, &entry.url, &core_bin, &entry.sha256, Decompress::Gzip).await?;
        io.progress(&format!("installed core binary v{}", manifest.version));
    }

    // `workers()` and `helpers()` collapse an ABSENT key to an empty slice:
    // absent versus empty matters to a publisher, not to a file placer.
    let workers = manifest.workers();
    for worker in workers {
        let dest = paths.workers_dir().join(&worker.name);
        download_and_verify(net, &worker.url, &dest, &worker.sha256, Decompress::Gzip).await?;
    }
    if !workers.is_empty() {
        io.progress(&format!("installed {} worker(s)", workers.len()));
    }

    let exe = platform_exe();
    // `helpers_for` is the filter, shared with apply: every triple's helpers
    // are in ONE document, so without it a mac places the Windows `.exe` set.
    let helpers = manifest.helpers_for(triple);
    for helper in &helpers {
        let dest = paths.bin_path(&format!("{}{exe}", helper.name));
        download_and_verify(net, &helper.url, &dest, &helper.sha256, Decompress::Gzip).await?;
    }
    if !helpers.is_empty() {
        io.progress(&format!("installed {} helper(s)", helpers.len()));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::Channel;
    use crate::update::install::io::testing::{recording, Captured};
    use crate::update::install::io::Streams;
    use crate::update::manifest::test_trust::Trusted;
    use crate::update::testing::{bounded, gzipped, sha256_hex, Serve, Upstream};
    use serde_json::{json, Value};

    struct Env {
        _dir: tempfile::TempDir,
        paths: Paths,
        io: Streams<Captured, Captured>,
        out: Captured,
        err: Captured,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().join("core"));
        paths.ensure_dirs().unwrap();
        let (io, out, err) = recording();
        Env {
            _dir: dir,
            paths,
            io,
            out,
            err,
        }
    }

    fn config(base: &str) -> Config {
        Config {
            channel: Channel::Stable,
            host: "127.0.0.1".into(),
            port: 8787,
            version: "0.1.15",
            storage_base_url: base.to_string(),
            hf_base_url: base.to_string(),
        }
    }

    /// A manifest covering this host, plus a worker and two helpers of which
    /// only one matches. Built from the published shape, not the Rust structs.
    fn manifest_for(base: &str, core: &[u8], worker: &[u8], helper: &[u8]) -> Value {
        json!({
            "schemaVersion": 1,
            "version": "9.9.9",
            "binaries": [{
                "triple": host_triple(),
                "url": format!("{base}/core.gz"),
                "sha256": sha256_hex(core),
            }],
            "workers": [{
                "name": "tool-worker.js",
                "url": format!("{base}/worker.gz"),
                "sha256": sha256_hex(worker),
            }],
            "helpers": [
                {
                    "name": "loca-core-updater",
                    "triple": host_triple(),
                    "url": format!("{base}/updater.gz"),
                    "sha256": sha256_hex(helper),
                },
                {
                    "name": "loca-core-updater",
                    // A triple this host is not, so its URL is never served
                    // and a deleted filter fails rather than downloading.
                    "triple": "some-other-triple",
                    "url": format!("{base}/foreign.gz"),
                    "sha256": sha256_hex(helper),
                },
            ],
        })
    }

    #[tokio::test]
    async fn a_fresh_install_places_the_whole_signed_set() {
        let trust = Trusted::new();
        let e = env();
        let (core, worker, helper) = (b"core binary".as_ref(), b"worker js".as_ref(), b"updater");
        let reserved = Upstream::reserve().await;
        let doc = trust.sign(manifest_for(reserved.base(), core, worker, helper));
        let up = reserved.serve(vec![
            (
                "/manifests/core.json",
                Serve::Body(doc.to_string().into_bytes()),
            ),
            ("/core.gz", Serve::Body(gzipped(core))),
            ("/worker.gz", Serve::Body(gzipped(worker))),
            ("/updater.gz", Serve::Body(gzipped(helper))),
        ]);
        let net = Net::new().unwrap();

        bounded(self_install(&config(up.base()), &e.paths, &net, &e.io))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(
                e.paths
                    .bin_path(&Channel::Stable.core_binary_name("loca-core"))
            )
            .unwrap(),
            core
        );
        assert_eq!(
            std::fs::read(e.paths.workers_dir().join("tool-worker.js")).unwrap(),
            worker
        );
        assert_eq!(
            std::fs::read(
                e.paths
                    .bin_path(&format!("loca-core-updater{}", platform_exe()))
            )
            .unwrap(),
            helper
        );
        assert_eq!(
            e.out.text(),
            "",
            "self-install emits no machine-readable line"
        );
    }

    #[tokio::test]
    async fn a_helper_for_another_triple_is_not_downloaded() {
        // The mock 404s anything undeclared, so deleting the triple filter
        // fails rather than placing a foreign binary under the same name.
        let trust = Trusted::new();
        let e = env();
        let (core, worker, helper) = (b"core".as_ref(), b"worker".as_ref(), b"updater");
        let reserved = Upstream::reserve().await;
        let doc = trust.sign(manifest_for(reserved.base(), core, worker, helper));
        let up = reserved.serve(vec![
            (
                "/manifests/core.json",
                Serve::Body(doc.to_string().into_bytes()),
            ),
            ("/core.gz", Serve::Body(gzipped(core))),
            ("/worker.gz", Serve::Body(gzipped(worker))),
            ("/updater.gz", Serve::Body(gzipped(helper))),
        ]);
        let net = Net::new().unwrap();

        bounded(self_install(&config(up.base()), &e.paths, &net, &e.io))
            .await
            .unwrap();

        assert!(
            !up.calls().iter().any(|call| call.contains("foreign.gz")),
            "requested: {:?}",
            up.calls()
        );
        assert!(e.err.text().contains("installed 1 helper(s)"));
    }

    #[tokio::test]
    async fn the_running_core_binary_is_left_in_place() {
        // This IS the binary running the verb, and Windows will not let a
        // process rewrite its own executable.
        let trust = Trusted::new();
        let e = env();
        let core_bin = e
            .paths
            .bin_path(&Channel::Stable.core_binary_name("loca-core"));
        std::fs::create_dir_all(core_bin.parent().unwrap()).unwrap();
        std::fs::write(&core_bin, b"the binary running this verb").unwrap();

        let (core, worker, helper) = (b"new core".as_ref(), b"worker".as_ref(), b"updater");
        let reserved = Upstream::reserve().await;
        let doc = trust.sign(manifest_for(reserved.base(), core, worker, helper));
        let up = reserved.serve(vec![
            (
                "/manifests/core.json",
                Serve::Body(doc.to_string().into_bytes()),
            ),
            // `/core.gz` undeclared, so a download of it 404s rather than
            // passing because the bytes happened to match.
            ("/worker.gz", Serve::Body(gzipped(worker))),
            ("/updater.gz", Serve::Body(gzipped(helper))),
        ]);
        let net = Net::new().unwrap();

        bounded(self_install(&config(up.base()), &e.paths, &net, &e.io))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(&core_bin).unwrap(),
            b"the binary running this verb"
        );
        assert!(e.err.text().contains("already present"));
        assert!(
            std::fs::metadata(e.paths.workers_dir().join("tool-worker.js")).is_ok(),
            "the rest of the set is still placed"
        );
    }

    #[tokio::test]
    async fn a_manifest_that_does_not_cover_this_host_is_refused_before_anything_is_written() {
        let trust = Trusted::new();
        let e = env();
        let reserved = Upstream::reserve().await;
        let doc = trust.sign(json!({
            "schemaVersion": 1,
            "version": "9.9.9",
            "binaries": [{
                "triple": "a-triple-this-host-is-not",
                "url": format!("{}/core.gz", reserved.base()),
                "sha256": sha256_hex(b"x"),
            }],
            "workers": [],
            "helpers": [],
        }));
        let up = reserved.serve(vec![(
            "/manifests/core.json",
            Serve::Body(doc.to_string().into_bytes()),
        )]);
        let net = Net::new().unwrap();

        let err = bounded(self_install(&config(up.base()), &e.paths, &net, &e.io))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no core binary for triple"));
        assert!(
            !e.paths.workers_dir().join("tool-worker.js").exists(),
            "nothing is placed before the refusal"
        );
    }

    #[tokio::test]
    async fn a_manifest_with_neither_optional_key_installs_the_core_and_nothing_else() {
        // `workers` and `helpers` are declared required in @loca/shared and
        // read as optional everywhere; matching the declaration would 422.
        let trust = Trusted::new();
        let e = env();
        let core = b"core".as_ref();
        let reserved = Upstream::reserve().await;
        let doc = trust.sign(json!({
            "schemaVersion": 1,
            "version": "9.9.9",
            "binaries": [{
                "triple": host_triple(),
                "url": format!("{}/core.gz", reserved.base()),
                "sha256": sha256_hex(core),
            }],
        }));
        let up = reserved.serve(vec![
            (
                "/manifests/core.json",
                Serve::Body(doc.to_string().into_bytes()),
            ),
            ("/core.gz", Serve::Body(gzipped(core))),
        ]);
        let net = Net::new().unwrap();

        bounded(self_install(&config(up.base()), &e.paths, &net, &e.io))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(
                e.paths
                    .bin_path(&Channel::Stable.core_binary_name("loca-core"))
            )
            .unwrap(),
            core
        );
        let text = e.err.text();
        assert!(!text.contains("worker(s)"), "{text}");
        assert!(!text.contains("helper(s)"), "{text}");
    }

    #[tokio::test]
    async fn an_unsigned_manifest_places_nothing() {
        // One audited trust path shared with self-update: placing files from
        // an unverified document would be a fresh-install signature bypass.
        let trust = Trusted::new();
        let e = env();
        let core = b"core".as_ref();
        let reserved = Upstream::reserve().await;
        let mut doc = manifest_for(reserved.base(), core, b"w", b"h");
        doc["signature"] = json!("bm90LWEtc2lnbmF0dXJl");
        let _ = &trust;
        let up = reserved.serve(vec![
            (
                "/manifests/core.json",
                Serve::Body(doc.to_string().into_bytes()),
            ),
            ("/core.gz", Serve::Body(gzipped(core))),
        ]);
        let net = Net::new().unwrap();

        let err = bounded(self_install(&config(up.base()), &e.paths, &net, &e.io))
            .await
            .unwrap_err();

        assert_eq!(err.status(), 422, "{err}");
        assert!(!e
            .paths
            .bin_path(&Channel::Stable.core_binary_name("loca-core"))
            .exists());
        assert!(
            !up.calls().iter().any(|call| call.contains("core.gz")),
            "nothing is even downloaded: {:?}",
            up.calls()
        );
    }
}

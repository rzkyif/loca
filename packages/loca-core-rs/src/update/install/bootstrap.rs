//! One-time secret and content bootstrap for a core install.
//!
//! Runs before, or as part of, `install-service`: it ensures the core tree
//! exists, mints the admin token that authorises pairing-code minting over
//! loopback, optionally seeds the two server settings that are not
//! API-writable, and plants the built-in extension so the core installs it
//! OFFLINE on first boot.
//!
//! `install-service` re-runs on every upgrade, so each step guards separately:
//! an existing non-empty admin token is left alone (re-minting would invalidate
//! a paired client), an existing `settings.json` is left entirely alone, an
//! already-planted extension is not re-fetched, and the plant is non-fatal in
//! every branch because the core seeds the built-in itself when it is missing.
//!
//! The plant writes the manifest bytes it received. The core verifies an
//! Ed25519 signature over the RAW document, so re-serialising the parsed value
//! would silently invalidate a perfectly valid manifest.
//!
//! The plant is the one outbound path here and runs only from the `bootstrap` /
//! `install-service` verbs, which is the install phase `AGENTS.md` sanctions.

use std::path::Path;

use serde_json::{Map, Value};

use crate::config::{Channel, Config};
use crate::error::Result;
use crate::extensions::install::seed::{
    planted_manifest_name, planted_tarball_name, BUILTIN_EXTENSION_ID,
};
use crate::files::write_owner_atomic;
use crate::paths::Paths;
use crate::update::artifact::{stream_to_file, Decompress};
use crate::update::install::io::Io;
use crate::update::net::Net;

/// The two install-time server settings, deliberately not API-writable: a
/// paired client must not widen the core's exposure or change how pairing
/// establishes trust.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootstrapOptions {
    /// `server.bindHost = 0.0.0.0`, so LAN devices can pair.
    pub bind_all: bool,
    /// `server.behindProxy = true`, so pairing trusts the proxy's real
    /// certificate instead of pinning the core's own.
    pub behind_proxy: bool,
}

pub async fn bootstrap(
    config: &Config,
    paths: &Paths,
    net: &Net,
    io: &dyn Io,
    opts: BootstrapOptions,
) -> Result<()> {
    paths.ensure_dirs()?;
    ensure_admin_token(paths, io).await?;
    seed_server_settings(paths, io, opts).await?;
    plant_builtin_extension(config, paths, net, io).await;
    Ok(())
}

/// Mint the admin token, once. Written owner-only **at creation** via
/// [`write_owner_atomic`] rather than written then chmodded, so the one
/// credential that authorises minting a pairing code is never world-readable.
async fn ensure_admin_token(paths: &Paths, io: &dyn Io) -> Result<()> {
    let file = paths.admin_token_file();
    if file_non_empty(&file).await {
        io.progress("admin token already present");
        return Ok(());
    }
    let mut bytes = [0u8; 16];
    {
        use rand::RngCore as _;
        rand::rng().fill_bytes(&mut bytes);
    }
    write_owner_atomic(file, hex::encode(bytes).into_bytes()).await?;
    io.progress("wrote admin token");
    Ok(())
}

/// Seed a FRESH `settings.json` with the install-time server settings.
///
/// Two distinct guards: nothing to seed writes **no file at all**, since a `{}`
/// would make every re-run take the "already present" branch and a machine
/// installed without `--bind-all` could never be given it; and an existing file
/// is left untouched whatever is in it, or an upgrade would clobber it.
async fn seed_server_settings(paths: &Paths, io: &dyn Io, opts: BootstrapOptions) -> Result<()> {
    let mut seed = Map::new();
    if opts.bind_all {
        seed.insert("server.bindHost".into(), Value::String("0.0.0.0".into()));
    }
    if opts.behind_proxy {
        seed.insert("server.behindProxy".into(), Value::Bool(true));
    }
    if seed.is_empty() {
        return Ok(());
    }
    let file = paths.settings_file();
    if exists(&file).await {
        io.progress("settings.json already present; leaving server settings as-is");
        return Ok(());
    }
    let keys: Vec<&str> = seed.keys().map(String::as_str).collect();
    let listed = keys.join(", ");
    let body = format!("{}\n", Value::Object(seed));
    tokio::fs::write(&file, body).await?;
    io.progress(&format!("seeded settings.json ({listed})"));
    Ok(())
}

/// Turn on `server.behindProxy` for an ALREADY-installed core.
///
/// An additive read-modify-write, unlike [`seed_server_settings`]'s refusal,
/// which is why both exist: the "install, pair, then flip" flow calls this
/// AFTER the loopback pair (a proxy-served core folds no certificate pin), by
/// which point `settings.json` holds the user's configuration.
///
/// An absent or unparseable file starts fresh rather than failing, or the verb
/// could not do the one thing it is for.
pub async fn enable_behind_proxy(paths: &Paths, io: &dyn Io) -> Result<()> {
    let file = paths.settings_file();
    let mut settings = match tokio::fs::read_to_string(&file).await {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        },
        Err(_) => Map::new(),
    };
    if settings.get("server.behindProxy") == Some(&Value::Bool(true)) {
        io.progress("server.behindProxy already on");
        return Ok(());
    }
    settings.insert("server.behindProxy".into(), Value::Bool(true));
    // Two-space pretty, matching the core's own settings writer, so its next
    // write produces no spurious reformat diff.
    let body = serde_json::to_string_pretty(&Value::Object(settings))
        .unwrap_or_else(|_| "{\"server.behindProxy\":true}".to_string());
    tokio::fs::write(&file, body).await?;
    io.progress("turned on server.behindProxy");
    Ok(())
}

/// Plant `.loca-extension-builtin.{tgz,json}` beside the extensions dir.
///
/// **Never fails.** Every branch reports and returns, because the core seeds
/// the built-in itself when the plant is missing, and a plant failure that
/// aborted `install-service` would turn a CDN hiccup into a failed install.
///
/// The filenames come from the extensions module rather than being re-derived,
/// or an installer could plant a file the core never looks for.
async fn plant_builtin_extension(config: &Config, paths: &Paths, net: &Net, io: &dyn Io) {
    if config.channel == Channel::Dev {
        // Dev resolves the built-in from the codebase, so a plant here would
        // install CDN bytes over a developer's working copy.
        return;
    }
    let extensions = paths.extensions_dir();
    let tarball_dest = extensions.join(planted_tarball_name(BUILTIN_EXTENSION_ID));
    let manifest_dest = extensions.join(planted_manifest_name(BUILTIN_EXTENSION_ID));
    if exists(&tarball_dest).await && exists(&manifest_dest).await {
        io.progress("built-in extension already planted");
        return;
    }

    let url = config.builtin_extension_manifest_url();
    let raw = match net.text(&url).await {
        Ok(raw) => raw,
        Err(err) => {
            io.progress(&format!(
                "built-in extension manifest unavailable ({err}); core will seed"
            ));
            return;
        }
    };
    let Some((tarball_url, sha256)) = read_plant_manifest(&raw) else {
        io.progress("built-in extension manifest incomplete; core will seed");
        return;
    };

    if let Err(err) = tokio::fs::create_dir_all(paths.staging_dir()).await {
        io.progress(&format!("built-in extension plant skipped: {err}"));
        return;
    }
    let tmp = paths
        .staging_dir()
        .join(format!("builtin-extension-{}.tgz", uuid::Uuid::new_v4()));

    // Hashed AS RECEIVED: the `.tgz` is the artifact the manifest digests.
    // Only a transport guard; the core re-verifies offline before installing.
    let got = match stream_to_file(net, &tarball_url, &tmp, Decompress::None).await {
        Ok(digest) => digest,
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            io.progress(&format!(
                "built-in extension plant skipped (core will seed): {err}"
            ));
            return;
        }
    };
    if got != sha256.to_lowercase() {
        let _ = tokio::fs::remove_file(&tmp).await;
        io.progress("built-in extension checksum mismatch; core will seed");
        return;
    }
    if let Err(err) = tokio::fs::rename(&tmp, &tarball_dest).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        io.progress(&format!(
            "built-in extension plant skipped (core will seed): {err}"
        ));
        return;
    }
    // The RAW document: the core verifies an Ed25519 signature over exactly
    // these bytes, so a re-encode silently invalidates a valid manifest.
    if let Err(err) = tokio::fs::write(&manifest_dest, raw.as_bytes()).await {
        io.progress(&format!(
            "built-in extension plant skipped (core will seed): {err}"
        ));
        return;
    }
    io.progress("planted built-in extension for offline seed");
}

/// The two fields the plant needs, read tolerantly. Deliberately not a parse
/// into the seeded-manifest type: the plant does not verify the signature and
/// must not appear to. A document missing either field reads as absent.
fn read_plant_manifest(raw: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let tarball_url = value.get("tarballUrl")?.as_str()?;
    let sha256 = value.get("sha256")?.as_str()?;
    if tarball_url.is_empty() || sha256.is_empty() {
        return None;
    }
    Some((tarball_url.to_string(), sha256.to_string()))
}

async fn exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

async fn file_non_empty(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(meta) => meta.is_file() && meta.len() > 0,
        Err(_) => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::update::install::io::testing::{recording, Captured};
    use crate::update::install::io::Streams;
    use crate::update::testing::{bounded, sha256_hex, Serve, Upstream};

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

    fn config(channel: Channel, base: &str) -> Config {
        Config {
            channel,
            host: "127.0.0.1".into(),
            port: 8787,
            version: "0.1.15",
            storage_base_url: base.to_string(),
            hf_base_url: base.to_string(),
        }
    }

    fn plant_manifest(tarball_url: &str, sha256: &str) -> String {
        // Carries a field this core does not model AND a signature: a fixture
        // of exactly the two fields read cannot see a re-encode drop the rest.
        format!(
            r#"{{"schemaVersion":1,"id":"loca-extension-builtin","version":"1.2.3","tarballUrl":"{tarball_url}","sha256":"{sha256}","publishedAt":"2026-01-01T00:00:00Z","signature":"c2ln"}}"#
        )
    }

    // --- the admin token ---------------------------------------------------

    #[tokio::test]
    async fn the_admin_token_is_owner_only_from_creation() {
        let e = env();
        let net = Net::new().unwrap();
        bounded(ensure_admin_token(&e.paths, &e.io)).await.unwrap();

        let file = e.paths.admin_token_file();
        let token = std::fs::read_to_string(&file).unwrap();
        assert_eq!(token.len(), 32, "16 random bytes as hex");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the credential that authorises minting a pairing code"
            );
        }
        drop(net);
    }

    #[tokio::test]
    async fn bootstrap_is_idempotent_for_the_admin_token() {
        // An upgrade re-runs install-service, and re-minting would invalidate
        // an already-paired install's admin access.
        let e = env();
        bounded(ensure_admin_token(&e.paths, &e.io)).await.unwrap();
        let first = std::fs::read_to_string(e.paths.admin_token_file()).unwrap();

        bounded(ensure_admin_token(&e.paths, &e.io)).await.unwrap();
        let second = std::fs::read_to_string(e.paths.admin_token_file()).unwrap();

        assert_eq!(first, second);
        assert!(e.err.text().contains("admin token already present"));
    }

    #[tokio::test]
    async fn an_empty_token_file_is_replaced_rather_than_kept() {
        // What an interrupted first install leaves. An empty token authorises
        // nothing, so treating it as present bricks pairing.
        let e = env();
        std::fs::write(e.paths.admin_token_file(), "").unwrap();

        bounded(ensure_admin_token(&e.paths, &e.io)).await.unwrap();

        let token = std::fs::read_to_string(e.paths.admin_token_file()).unwrap();
        assert_eq!(token.len(), 32);
    }

    #[tokio::test]
    async fn two_installs_do_not_mint_the_same_token() {
        // A credential, so it must come from the CSPRNG; a constant would
        // satisfy every other assertion in this file.
        let a = env();
        let b = env();
        bounded(ensure_admin_token(&a.paths, &a.io)).await.unwrap();
        bounded(ensure_admin_token(&b.paths, &b.io)).await.unwrap();
        assert_ne!(
            std::fs::read_to_string(a.paths.admin_token_file()).unwrap(),
            std::fs::read_to_string(b.paths.admin_token_file()).unwrap(),
        );
    }

    // --- the settings seed -------------------------------------------------

    #[tokio::test]
    async fn a_re_run_does_not_clobber_a_configured_core() {
        let e = env();
        let configured = r#"{"llm.model":"a-model","server.bindHost":"127.0.0.1"}"#;
        std::fs::write(e.paths.settings_file(), configured).unwrap();

        bounded(seed_server_settings(
            &e.paths,
            &e.io,
            BootstrapOptions {
                bind_all: true,
                behind_proxy: true,
            },
        ))
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(e.paths.settings_file()).unwrap(),
            configured,
            "byte for byte: a re-run must not rewrite a configured core at all"
        );
    }

    #[tokio::test]
    async fn nothing_to_seed_writes_no_file_at_all() {
        // An empty `{}` would make every later re-run take the "already
        // present" branch, so --bind-all could never be added afterwards.
        let e = env();
        bounded(seed_server_settings(
            &e.paths,
            &e.io,
            BootstrapOptions::default(),
        ))
        .await
        .unwrap();

        assert!(!e.paths.settings_file().exists());
        assert_eq!(e.err.text(), "", "and it says nothing about it either");
    }

    #[tokio::test]
    async fn each_flag_seeds_only_its_own_key() {
        for (opts, expected) in [
            (
                BootstrapOptions {
                    bind_all: true,
                    behind_proxy: false,
                },
                vec![("server.bindHost", Value::String("0.0.0.0".into()))],
            ),
            (
                BootstrapOptions {
                    bind_all: false,
                    behind_proxy: true,
                },
                vec![("server.behindProxy", Value::Bool(true))],
            ),
            (
                BootstrapOptions {
                    bind_all: true,
                    behind_proxy: true,
                },
                vec![
                    ("server.bindHost", Value::String("0.0.0.0".into())),
                    ("server.behindProxy", Value::Bool(true)),
                ],
            ),
        ] {
            let e = env();
            bounded(seed_server_settings(&e.paths, &e.io, opts))
                .await
                .unwrap();
            let raw = std::fs::read_to_string(e.paths.settings_file()).unwrap();
            let parsed: Map<String, Value> = serde_json::from_str(&raw).unwrap();
            assert_eq!(parsed.len(), expected.len(), "{opts:?} wrote {raw}");
            for (key, value) in expected {
                assert_eq!(parsed.get(key), Some(&value), "{opts:?}");
            }
            assert!(raw.ends_with('\n'));
        }
    }

    // --- enable-behind-proxy -----------------------------------------------

    #[tokio::test]
    async fn turning_on_behind_proxy_keeps_every_other_key() {
        // Install, pair over loopback, THEN flip: by that point settings.json
        // holds the user's configuration, so this must merge, not seed.
        let e = env();
        std::fs::write(
            e.paths.settings_file(),
            r#"{"llm.model":"a-model","stt.enabled":true}"#,
        )
        .unwrap();

        bounded(enable_behind_proxy(&e.paths, &e.io)).await.unwrap();

        let parsed: Map<String, Value> =
            serde_json::from_str(&std::fs::read_to_string(e.paths.settings_file()).unwrap())
                .unwrap();
        assert_eq!(
            parsed.get("llm.model"),
            Some(&Value::String("a-model".into()))
        );
        assert_eq!(parsed.get("stt.enabled"), Some(&Value::Bool(true)));
        assert_eq!(parsed.get("server.behindProxy"), Some(&Value::Bool(true)));
        assert_eq!(parsed.len(), 3);
    }

    #[tokio::test]
    async fn turning_it_on_twice_is_a_no_op_the_second_time() {
        let e = env();
        bounded(enable_behind_proxy(&e.paths, &e.io)).await.unwrap();
        let first = std::fs::read_to_string(e.paths.settings_file()).unwrap();

        bounded(enable_behind_proxy(&e.paths, &e.io)).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(e.paths.settings_file()).unwrap(),
            first
        );
        assert!(e.err.text().contains("already on"));
    }

    #[tokio::test]
    async fn an_absent_or_unreadable_settings_file_still_gets_the_key() {
        // Refusing here would leave the verb unable to do its one job.
        for staged in [None, Some("{ not json"), Some("[1,2,3]")] {
            let e = env();
            if let Some(raw) = staged {
                std::fs::write(e.paths.settings_file(), raw).unwrap();
            }

            bounded(enable_behind_proxy(&e.paths, &e.io)).await.unwrap();

            let parsed: Map<String, Value> =
                serde_json::from_str(&std::fs::read_to_string(e.paths.settings_file()).unwrap())
                    .unwrap();
            assert_eq!(
                parsed.get("server.behindProxy"),
                Some(&Value::Bool(true)),
                "{staged:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_merged_file_is_two_space_pretty_like_the_core_writes_it() {
        // So the core's next settings write produces no reformat diff.
        let e = env();
        bounded(enable_behind_proxy(&e.paths, &e.io)).await.unwrap();
        let raw = std::fs::read_to_string(e.paths.settings_file()).unwrap();
        assert!(raw.contains("\n  \"server.behindProxy\""), "{raw}");
    }

    // --- the plant ---------------------------------------------------------

    #[tokio::test]
    async fn dev_resolves_the_builtin_from_the_codebase_and_never_fetches() {
        let e = env();
        let up = Upstream::start(Serve::Status(500)).await;
        let net = Net::new().unwrap();

        bounded(plant_builtin_extension(
            &config(Channel::Dev, up.base()),
            &e.paths,
            &net,
            &e.io,
        ))
        .await;

        assert!(up.calls().is_empty(), "dev must not reach the network");
        assert!(!e
            .paths
            .extensions_dir()
            .join(planted_tarball_name(BUILTIN_EXTENSION_ID))
            .exists());
    }

    #[tokio::test]
    async fn a_stable_install_plants_the_tarball_and_the_raw_manifest() {
        let e = env();
        let tarball = b"a gzipped npm tarball, as bytes".to_vec();
        let digest = sha256_hex(&tarball);
        let reserved = Upstream::reserve().await;
        let raw = plant_manifest(&format!("{}/ext.tgz", reserved.base()), &digest);
        let up = reserved.serve(vec![
            ("/manifests/extension.json", Serve::Body(raw.clone().into())),
            ("/ext.tgz", Serve::Body(tarball.clone())),
        ]);
        let net = Net::new().unwrap();

        bounded(plant_builtin_extension(
            &config(Channel::Stable, up.base()),
            &e.paths,
            &net,
            &e.io,
        ))
        .await;

        let ext = e.paths.extensions_dir();
        assert_eq!(
            std::fs::read(ext.join(planted_tarball_name(BUILTIN_EXTENSION_ID))).unwrap(),
            tarball
        );
        // BYTE for byte: the signature is over exactly these bytes, so a
        // re-encode silently invalidates a valid manifest.
        assert_eq!(
            std::fs::read_to_string(ext.join(planted_manifest_name(BUILTIN_EXTENSION_ID))).unwrap(),
            raw
        );
        assert!(e.err.text().contains("planted built-in extension"));
    }

    #[tokio::test]
    async fn an_already_planted_extension_is_not_re_fetched() {
        let e = env();
        let ext = e.paths.extensions_dir();
        std::fs::write(ext.join(planted_tarball_name(BUILTIN_EXTENSION_ID)), b"old").unwrap();
        std::fs::write(ext.join(planted_manifest_name(BUILTIN_EXTENSION_ID)), b"{}").unwrap();
        let up = Upstream::start(Serve::Status(500)).await;
        let net = Net::new().unwrap();

        bounded(plant_builtin_extension(
            &config(Channel::Stable, up.base()),
            &e.paths,
            &net,
            &e.io,
        ))
        .await;

        assert!(up.calls().is_empty());
        assert_eq!(
            std::fs::read(ext.join(planted_tarball_name(BUILTIN_EXTENSION_ID))).unwrap(),
            b"old"
        );
    }

    #[tokio::test]
    async fn only_half_a_plant_is_completed_rather_than_accepted() {
        // A tarball with no manifest is unusable: the core verifies the
        // signature before extracting, so it would seed over the network
        // anyway. Both files, not either.
        let e = env();
        let ext = e.paths.extensions_dir();
        std::fs::write(ext.join(planted_tarball_name(BUILTIN_EXTENSION_ID)), b"old").unwrap();

        let tarball = b"fresh bytes".to_vec();
        let digest = sha256_hex(&tarball);
        let reserved = Upstream::reserve().await;
        let raw = plant_manifest(&format!("{}/ext.tgz", reserved.base()), &digest);
        let up = reserved.serve(vec![
            ("/manifests/extension.json", Serve::Body(raw.into())),
            ("/ext.tgz", Serve::Body(tarball.clone())),
        ]);
        let net = Net::new().unwrap();

        bounded(plant_builtin_extension(
            &config(Channel::Stable, up.base()),
            &e.paths,
            &net,
            &e.io,
        ))
        .await;

        assert!(!up.calls().is_empty(), "the half-plant is completed");
        assert_eq!(
            std::fs::read(ext.join(planted_tarball_name(BUILTIN_EXTENSION_ID))).unwrap(),
            tarball
        );
        assert!(ext
            .join(planted_manifest_name(BUILTIN_EXTENSION_ID))
            .exists());
    }

    #[tokio::test]
    async fn a_failed_plant_is_not_fatal_because_core_seeds_it_instead() {
        // Four ways to fail, all reported and none fatal: an install of the
        // whole product must not fail on a CDN hiccup.
        let tarball = b"bytes".to_vec();
        let good = sha256_hex(&tarball);

        // The routes need the base URL, unknown until the port is reserved,
        // so each case is a closure rather than a value.
        type Routes = Box<dyn Fn(&str) -> Vec<(&'static str, Serve)>>;
        let cases: Vec<(&str, Routes)> = vec![
            (
                "manifest unavailable",
                Box::new(|_| vec![("/manifests/extension.json", Serve::Status(503))]),
            ),
            (
                "manifest incomplete",
                Box::new(|_| {
                    vec![(
                        "/manifests/extension.json",
                        Serve::Body(br#"{"schemaVersion":1}"#.to_vec()),
                    )]
                }),
            ),
            (
                "manifest unparseable",
                Box::new(|_| {
                    vec![(
                        "/manifests/extension.json",
                        Serve::Body(b"not json at all".to_vec()),
                    )]
                }),
            ),
            (
                "tarball missing",
                Box::new(move |base: &str| {
                    vec![(
                        "/manifests/extension.json",
                        Serve::Body(plant_manifest(&format!("{base}/ext.tgz"), &good).into()),
                    )]
                }),
            ),
        ];

        for (name, routes) in cases {
            let e = env();
            let reserved = Upstream::reserve().await;
            let base = reserved.base().to_string();
            let up = reserved.serve(routes(&base));
            let net = Net::new().unwrap();

            bounded(plant_builtin_extension(
                &config(Channel::Stable, up.base()),
                &e.paths,
                &net,
                &e.io,
            ))
            .await;

            let ext = e.paths.extensions_dir();
            assert!(
                !ext.join(planted_tarball_name(BUILTIN_EXTENSION_ID))
                    .exists(),
                "{name} must plant nothing"
            );
            assert!(
                e.err.text().contains("core will seed") || e.err.text().contains("plant skipped"),
                "{name} must say why: {}",
                e.err.text()
            );
            assert_eq!(e.out.text(), "", "{name} must not touch stdout");
        }
    }

    #[tokio::test]
    async fn a_checksum_mismatch_plants_nothing_and_leaves_no_partial() {
        let e = env();
        let reserved = Upstream::reserve().await;
        // A digest for bytes the upstream is not going to serve.
        let raw = plant_manifest(
            &format!("{}/ext.tgz", reserved.base()),
            &sha256_hex(b"what the manifest promised"),
        );
        let up = reserved.serve(vec![
            ("/manifests/extension.json", Serve::Body(raw.into())),
            ("/ext.tgz", Serve::Body(b"something else entirely".to_vec())),
        ]);
        let net = Net::new().unwrap();

        bounded(plant_builtin_extension(
            &config(Channel::Stable, up.base()),
            &e.paths,
            &net,
            &e.io,
        ))
        .await;

        let ext = e.paths.extensions_dir();
        assert!(!ext
            .join(planted_tarball_name(BUILTIN_EXTENSION_ID))
            .exists());
        assert!(!ext
            .join(planted_manifest_name(BUILTIN_EXTENSION_ID))
            .exists());
        assert!(e.err.text().contains("checksum mismatch"));
        // The partial is destroyed, or the next attempt could resume onto
        // bytes that already failed verification.
        let staged: Vec<_> = std::fs::read_dir(e.paths.staging_dir())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .collect();
        assert!(staged.is_empty(), "left behind: {staged:?}");
    }

    #[tokio::test]
    async fn an_uppercase_manifest_digest_still_matches() {
        // A publisher emitting uppercase hex would otherwise fail every plant
        // with a checksum mismatch that reads as a corrupt CDN.
        let e = env();
        let tarball = b"bytes".to_vec();
        let reserved = Upstream::reserve().await;
        let raw = plant_manifest(
            &format!("{}/ext.tgz", reserved.base()),
            &sha256_hex(&tarball).to_uppercase(),
        );
        let up = reserved.serve(vec![
            ("/manifests/extension.json", Serve::Body(raw.into())),
            ("/ext.tgz", Serve::Body(tarball.clone())),
        ]);
        let net = Net::new().unwrap();

        bounded(plant_builtin_extension(
            &config(Channel::Stable, up.base()),
            &e.paths,
            &net,
            &e.io,
        ))
        .await;

        assert_eq!(
            std::fs::read(
                e.paths
                    .extensions_dir()
                    .join(planted_tarball_name(BUILTIN_EXTENSION_ID))
            )
            .unwrap(),
            tarball
        );
    }

    #[tokio::test]
    async fn the_plant_reads_only_the_two_fields_it_uses() {
        assert_eq!(
            read_plant_manifest(r#"{"tarballUrl":"https://x/y.tgz","sha256":"ab"}"#),
            Some(("https://x/y.tgz".into(), "ab".into()))
        );
        // Absent, wrong type, and empty all read as absent: a falsiness check
        // rather than a presence one.
        for raw in [
            r#"{"sha256":"ab"}"#,
            r#"{"tarballUrl":"https://x/y.tgz"}"#,
            r#"{"tarballUrl":42,"sha256":"ab"}"#,
            r#"{"tarballUrl":"","sha256":"ab"}"#,
            r#"{"tarballUrl":"https://x/y.tgz","sha256":""}"#,
            "not json",
            "[]",
        ] {
            assert_eq!(read_plant_manifest(raw), None, "{raw}");
        }
    }

    // --- the whole verb, and consent ---------------------------------------

    #[tokio::test]
    async fn bootstrap_provisions_a_fresh_core_in_one_call() {
        // The composition, not the parts: install-service calls this once and
        // expects a fully provisioned core.
        let e = env();
        let tarball = b"tarball".to_vec();
        let reserved = Upstream::reserve().await;
        let raw = plant_manifest(
            &format!("{}/ext.tgz", reserved.base()),
            &sha256_hex(&tarball),
        );
        let up = reserved.serve(vec![
            ("/manifests/extension.json", Serve::Body(raw.into())),
            ("/ext.tgz", Serve::Body(tarball)),
        ]);
        let net = Net::new().unwrap();

        bounded(bootstrap(
            &config(Channel::Stable, up.base()),
            &e.paths,
            &net,
            &e.io,
            BootstrapOptions {
                bind_all: true,
                behind_proxy: false,
            },
        ))
        .await
        .unwrap();

        assert!(!std::fs::read_to_string(e.paths.admin_token_file())
            .unwrap()
            .is_empty());
        assert!(std::fs::read_to_string(e.paths.settings_file())
            .unwrap()
            .contains("0.0.0.0"));
        assert!(e
            .paths
            .extensions_dir()
            .join(planted_tarball_name(BUILTIN_EXTENSION_ID))
            .exists());
        assert_eq!(e.out.text(), "", "no verb result on stdout from bootstrap");
    }

    #[tokio::test]
    async fn the_admin_token_and_the_settings_seed_make_no_outbound_call() {
        // The plant is the ONLY outbound path here, so everything else must
        // leave the counter at zero. `net.rs` asserts the counter goes up.
        let e = env();
        let net = Net::new().unwrap();

        bounded(ensure_admin_token(&e.paths, &e.io)).await.unwrap();
        bounded(seed_server_settings(
            &e.paths,
            &e.io,
            BootstrapOptions {
                bind_all: true,
                behind_proxy: true,
            },
        ))
        .await
        .unwrap();
        bounded(enable_behind_proxy(&e.paths, &e.io)).await.unwrap();

        assert_eq!(net.calls(), 0);
    }
}

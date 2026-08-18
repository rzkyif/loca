//! Canonical filesystem layout.
//!
//! Per-channel state derives from a single root, `~/.loca/<channel>/core`
//! (default channel stable), so a dev or latest install never reads or clobbers
//! a stable one. Models are the exception: they live at the shared
//! `~/.loca/models` so multi-GB weights are not re-downloaded per channel.
//!
//! `LOCA_CORE_HOME` overrides the root outright, and models move inside it. That
//! is one code path with three users: the isolated tempdir a test points at, an
//! installer placing state somewhere explicit, and the mobile frontend, which
//! calls [`Paths::rooted`] with the app-private dir because Android has no
//! `$HOME` to derive one from.
//!
//! An existing install's tree is read as-is, so every name below is fixed by
//! what is already on disk. It is one of the three byte-exact parity surfaces of
//! the port; changing a filename here silently orphans a user's data.
//!
//! `Paths` is a plain value the frontend constructs and application state
//! carries (D-010). There is no process global and no lazily-derived default: a
//! default would win over the frontend's injected root and leave mobile writing
//! to a path that does not exist.

use std::path::{Path, PathBuf};

use crate::config::Channel;
use crate::error::{internal, Result};

/// The resolved filesystem layout. Stores only the roots; every other path
/// derives on call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
    models: PathBuf,
    workers: PathBuf,
}

/// The environment values resolution reads, passed in rather than read inside so
/// every rule is a pure function. Tests cover the rules without touching the
/// process environment, which they could not do safely anyway: `set_var` is
/// unsound while other threads run.
struct PathEnv<'a> {
    home: Option<&'a str>,
    core_home: Option<&'a str>,
    workers_dir: Option<&'a str>,
}

impl Paths {
    /// Desktop resolution: `LOCA_CORE_HOME`, else `~/.loca/<channel>/core`.
    ///
    /// `channel` comes from [`crate::config::Config`]. This module never reads
    /// `LOCA_CHANNEL` itself.
    pub fn from_env(channel: Channel) -> Result<Self> {
        let home = env_var("HOME").or_else(|| env_var("USERPROFILE"));
        let core_home = env_var("LOCA_CORE_HOME");
        let workers_dir = env_var("LOCA_WORKERS_DIR");
        Self::resolve(
            channel,
            PathEnv {
                home: home.as_deref(),
                core_home: core_home.as_deref(),
                workers_dir: workers_dir.as_deref(),
            },
        )
    }

    /// Layout under an explicit root: models and workers nest inside it, nothing
    /// is shared with any other install. The mobile frontend's entry point, and
    /// what a test uses for a tempdir root.
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            models: root.join("models"),
            workers: root.join("workers"),
            root,
        }
    }

    fn resolve(channel: Channel, env: PathEnv<'_>) -> Result<Self> {
        let mut paths = match env.core_home {
            Some(root) => Self::rooted(root),
            None => {
                // The home lookup is deliberately inside this arm: a boot driven
                // by LOCA_CORE_HOME must work on a host with no home directory
                // at all, such as a service account or a CI container.
                let home = Path::new(env.home.ok_or_else(|| {
                    internal("could not determine home directory (no HOME or USERPROFILE)")
                })?);
                let base = home.join(".loca");
                Self {
                    root: base.join(channel.as_str()).join("core"),
                    // Shared across channels, so dev and latest reuse the weights
                    // stable already downloaded.
                    models: base.join("models"),
                    workers: PathBuf::new(),
                }
            }
        };
        // The tool worker .ts files ship separately from the compiled binary and
        // install under <root>/workers. `deno task dev` points LOCA_WORKERS_DIR
        // at the in-repo sources so editing one takes effect without a reinstall.
        paths.workers = match env.workers_dir {
            Some(dir) => PathBuf::from(dir),
            None => paths.root.join("workers"),
        };
        Ok(paths)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Shared across channels unless the root was overridden. Weights are large
    /// enough that re-downloading them per channel is not acceptable.
    pub fn models_dir(&self) -> &Path {
        &self.models
    }

    pub fn workers_dir(&self) -> &Path {
        &self.workers
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// The encrypted secrets bag. Its master key lives in the OS keychain.
    pub fn secrets_enc_file(&self) -> PathBuf {
        self.root.join("secrets.enc")
    }

    /// A plaintext secrets file nothing writes any more. It survives only so the
    /// sandbox keeps denying it on an install old enough to still have one.
    pub fn secrets_plain_file(&self) -> PathBuf {
        self.root.join("secrets.json")
    }

    pub fn admin_token_file(&self) -> PathBuf {
        self.root.join(".admin-token")
    }

    pub fn admin_password_file(&self) -> PathBuf {
        self.root.join(".admin-password")
    }

    pub fn db_file(&self) -> PathBuf {
        self.root.join("core.sqlite")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// `DENO_DIR` for the pinned `deno` the extension workers and stdio MCP
    /// servers run under, kept inside the channel root so it is one of the
    /// caches the storage screen can clear.
    pub fn deno_cache_dir(&self) -> PathBuf {
        self.root.join("deno-cache")
    }

    /// Scratch space for downloads mid-verification and for a staged update
    /// before the binary swap.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn memories_dir(&self) -> PathBuf {
        self.root.join("memories")
    }

    pub fn extensions_dir(&self) -> PathBuf {
        self.root.join("extensions")
    }

    /// Parent of every extension's private data dir. Named rather than joined at
    /// each site because the sandbox denies the whole subtree in one entry: an
    /// extension reaches its own data only through the module broker, never
    /// through the filesystem, and never reaches another's at all.
    pub fn extension_data_root(&self) -> PathBuf {
        self.root.join("extension-data")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn log_file(&self) -> PathBuf {
        self.logs_dir().join("core.log")
    }

    /// One line naming the most recent fatal boot failure (port in use, missing
    /// helper, unwritable database). Written on the fatal path and removed once
    /// the listener binds, so a supervising client or installer can say why the
    /// core is down instead of showing an opaque connection timeout.
    pub fn boot_error_file(&self) -> PathBuf {
        self.root.join("last-error.txt")
    }

    /// Written by the self-updater before the binary swap and read at the next
    /// startup, which is how a post-update first boot is told apart from a
    /// crash loop after a failed one.
    pub fn update_marker_file(&self) -> PathBuf {
        self.root.join("update.pending.json")
    }

    /// The self-signed HTTPS certificate the API is served with, public PEM
    /// only. Written at boot so co-located tooling on this host can verify the
    /// loopback API against a CA-of-one instead of disabling TLS verification.
    /// Clients pin the SPKI at pairing time and do not read this.
    pub fn tls_cert_file(&self) -> PathBuf {
        self.root.join("tls-cert.pem")
    }

    /// Exists once the named extension has been seeded at least once, so an
    /// extension the user deleted is not re-seeded on the next boot.
    pub fn seeded_marker_file(&self, extension_id: &str) -> PathBuf {
        self.root.join(format!("{extension_id}-seeded"))
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(session_id)
    }

    pub fn session_attachments_dir(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("attachments")
    }

    pub fn extension_dir(&self, extension_id: &str) -> PathBuf {
        self.extensions_dir().join(extension_id)
    }

    /// One extension's private data, including the SQLite database the module
    /// broker hands it. Separate from the install dir so a reinstall or a
    /// content hash never touches user data.
    pub fn extension_data_dir(&self, extension_id: &str) -> PathBuf {
        self.extension_data_root().join(extension_id)
    }

    pub fn model_path(&self, rel_path: &str) -> PathBuf {
        self.models.join(rel_path)
    }

    pub fn bin_path(&self, name: &str) -> PathBuf {
        self.bin_dir().join(name)
    }

    /// Creates every long-lived directory. Called once at boot, before the
    /// listener binds.
    ///
    /// No explicit mode, matching what the tree already has on disk: these
    /// inherit the umask.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.bin_dir(),
            self.deno_cache_dir(),
            self.staging_dir(),
            self.sessions_dir(),
            self.memories_dir(),
            self.models.clone(),
            self.extensions_dir(),
            self.workers.clone(),
            self.cache_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

fn env_var(key: &str) -> Option<String> {
    present(std::env::var(key).ok())
}

/// Set but empty counts as unset. Otherwise an empty `LOCA_CORE_HOME` would
/// resolve the models dir to the relative path `models`, and an empty `HOME`
/// would mask a usable `USERPROFILE`.
fn present(raw: Option<String>) -> Option<String> {
    raw.filter(|v| !v.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const HOME: &str = "/fake/home";

    fn resolve(channel: Channel, core_home: Option<&str>, workers: Option<&str>) -> Paths {
        Paths::resolve(
            channel,
            PathEnv {
                home: Some(HOME),
                core_home,
                workers_dir: workers,
            },
        )
        .unwrap()
    }

    #[test]
    fn each_channel_gets_its_own_subtree() {
        let home = Path::new(HOME);
        for (channel, dir) in [
            (Channel::Stable, "stable"),
            (Channel::Dev, "dev"),
            (Channel::Latest, "latest"),
        ] {
            assert_eq!(
                resolve(channel, None, None).root(),
                home.join(".loca").join(dir).join("core")
            );
        }
    }

    #[test]
    fn models_are_shared_across_channels() {
        let shared = Path::new(HOME).join(".loca").join("models");
        for channel in [Channel::Stable, Channel::Dev, Channel::Latest] {
            assert_eq!(resolve(channel, None, None).models_dir(), shared);
        }
    }

    #[test]
    fn core_home_override_wins_and_keeps_models_inside_it() {
        let p = resolve(Channel::Dev, Some("/tmp/iso"), None);
        assert_eq!(p.root(), Path::new("/tmp/iso"));
        assert_eq!(p.models_dir(), Path::new("/tmp/iso").join("models"));
        assert_eq!(p.workers_dir(), Path::new("/tmp/iso").join("workers"));
    }

    #[test]
    fn rooted_matches_the_core_home_override() {
        assert_eq!(
            Paths::rooted("/tmp/iso"),
            resolve(Channel::Dev, Some("/tmp/iso"), None)
        );
    }

    #[test]
    fn workers_dir_override_wins_over_the_root() {
        assert_eq!(
            resolve(Channel::Stable, None, Some("/repo/workers")).workers_dir(),
            Path::new("/repo/workers")
        );
        assert_eq!(
            resolve(Channel::Stable, Some("/tmp/iso"), Some("/repo/workers")).workers_dir(),
            Path::new("/repo/workers")
        );
    }

    #[test]
    fn a_missing_home_is_an_error_only_without_an_override() {
        let missing = |core_home| {
            Paths::resolve(
                Channel::Stable,
                PathEnv {
                    home: None,
                    core_home,
                    workers_dir: None,
                },
            )
        };
        assert!(missing(None).is_err());
        assert_eq!(
            missing(Some("/tmp/iso")).unwrap().root(),
            Path::new("/tmp/iso")
        );
    }

    /// The table is the recorded output of the TypeScript `paths()` under
    /// `HOME=/fake/home LOCA_CHANNEL=dev`, not a transcription of it. The
    /// command that produced it is in `docs/rust-port/packages/F3/plan.md`.
    /// Asserted on unix only: the separator differs on Windows, and the
    /// structural cases above already cover composition portably.
    #[test]
    #[cfg(unix)]
    fn every_path_matches_the_recorded_typescript_output() {
        let p = resolve(Channel::Dev, None, None);
        let root = "/fake/home/.loca/dev/core";
        let cases: Vec<(PathBuf, String)> = vec![
            (p.root().to_path_buf(), root.to_string()),
            (p.settings_file(), format!("{root}/settings.json")),
            (p.secrets_enc_file(), format!("{root}/secrets.enc")),
            (p.secrets_plain_file(), format!("{root}/secrets.json")),
            (p.admin_token_file(), format!("{root}/.admin-token")),
            (p.admin_password_file(), format!("{root}/.admin-password")),
            (p.db_file(), format!("{root}/core.sqlite")),
            (p.bin_dir(), format!("{root}/bin")),
            (p.deno_cache_dir(), format!("{root}/deno-cache")),
            (p.staging_dir(), format!("{root}/staging")),
            (p.sessions_dir(), format!("{root}/sessions")),
            (p.memories_dir(), format!("{root}/memories")),
            (p.extensions_dir(), format!("{root}/extensions")),
            (p.extension_data_root(), format!("{root}/extension-data")),
            (p.workers_dir().to_path_buf(), format!("{root}/workers")),
            (p.cache_dir(), format!("{root}/cache")),
            (p.logs_dir(), format!("{root}/logs")),
            (p.log_file(), format!("{root}/logs/core.log")),
            (p.boot_error_file(), format!("{root}/last-error.txt")),
            (
                p.update_marker_file(),
                format!("{root}/update.pending.json"),
            ),
            (p.tls_cert_file(), format!("{root}/tls-cert.pem")),
            (
                p.models_dir().to_path_buf(),
                "/fake/home/.loca/models".to_string(),
            ),
            (
                p.seeded_marker_file("loca-extension-builtin"),
                format!("{root}/loca-extension-builtin-seeded"),
            ),
            (p.session_dir("s1"), format!("{root}/sessions/s1")),
            (
                p.session_attachments_dir("s1"),
                format!("{root}/sessions/s1/attachments"),
            ),
            (p.extension_dir("ext"), format!("{root}/extensions/ext")),
            (
                p.extension_data_dir("ext"),
                format!("{root}/extension-data/ext"),
            ),
            (
                p.model_path("hf/repo/model.gguf"),
                "/fake/home/.loca/models/hf/repo/model.gguf".to_string(),
            ),
            (p.bin_path("deno"), format!("{root}/bin/deno")),
        ];
        for (got, want) in cases {
            assert_eq!(got, PathBuf::from(&want));
        }
    }

    #[test]
    fn ensure_dirs_creates_the_tree_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::rooted(tmp.path());
        p.ensure_dirs().unwrap();
        p.ensure_dirs().unwrap();
        for dir in [
            p.root().to_path_buf(),
            p.bin_dir(),
            p.deno_cache_dir(),
            p.staging_dir(),
            p.sessions_dir(),
            p.memories_dir(),
            p.models_dir().to_path_buf(),
            p.extensions_dir(),
            p.workers_dir().to_path_buf(),
            p.cache_dir(),
            p.logs_dir(),
        ] {
            assert!(dir.is_dir(), "missing {}", dir.display());
        }
    }

    #[test]
    fn from_env_resolves_without_panicking() {
        // Whatever the developer's environment holds, resolution either yields a
        // root or reports a missing home; it never panics and never falls back.
        let _ = Paths::from_env(Channel::Stable);
    }

    #[test]
    fn a_set_but_empty_var_counts_as_unset() {
        assert_eq!(present(Some(String::new())), None);
        assert_eq!(present(None), None);
        assert_eq!(
            present(Some("/tmp/iso".into())).as_deref(),
            Some("/tmp/iso")
        );
    }
}

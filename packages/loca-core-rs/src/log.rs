//! Structured logging. Two sinks, both scrubbed:
//!
//!   - stderr, for the dev console and journald/launchd capture. Human-tuned:
//!     lowercase padded level, optional dim scope, then the message.
//!   - `<logs_dir>/core.log`, size-rotated. A machine record, so its shape (ISO
//!     timestamp, uppercase level, bracketed scope) never changes.
//!
//! Modules log through `tracing` with a `loca::<scope>` target, mirroring the
//! client's convention:
//!
//! ```ignore
//! tracing::info!(target: "loca::chat", "stream {id} started");
//! ```
//!
//! Every formatted line runs through [`scrub_secrets`], at format time, after
//! the message is built, which is the only point covering every path.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::LazyLock;

use file_rotate::{compression::Compression, suffix::AppendCount, ContentLimit, FileRotate};
use regex::Regex;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

/// core.log rotation.
const MAX_LOG_BYTES: usize = 10 * 1024 * 1024;
const LOG_BACKUPS: usize = 5;

/// Install the global subscriber, once and as early as possible. Non-fatal: a
/// logging failure complains to stderr and continues. `logs_dir` is passed in so
/// this module stays independent of the filesystem layout.
pub fn init(logs_dir: Option<&Path>) {
    // Dependencies that log through the `log` facade (rustls, hyper, ...) are
    // bridged into tracing so they hit the same filter and the same scrubber.
    let _ = tracing_log::LogTracer::init();

    let file = logs_dir.and_then(|dir| match open_log_file(dir) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("[log] file logging disabled: {e}");
            None
        }
    });

    let subscriber = tracing_subscriber::registry().with(LocaLayer {
        console: ConsoleFormat::from_env(),
        file: file.map(std::sync::Mutex::new),
    });
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        eprintln!("[log] a global subscriber was already installed");
    }
}

fn open_log_file(dir: &Path) -> std::io::Result<FileRotate<AppendCount>> {
    std::fs::create_dir_all(dir)?;
    Ok(FileRotate::new(
        dir.join("core.log"),
        AppendCount::new(LOG_BACKUPS),
        ContentLimit::Bytes(MAX_LOG_BYTES),
        Compression::None,
        None,
    ))
}

struct LocaLayer {
    console: ConsoleFormat,
    file: Option<std::sync::Mutex<FileRotate<AppendCount>>>,
}

impl<S: Subscriber> Layer<S> for LocaLayer {
    fn enabled(&self, meta: &Metadata<'_>, _: Context<'_, S>) -> bool {
        *meta.level() <= max_level(meta.target())
    }

    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();
        let scope = scope_name(meta.target());

        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        let message = scrub_secrets(&message);

        let mut console = std::io::stderr().lock();
        let _ = writeln!(console, "{}", self.console.render(level, scope, &message));

        // The file sink is the machine record and starts at INFO; console keeps
        // the debug chatter.
        if level <= Level::INFO {
            if let Some(file) = &self.file {
                if let Ok(mut file) = file.lock() {
                    let _ = writeln!(file, "{}", render_file(level, scope, &message));
                }
            }
        }
    }
}

/// Our own logs are `loca` / `loca::<scope>`; the crate's own module paths come
/// through as `loca_core::...`. Everything else is a dependency.
fn is_ours(target: &str) -> bool {
    target == "loca" || target.starts_with("loca::") || target.starts_with("loca_core")
}

/// Scopes that relay third-party output. Their DEBUG chatter is dropped; INFO
/// and above pass. Every scope of our own logs at DEBUG.
fn max_level(target: &str) -> Level {
    match scope_name(target) {
        _ if !is_ours(target) => Level::WARN,
        "sidecars" => Level::INFO,
        _ => Level::DEBUG,
    }
}

/// The scope shown in both sinks: our `loca::` prefix stripped so it reads as a
/// bare scope, empty for the unscoped default. A dependency target is shown
/// as-is so its origin stays visible when it surfaces a warning.
fn scope_name(target: &str) -> &str {
    match target {
        "loca" | "default" | "" => "",
        t => t.strip_prefix("loca::").unwrap_or(t),
    }
}

/// Collects the event's `message` field, then appends any structured fields as
/// ` key=value` so nothing a caller attached is silently dropped.
struct MessageVisitor<'a>(&'a mut String);

impl Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

/// Console rendering options, resolved once from the environment.
///
///   - `LOCA_LOG_NO_TIME=1` drops the timestamp. `scripts/dev.ts` sets it: it
///     owns a single timestamp column for the multiplexed dev console.
///   - Color is on when `LOCA_LOG_COLOR=1` (forced by dev.ts, since core's
///     stderr is piped there and not a TTY) or when stderr is a TTY and
///     `NO_COLOR` is unset.
struct ConsoleFormat {
    time: bool,
    color: bool,
}

impl ConsoleFormat {
    fn from_env() -> Self {
        let forced = std::env::var("LOCA_LOG_COLOR").as_deref() == Ok("1");
        Self {
            time: std::env::var("LOCA_LOG_NO_TIME").as_deref() != Ok("1"),
            color: forced
                || (std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()),
        }
    }

    fn render(&self, level: Level, scope: &str, message: &str) -> String {
        let time = if self.time {
            format!(
                "{} ",
                self.paint(
                    "2",
                    &chrono::Local::now().format("%H:%M:%S%.3f").to_string()
                )
            )
        } else {
            String::new()
        };
        let scope = if scope.is_empty() {
            String::new()
        } else {
            format!("{} ", self.paint("2", &scope.to_lowercase()))
        };
        // Per-line head, so a multi-line message keeps its level and scope:
        // the dev multiplexer badges by the leading level word.
        let head = format!(
            "{time}{} {scope}",
            self.paint(level_color(level), level_label(level))
        );
        format!("{head}{}", message.replace('\n', &format!("\n{head}")))
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

fn render_file(level: Level, scope: &str, message: &str) -> String {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let scope = if scope.is_empty() {
        String::new()
    } else {
        format!(" [{scope}]")
    };
    let head = format!("{ts} {level}{scope} ");
    format!("{head}{}", message.replace('\n', &format!("\n{head}")))
}

/// SGR color per level: error red, warn yellow, info green, debug/trace dim.
fn level_color(level: Level) -> &'static str {
    match level {
        Level::ERROR => "31",
        Level::WARN => "33",
        Level::INFO => "32",
        Level::DEBUG | Level::TRACE => "2",
    }
}

/// Lowercase level label, right-padded to width 5 so the next column aligns.
fn level_label(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn ",
        Level::INFO => "info ",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

// Patterns ordered most-specific-first, so the labeled forms keep their label
// visible while masking the value.
#[allow(clippy::unwrap_used)]
static SCRUBBERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // Authorization: Bearer <token>
        (
            Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9_\-.~+/=]{16,}").unwrap(),
            "${1}<REDACTED>",
        ),
        // X-Admin-Token: <token>  /  x-admin-token=<token>
        (
            Regex::new(r"(?i)(x-admin-token\s*[:=]\s*)\S+").unwrap(),
            "${1}<REDACTED>",
        ),
        // "password":"<value>" in a logged JSON body (admin password set / mint
        // / revoke). Masks the value, keeps the key visible.
        (
            Regex::new(r#"(?i)("password"\s*:\s*)"(?:[^"\\]|\\.)*""#).unwrap(),
            r#"${1}"<REDACTED>""#,
        ),
        // ?token=<value> in URLs and WS upgrade query strings.
        (
            Regex::new(r"(?i)(\btoken=)[A-Za-z0-9_\-.]+").unwrap(),
            "${1}<REDACTED>",
        ),
        // Bare base64url tokens: 32 random bytes is 43 chars, and 40+ catches
        // any reasonable width without over-matching short identifiers.
        (Regex::new(r"[A-Za-z0-9_-]{40,}").unwrap(), "<REDACTED>"),
        // Bare hex: the admin-token file is 32 hex chars, sha256 digests are 64.
        (Regex::new(r"(?i)\b[a-f0-9]{32,}\b").unwrap(), "<REDACTED>"),
        // Padded standard base64, for the vault's master key: the base64url
        // pattern above breaks on any `+` or `/` in the value. Requiring the
        // `=` padding and a 40+ char body keeps this off ordinary text.
        (
            Regex::new(r"[A-Za-z0-9+/]{40,}={1,2}").unwrap(),
            "<REDACTED>",
        ),
    ]
});

/// Strip credential-shaped substrings from a string before it is written to a
/// log or crosses the wire. Best-effort defense: a misplaced token still leaks
/// at the call site, but at least it does not persist.
pub fn scrub_secrets(input: &str) -> String {
    let mut out = input.to_string();
    for (re, repl) in SCRUBBERS.iter() {
        out = re.replace_all(&out, *repl).into_owned();
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ours_covers_the_loca_namespace_only() {
        assert!(is_ours("loca"));
        assert!(is_ours("loca::chat"));
        assert!(is_ours("loca_core::transport::hub"));
        assert!(!is_ours("rustls::client"));
        assert!(!is_ours("hyper"));
    }

    #[test]
    fn scope_strips_our_prefix_and_passes_deps_through() {
        assert_eq!(scope_name("loca::chat"), "chat");
        assert_eq!(scope_name("loca"), "");
        assert_eq!(scope_name(""), "");
        assert_eq!(scope_name("rustls"), "rustls");
    }

    #[test]
    fn dependency_chatter_is_gated_and_ours_is_not() {
        assert_eq!(max_level("loca::chat"), Level::DEBUG);
        assert_eq!(max_level("loca::sidecars"), Level::INFO);
        assert_eq!(max_level("hyper::proto"), Level::WARN);
    }

    #[test]
    fn console_head_repeats_on_every_line() {
        let fmt = ConsoleFormat {
            time: false,
            color: false,
        };
        let out = fmt.render(Level::WARN, "chat", "first\nsecond");
        assert_eq!(out, "warn  chat first\nwarn  chat second");
    }

    #[test]
    fn file_line_keeps_the_machine_shape() {
        let out = render_file(Level::INFO, "boot", "started");
        assert!(out.ends_with(" INFO [boot] started"), "got {out}");
        assert!(out.starts_with("20"), "got {out}");
    }

    #[test]
    fn redacts_bearer_token_keeps_label() {
        let out = scrub_secrets("Authorization: Bearer abcdef0123456789ABCDEF");
        assert!(out.contains("Bearer <REDACTED>"), "got {out}");
        assert!(!out.contains("abcdef0123456789ABCDEF"), "got {out}");
    }

    #[test]
    fn redacts_admin_token_header() {
        let out = scrub_secrets("x-admin-token: deadbeefdeadbeefdeadbeefdeadbeef");
        assert!(out.contains("x-admin-token: <REDACTED>"), "got {out}");
    }

    #[test]
    fn redacts_password_in_a_logged_json_body() {
        let out = scrub_secrets(r#"POST /admin/password {"password":"hunter2","confirm":true}"#);
        assert!(out.contains(r#""password":"<REDACTED>""#), "got {out}");
        assert!(!out.contains("hunter2"), "got {out}");
    }

    #[test]
    fn redacts_token_query_param_keeps_key() {
        let out = scrub_secrets("connecting wss://h/ws?token=abc.def-123_XYZ");
        assert!(out.contains("token=<REDACTED>"), "got {out}");
        assert!(!out.contains("abc.def-123_XYZ"), "got {out}");
    }

    #[test]
    fn redacts_bare_tokens_and_digests() {
        let token = "A".repeat(43);
        assert!(!scrub_secrets(&format!("paired {token}")).contains(&token));
        let digest = "a".repeat(64);
        assert!(!scrub_secrets(&format!("sha256 {digest}")).contains(&digest));
    }

    #[test]
    fn redacts_a_standard_base64_master_key() {
        // 43 chars plus one `=`, and it can carry `+` and `/`.
        let key = format!("{}+{}/{}=", "A".repeat(20), "B".repeat(11), "C".repeat(10));
        let out = scrub_secrets(&format!("sealed master key {key}"));
        assert!(!out.contains(&key), "got {out}");
        assert!(out.contains("<REDACTED>"), "got {out}");
    }

    #[test]
    fn leaves_long_paths_alone() {
        // The `=` padding requirement is what lets a long path, otherwise all
        // base64 characters, survive into the log.
        let msg = "reading /Users/someone/Projects/loca/packages/locacoders/src/contract";
        assert_eq!(scrub_secrets(msg), msg);
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let msg = "rejected frame: unknown kind requirements.snapshot";
        assert_eq!(scrub_secrets(msg), msg);
    }
}

//! Shared helpers for the external, OpenAI-compatible endpoints.
//!
//! **Orchestrator-owned.** Not a work package: the LLM (D3), embedding (D5) and
//! speech (D15) paths all resolve a key the same way, and making it any one of
//! their exports would force the other two to import a module they have no
//! business depending on.
//!
//! Centralising it is also what keeps the three treating a keyless loopback
//! gateway and a missing key identically, which is the whole reason the
//! TypeScript centralised it.

use crate::error::Result;
use crate::secrets::Vault;
use crate::settings::access::str_setting;
use crate::settings::SettingsRecord;

/// Bound on an external speech call, matching the local engine's own.
///
/// Never express "no timeout" as zero here: the value travels to clients that
/// read 0 as a literal zero-millisecond deadline and abort every request
/// instantly. Absence is `None`, not `0`.
pub const EXTERNAL_TIMEOUT_MS: u64 = 120_000;

/// A placeholder for a gateway that wants no key at all. It has to be
/// non-empty: the SDK-shaped clients refuse to build without one, and the
/// "is this configured" checks key off emptiness.
const NO_AUTH_PLACEHOLDER: &str = "sk-noauth";

/// True for a loopback base URL, the only host allowed to run over plain HTTP.
///
/// Deliberately a literal prefix test rather than a URL parse: it is the same
/// test the TypeScript applies, and widening it (to any private address, say)
/// would extend the keyless-gateway exemption to hosts that never had it.
pub fn is_localhost_url(url: &str) -> bool {
    for host in ["http://localhost", "http://127.0.0.1"] {
        if let Some(rest) = url.strip_prefix(host) {
            if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') {
                return true;
            }
        }
    }
    false
}

/// Resolve the API key for an external endpoint.
///
/// Precedence is the vault first, then a plaintext value in `settings.json`.
/// The fallback exists for headless setups that provision a settings file and
/// have no keychain; it is lower precedence so a key that made it into the
/// vault is never shadowed by a stale one left in the file.
///
/// Returns [`NO_AUTH_PLACEHOLDER`] for a keyless loopback gateway, and `""` for
/// any other host with no key. **The caller decides whether `""` is fatal**, and
/// the three callers deliberately differ: the chat path reports it as a friendly
/// "not configured yet", while speech rejects it outright.
pub async fn resolve_external_api_key(
    settings: &SettingsRecord,
    vault: &Vault,
    key_id: &str,
    base_url: &str,
) -> Result<String> {
    if let Some(sealed) = vault.get(key_id).await? {
        if !sealed.is_empty() {
            return Ok(sealed);
        }
    }
    let from_settings = str_setting(settings, key_id, "");
    if !from_settings.is_empty() {
        return Ok(from_settings);
    }
    Ok(if is_localhost_url(base_url) {
        NO_AUTH_PLACEHOLDER.to_string()
    } else {
        String::new()
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_recognised_only_on_a_host_boundary() {
        assert!(is_localhost_url("http://127.0.0.1:1234/v1"));
        assert!(is_localhost_url("http://localhost/v1"));
        assert!(is_localhost_url("http://localhost"));
        // The boundary is what stops a lookalike host inheriting the keyless
        // exemption. Without it `localhost.attacker.example` reads as loopback.
        assert!(!is_localhost_url("http://localhost.attacker.example/v1"));
        assert!(!is_localhost_url("http://127.0.0.1.attacker.example/v1"));
        // https is never the plain-HTTP exemption's concern, and a remote host
        // with no key must resolve to "" rather than to the placeholder.
        assert!(!is_localhost_url("https://api.openai.com/v1"));
    }

    fn record(pairs: &[(&str, &str)]) -> SettingsRecord {
        let mut map = SettingsRecord::new();
        for (k, v) in pairs {
            map.insert(
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            );
        }
        map
    }

    fn vault() -> (Vault, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::with_store(
            dir.path().join("secrets.enc"),
            std::sync::Arc::new(crate::secrets::testing::MemoryStore::new()),
        );
        (vault, dir)
    }

    #[tokio::test]
    async fn the_vault_beats_a_plaintext_key_in_settings() {
        let (vault, _dir) = vault();
        vault
            .set("llm.external.apiKey", "sk-from-vault")
            .await
            .unwrap();
        let settings = record(&[("llm.external.apiKey", "sk-from-settings")]);
        let key = resolve_external_api_key(
            &settings,
            &vault,
            "llm.external.apiKey",
            "https://api.openai.com/v1",
        )
        .await
        .unwrap();
        assert_eq!(key, "sk-from-vault");
    }

    #[tokio::test]
    async fn settings_are_the_fallback_when_the_vault_has_nothing() {
        let (vault, _dir) = vault();
        let settings = record(&[("llm.external.apiKey", "sk-from-settings")]);
        let key = resolve_external_api_key(
            &settings,
            &vault,
            "llm.external.apiKey",
            "https://api.openai.com/v1",
        )
        .await
        .unwrap();
        assert_eq!(key, "sk-from-settings");
    }

    #[tokio::test]
    async fn a_keyless_loopback_gateway_gets_the_placeholder_and_a_remote_host_gets_nothing() {
        let (vault, _dir) = vault();
        let settings = SettingsRecord::new();
        let local = resolve_external_api_key(
            &settings,
            &vault,
            "llm.external.apiKey",
            "http://127.0.0.1:8080/v1",
        )
        .await
        .unwrap();
        assert_eq!(local, NO_AUTH_PLACEHOLDER);

        let remote = resolve_external_api_key(
            &settings,
            &vault,
            "llm.external.apiKey",
            "https://api.openai.com/v1",
        )
        .await
        .unwrap();
        assert_eq!(
            remote, "",
            "the caller decides whether an empty key is fatal"
        );
    }
}

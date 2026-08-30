//! Turns a settings record into an endpoint the provider can call.
//!
//! The record must be the SPARSE one (`Settings::load` or
//! `Settings::load_effective`), never a resolved one, for exactly one field:
//! `llm.port`'s schema default is the bare 7701, because static schema data
//! cannot express `7701 + channel.port_offset()`. A resolved record makes every
//! `dev` and `latest` install talk to the STABLE channel's llama-server.
//!
//! External API keys come from the shared resolver in [`crate::external`]: the
//! vault is authoritative and a plaintext value in `settings.json` is a
//! lower-precedence fallback for headless setups.

use crate::config::Channel;
use crate::error::Result;
use crate::external::resolve_external_api_key;
use crate::secrets::Vault;
use crate::settings::access::{num_setting, str_setting};
use crate::settings::SettingsRecord;

use super::types::{LlmEndpointConfig, LlmRoute, Reasoning, ReasoningEffort};

/// The local `llama-server` port before the channel offset.
const DEFAULT_LLM_PORT: u16 = 7701;

/// The sampling fallbacks, mirroring `DEFAULT_SAMPLING` in
/// `packages/loca-shared/src/domain/recommend.ts`. They must stay equal to it,
/// and to the catalog's own per-family recommendations: a core that samples
/// differently reads to a user as the model changing personality.
mod default_sampling {
    pub const TEMPERATURE: f64 = 0.7;
    pub const TOP_P: f64 = 0.95;
    pub const TOP_K: f64 = 20.0;
    pub const MIN_P: f64 = 0.0;
    pub const REPEAT_PENALTY: f64 = 1.0;
    pub const DRY_MULTIPLIER: f64 = 0.8;
}

/// Default context window when the user has set none, per provider.
const DEFAULT_LOCAL_CONTEXT: f64 = 4096.0;
const DEFAULT_EXTERNAL_CONTEXT: f64 = 128_000.0;

/// Whether an endpoint accepts the llama.cpp-only body fields.
///
/// Deliberately a loose substring test, and deliberately NOT
/// [`crate::external::is_localhost_url`], which is a prefix test with a host
/// boundary. This one gates SAMPLER FIELDS, where a false positive costs an
/// ignored key and `0.0.0.0` has to count as a legal llama-server bind address;
/// the other gates SENDING NO CREDENTIAL, where a false positive would extend
/// the keyless-gateway exemption to a lookalike host.
fn accepts_llama_body_fields(base_url: &str) -> bool {
    base_url.contains("127.0.0.1") || base_url.contains("localhost") || base_url.contains("0.0.0.0")
}

/// Resolve the endpoint for `route` from the current settings.
pub async fn resolve_endpoint(
    settings: &SettingsRecord,
    vault: &Vault,
    channel: Channel,
    route: LlmRoute,
) -> Result<LlmEndpointConfig> {
    if route == LlmRoute::Secondary {
        let base_url = str_setting(settings, "dualModel.external.baseUrl", "");
        let api_key =
            resolve_external_api_key(settings, vault, "dualModel.external.apiKey", &base_url)
                .await?;
        return Ok(LlmEndpointConfig {
            llama_body_extras: accepts_llama_body_fields(&base_url),
            base_url,
            api_key,
            model: str_setting(settings, "dualModel.external.model", ""),
            // The secondary route is a fast classifier/responder for simple
            // turns: thinking is forced off and sampling overrides are dropped
            // by design.
            reasoning: Some(Reasoning::Off),
            ..LlmEndpointConfig::default()
        });
    }

    let provider = str_setting(settings, "llm.provider", "local");
    // Sparse store, so the default mirrors the schema: thinking is off unless
    // the user turns it on. A legacy "auto" reads as "on"; the two were
    // equivalent for thinking and "auto" was dropped from the toggle.
    let reasoning = if str_setting(settings, "llm.reasoning", "off") == "off" {
        Reasoning::Off
    } else {
        Reasoning::On
    };
    // OpenAI-style samplers apply to both providers.
    let temperature = num_setting(settings, "llm.temperature", default_sampling::TEMPERATURE);
    let top_p = num_setting(settings, "llm.topP", default_sampling::TOP_P);
    // presence_penalty is provider-agnostic and 0 is its no-op, so 0 means
    // "omit". dry_multiplier is llama.cpp-only and read in the local branch.
    let presence_penalty = non_zero(num_setting(settings, "llm.presencePenalty", 0.0));

    if provider == "external" {
        let base_url = str_setting(settings, "llm.external.baseUrl", "");
        let api_key =
            resolve_external_api_key(settings, vault, "llm.external.apiKey", &base_url).await?;
        return Ok(LlmEndpointConfig {
            llama_body_extras: accepts_llama_body_fields(&base_url),
            base_url,
            api_key,
            model: str_setting(settings, "llm.external.model", ""),
            reasoning: Some(reasoning),
            // OpenAI-style endpoints take an effort level rather than a token
            // budget.
            reasoning_effort: Some(ReasoningEffort::from_setting(&str_setting(
                settings,
                "llm.reasoningEffort",
                "high",
            ))),
            temperature: Some(temperature),
            top_p: Some(top_p),
            presence_penalty,
            ..LlmEndpointConfig::default()
        });
    }

    let host = str_setting(settings, "llm.host", "127.0.0.1");
    let port = str_setting(
        settings,
        "llm.port",
        &(DEFAULT_LLM_PORT + channel.port_offset()).to_string(),
    );
    let base_url = format!("http://{host}:{port}/v1");
    Ok(LlmEndpointConfig {
        llama_body_extras: accepts_llama_body_fields(&base_url),
        base_url,
        api_key: "sk-local".to_string(),
        model: "default".to_string(),
        reasoning: Some(reasoning),
        temperature: Some(temperature),
        top_p: Some(top_p),
        // llama.cpp-only samplers. A model preset writes all of these on select.
        top_k: Some(whole(num_setting(
            settings,
            "llm.topK",
            default_sampling::TOP_K,
        ))),
        min_p: Some(num_setting(settings, "llm.minP", default_sampling::MIN_P)),
        repeat_penalty: Some(num_setting(
            settings,
            "llm.repeatPenalty",
            default_sampling::REPEAT_PENALTY,
        )),
        // DRY is on by default: small local models loop without it. 0 turns it
        // off, which means omitting the field.
        dry_multiplier: non_zero(num_setting(
            settings,
            "llm.dryMultiplier",
            default_sampling::DRY_MULTIPLIER,
        )),
        presence_penalty,
        // Optional chain order, `;`-joined by a preset from a catalog model.
        // Empty leaves the server's own chain alone.
        samplers: split_samplers(&str_setting(settings, "llm.samplers", "")),
        // 0 or unset is unrestricted, so it is omitted rather than sent as a cap
        // of zero, which would mean the opposite.
        reasoning_budget: non_zero(num_setting(settings, "llm.reasoningBudget", 0.0)).map(whole),
        is_local_provider: true,
        ..LlmEndpointConfig::default()
    })
}

/// The context window for the active provider on `route`, in tokens.
///
/// Feeds the token-budget maths (the tool filter's candidate listing, session
/// usage, the title generator's transcript selection).
pub fn resolve_context_size(settings: &SettingsRecord, route: LlmRoute) -> u32 {
    if route == LlmRoute::Secondary {
        return whole(num_setting(
            settings,
            "dualModel.external.contextSize",
            DEFAULT_EXTERNAL_CONTEXT,
        ));
    }
    if str_setting(settings, "llm.provider", "local") == "external" {
        return whole(num_setting(
            settings,
            "llm.external.contextSize",
            DEFAULT_EXTERNAL_CONTEXT,
        ));
    }
    whole(num_setting(
        settings,
        "llm.contextSize",
        DEFAULT_LOCAL_CONTEXT,
    ))
}

/// `;`-joined sampler names, or `None` when there are none.
fn split_samplers(value: &str) -> Option<Vec<String>> {
    let names: Vec<String> = value
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// JavaScript's `x || undefined` for a number: zero means absent.
fn non_zero(value: f64) -> Option<f64> {
    (value != 0.0).then_some(value)
}

/// A count on the wire, so serde renders `20` rather than `20.0`.
fn whole(value: f64) -> u32 {
    if value <= 0.0 {
        return 0;
    }
    // `as` saturates at the bounds and the negative case is handled above, so
    // this cannot wrap.
    value.floor() as u32
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn record(pairs: Value) -> SettingsRecord {
        match pairs {
            Value::Object(map) => map,
            _ => unreachable!("test records are objects"),
        }
    }

    fn vault() -> (Vault, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::with_store(
            dir.path().join("secrets.enc"),
            std::sync::Arc::new(crate::secrets::testing::MemoryStore::new()),
        );
        (vault, dir)
    }

    async fn resolve(settings: Value, route: LlmRoute) -> LlmEndpointConfig {
        let (vault, _dir) = vault();
        resolve_endpoint(&record(settings), &vault, Channel::Stable, route)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_empty_record_resolves_the_local_llama_server() {
        let cfg = resolve(json!({}), LlmRoute::Default).await;
        assert_eq!(cfg.base_url, "http://127.0.0.1:7701/v1");
        assert_eq!(cfg.api_key, "sk-local");
        assert_eq!(cfg.model, "default");
        assert_eq!(cfg.reasoning, Some(Reasoning::Off));
        assert_eq!(cfg.temperature, Some(0.7));
        assert_eq!(cfg.top_p, Some(0.95));
        assert_eq!(cfg.top_k, Some(20));
        assert_eq!(cfg.min_p, Some(0.0));
        assert_eq!(cfg.repeat_penalty, Some(1.0));
        assert_eq!(cfg.dry_multiplier, Some(0.8));
        // 0 or unset is unrestricted, so it is omitted rather than sent as 0.
        assert_eq!(cfg.reasoning_budget, None);
        assert_eq!(cfg.presence_penalty, None);
        assert!(cfg.is_local_provider);
        assert!(cfg.llama_body_extras);
    }

    #[tokio::test]
    async fn the_local_port_default_follows_the_channel() {
        let (vault, _dir) = vault();
        for (channel, port) in [
            (Channel::Stable, 7701),
            (Channel::Latest, 7711),
            (Channel::Dev, 7721),
        ] {
            let cfg = resolve_endpoint(&SettingsRecord::new(), &vault, channel, LlmRoute::Default)
                .await
                .unwrap();
            assert_eq!(
                cfg.base_url,
                format!("http://127.0.0.1:{port}/v1"),
                "the sidecar binds the channel-offset port, and the schema's flat \
                 \"7701\" would point a dev core at nothing",
            );
        }
    }

    #[tokio::test]
    async fn local_honours_host_port_sampling_and_reads_legacy_auto_as_on() {
        let cfg = resolve(
            json!({
                "llm.provider": "local",
                "llm.host": "192.168.1.10",
                "llm.port": "8080",
                "llm.reasoning": "auto",
                "llm.temperature": 0.3,
                "llm.topK": 40,
                "llm.reasoningBudget": 512,
                "llm.samplers": "top_k;temperature; dry ;;penalties",
            }),
            LlmRoute::Default,
        )
        .await;
        assert_eq!(cfg.base_url, "http://192.168.1.10:8080/v1");
        assert_eq!(cfg.reasoning, Some(Reasoning::On));
        assert_eq!(cfg.temperature, Some(0.3));
        assert_eq!(cfg.top_k, Some(40));
        assert_eq!(cfg.reasoning_budget, Some(512));
        assert_eq!(
            cfg.samplers,
            Some(vec![
                "top_k".to_string(),
                "temperature".to_string(),
                "dry".to_string(),
                "penalties".to_string(),
            ])
        );
        // A non-loopback host is still the local PROVIDER, so it still takes the
        // llama-only body fields.
        assert!(cfg.is_local_provider);
        assert!(!cfg.llama_body_extras);
    }

    #[tokio::test]
    async fn external_routes_to_the_external_fields_and_drops_the_llama_only_ones() {
        let cfg = resolve(
            json!({
                "llm.provider": "external",
                "llm.external.baseUrl": "https://api.openai.com/v1",
                "llm.external.model": "gpt-4o-mini",
                "llm.external.apiKey": "sk-real",
                "llm.reasoning": "on",
            }),
            LlmRoute::Default,
        )
        .await;
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.api_key, "sk-real");
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.reasoning, Some(Reasoning::On));
        assert_eq!(cfg.temperature, Some(0.7));
        assert_eq!(cfg.top_p, Some(0.95));
        assert_eq!(cfg.top_k, None);
        assert_eq!(cfg.min_p, None);
        assert_eq!(cfg.repeat_penalty, None);
        assert_eq!(cfg.dry_multiplier, None);
        assert_eq!(cfg.samplers, None);
        assert_eq!(cfg.reasoning_budget, None);
        assert_eq!(cfg.reasoning_effort, Some(ReasoningEffort::High));
        assert!(!cfg.is_local_provider);
        assert!(!cfg.llama_body_extras);
    }

    #[tokio::test]
    async fn external_forwards_the_reasoning_effort_level() {
        let cfg = resolve(
            json!({
                "llm.provider": "external",
                "llm.external.baseUrl": "https://api.openai.com/v1",
                "llm.reasoningEffort": "low",
            }),
            LlmRoute::Default,
        )
        .await;
        assert_eq!(cfg.reasoning_effort, Some(ReasoningEffort::Low));
    }

    #[tokio::test]
    async fn an_external_provider_with_no_key_stays_total() {
        // The chat path turns an empty key into a friendly "not set up yet"
        // message; resolving must not throw here.
        let cfg = resolve(
            json!({
                "llm.provider": "external",
                "llm.external.baseUrl": "https://api.openai.com/v1",
                "llm.external.model": "gpt-4o-mini",
            }),
            LlmRoute::Default,
        )
        .await;
        assert_eq!(cfg.api_key, "");
    }

    #[tokio::test]
    async fn an_external_loopback_gateway_needs_no_key_but_still_takes_llama_fields() {
        let cfg = resolve(
            json!({
                "llm.provider": "external",
                "llm.external.baseUrl": "http://127.0.0.1:1234/v1",
                "llm.external.model": "local-model",
            }),
            LlmRoute::Default,
        )
        .await;
        assert_eq!(cfg.api_key, "sk-noauth");
        // The pair that must not be folded together: an external provider on a
        // loopback gateway takes the llama samplers and does NOT queue.
        assert!(cfg.llama_body_extras);
        assert!(!cfg.is_local_provider);
    }

    #[tokio::test]
    async fn the_vault_beats_a_plaintext_key_for_the_llm_endpoint() {
        let (vault, _dir) = vault();
        vault.set("llm.external.apiKey", "sk-vault").await.unwrap();
        let settings = record(json!({
            "llm.provider": "external",
            "llm.external.baseUrl": "https://api.openai.com/v1",
            "llm.external.apiKey": "sk-plaintext",
        }));
        let cfg = resolve_endpoint(&settings, &vault, Channel::Stable, LlmRoute::Default)
            .await
            .unwrap();
        assert_eq!(cfg.api_key, "sk-vault");
    }

    #[tokio::test]
    async fn the_secondary_route_reads_dual_model_and_forwards_no_sampling() {
        let cfg = resolve(
            json!({
                "dualModel.external.baseUrl": "https://api2.example.com/v1",
                "dualModel.external.model": "fast-model",
                "dualModel.external.apiKey": "sk-2",
                // Present, and deliberately ignored on this route.
                "llm.temperature": 0.3,
                "llm.reasoning": "on",
            }),
            LlmRoute::Secondary,
        )
        .await;
        assert_eq!(cfg.base_url, "https://api2.example.com/v1");
        assert_eq!(cfg.model, "fast-model");
        assert_eq!(cfg.api_key, "sk-2");
        assert_eq!(cfg.reasoning, Some(Reasoning::Off));
        assert_eq!(cfg.temperature, None);
        assert_eq!(cfg.top_p, None);
        assert_eq!(cfg.top_k, None);
        assert!(!cfg.is_local_provider);
    }

    #[test]
    fn context_size_defaults_per_provider_and_route() {
        assert_eq!(
            resolve_context_size(&SettingsRecord::new(), LlmRoute::Default),
            4096
        );
        assert_eq!(
            resolve_context_size(
                &record(json!({ "llm.provider": "external" })),
                LlmRoute::Default
            ),
            128_000,
        );
        assert_eq!(
            resolve_context_size(
                &record(json!({ "dualModel.external.contextSize": 64000 })),
                LlmRoute::Secondary,
            ),
            64_000,
        );
    }

    #[test]
    fn context_size_coerces_a_numeric_string_and_ignores_junk() {
        assert_eq!(
            resolve_context_size(
                &record(json!({ "llm.contextSize": "8192" })),
                LlmRoute::Default
            ),
            8192,
        );
        assert_eq!(
            resolve_context_size(
                &record(json!({ "llm.contextSize": "nope" })),
                LlmRoute::Default
            ),
            4096,
        );
    }

    #[test]
    fn the_two_locality_tests_answer_different_questions() {
        // 0.0.0.0 is a legal llama-server bind address and takes the sampler
        // fields, but it is NOT the keyless-gateway exemption.
        assert!(accepts_llama_body_fields("http://0.0.0.0:7701/v1"));
        assert!(!crate::external::is_localhost_url("http://0.0.0.0:7701/v1"));
        // And the strict test is the one that refuses a lookalike host.
        assert!(!crate::external::is_localhost_url(
            "http://localhost.attacker.example/v1"
        ));
    }
}

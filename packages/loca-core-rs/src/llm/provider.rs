//! The upstream call: one streaming chat completion, normalised into deltas.
//! [`ChatProvider`] is the seam: [`HttpProvider`] serves any OpenAI-compatible
//! endpoint, and the in-process engine implements the same trait.

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::stream::{self, BoxStream, StreamExt as _};
use serde_json::{json, Map, Value};

use crate::error::{AppError, ErrorCode};

use super::sse::{ChatChunk, SseDecoder, DONE_SENTINEL};
use super::types::{LlmDelta, LlmRequest, Reasoning};

/// A completion in flight. Dropping it tears the upstream request down.
pub type DeltaStream = BoxStream<'static, Result<LlmDelta, ProviderError>>;

/// One upstream that can run a streaming chat completion. Stream-returning
/// rather than `async fn`, so it stays usable as a trait object without
/// `async-trait`.
pub trait ChatProvider: Send + Sync {
    fn stream(&self, req: LlmRequest) -> DeltaStream;
}

/// What went wrong upstream, before it is turned into an [`AppError`]. The
/// status and the provider's own code are only visible here, and the two
/// callers read them differently: the utility routes let
/// [`From<ProviderError>`] answer `internal_error`, while the chat path calls
/// [`classify_provider_error`] for a code the client can act on.
#[derive(Clone, Debug)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    /// HTTP status, when there was one. An in-process engine has none.
    pub status: Option<u16>,
    /// The provider's own `error.code`, when it sent one.
    pub code: Option<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// The request never completed: connection refused, DNS, TLS, a dropped
    /// stream.
    Transport,
    /// The provider answered with a non-2xx.
    Http,
    /// The body was not the stream we expect.
    Decode,
    /// Cancelled from this side: the scheduler's watchdog.
    Aborted,
}

impl ProviderError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Transport,
            status: None,
            code: None,
            message: message.into(),
        }
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Decode,
            status: None,
            code: None,
            message: message.into(),
        }
    }

    pub fn aborted(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Aborted,
            status: None,
            code: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

/// The default reading of an unclassified provider failure: 500
/// `internal_error`, message preserved. `transport::http` scrubs it on the way
/// to the wire, which is what makes carrying an upstream body safe.
impl From<ProviderError> for AppError {
    fn from(err: ProviderError) -> Self {
        AppError::new(ErrorCode::InternalError, err.message)
    }
}

/// Map an upstream failure to the code the client switches on. Structured
/// fields first, then the message, because a provider that sets neither
/// `status` nor `code` still says "maximum context length" in prose.
pub fn classify_provider_error(err: &ProviderError) -> AppError {
    let code = err.code.as_deref();
    let classified = if err.status == Some(401) || code == Some("invalid_api_key") {
        ErrorCode::ProviderUnauthorized
    } else if err.status == Some(429) || code == Some("rate_limit_exceeded") {
        ErrorCode::ProviderRateLimited
    } else if err.status == Some(503) || err.status == Some(504) {
        ErrorCode::ServerUnavailable
    } else if code == Some("context_length_exceeded") || mentions_context_window(&err.message) {
        ErrorCode::ContextWindowExceeded
    } else {
        ErrorCode::ProviderError
    };
    AppError::new(classified, err.message.clone())
}

/// The message-text half of the classification, case-insensitive.
fn mentions_context_window(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("context length") || lower.contains("context window")
}

/// How long the client waits for ANY byte before giving up. Deliberately not a
/// total request timeout: `reqwest`'s `timeout` bounds the whole request
/// including the streamed body, so it would kill a long generation mid-stream.
/// Do not "fix" this by adding `.timeout()`.
const READ_TIMEOUT: Duration = Duration::from_secs(600);

/// The largest upstream error body worth keeping in a message. A gateway that
/// answers HTML would otherwise put a whole page in an error response.
const MAX_ERROR_BODY: usize = 500;

/// The OpenAI-compatible HTTP provider.
pub struct HttpProvider {
    client: reqwest::Client,
}

impl HttpProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .read_timeout(READ_TIMEOUT)
                .build()
                // The builder only fails on a TLS backend it cannot
                // initialise, which is a broken build rather than a runtime
                // condition.
                .unwrap_or_else(|err| {
                    tracing::error!(
                        target: "loca::llm",
                        "falling back to the default HTTP client: {err}",
                    );
                    reqwest::Client::new()
                }),
        }
    }
}

impl Default for HttpProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatProvider for HttpProvider {
    fn stream(&self, req: LlmRequest) -> DeltaStream {
        let client = self.client.clone();
        stream::unfold(
            State::Start(Box::new(Pending { client, req })),
            |state| async move {
                match state {
                    State::Start(pending) => start(pending.client, pending.req).await,
                    State::Streaming(streaming) => streaming.next().await,
                    State::Done => None,
                }
            },
        )
        .boxed()
    }
}

enum State {
    /// Boxed: unboxed, every poll of the stream would move the whole endpoint
    /// config around long after it was sent.
    Start(Box<Pending>),
    Streaming(Box<Streaming>),
    Done,
}

struct Pending {
    client: reqwest::Client,
    req: LlmRequest,
}

struct Streaming {
    response: reqwest::Response,
    decoder: SseDecoder,
    /// One SSE chunk can decode to one delta, but a single read can carry
    /// several events, so decoded deltas queue here between polls.
    pending: VecDeque<LlmDelta>,
    /// A failure that must wait for [`Self::pending`] to drain: one read can
    /// carry good events and a malformed one, and the good ones came first.
    failure: Option<ProviderError>,
    saw_done: bool,
}

/// Open the request. Returns the first item to yield alongside the next state,
/// because a failure here is the stream's only item, or `None` when the upstream
/// answered without producing anything at all.
async fn start(
    client: reqwest::Client,
    req: LlmRequest,
) -> Option<(Result<LlmDelta, ProviderError>, State)> {
    let url = format!(
        "{}/chat/completions",
        req.endpoint.base_url.trim_end_matches('/')
    );
    // An empty key still sends a bearer, so a misconfigured external provider
    // gets a real 401 back rather than a local error that looks like a bug.
    let key = if req.endpoint.api_key.is_empty() {
        "sk-local"
    } else {
        &req.endpoint.api_key
    };
    let sent = client
        .post(&url)
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .json(&build_body(&req))
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(err) => {
            return Some((
                Err(ProviderError::transport(format!(
                    "{} is unreachable: {err}",
                    req.endpoint.base_url
                ))),
                State::Done,
            ))
        }
    };
    let status = response.status();
    if !status.is_success() {
        return Some((
            Err(http_error(status.as_u16(), response).await),
            State::Done,
        ));
    }
    // A 200 whose body carries no event ends with no items at all: an empty
    // delta would be a phantom token to a consumer counting deltas.
    Box::new(Streaming {
        response,
        decoder: SseDecoder::new(),
        pending: VecDeque::new(),
        failure: None,
        saw_done: false,
    })
    .next()
    .await
}

impl Streaming {
    /// The next decoded delta, reading more bytes until there is one.
    async fn next(mut self: Box<Self>) -> Option<(Result<LlmDelta, ProviderError>, State)> {
        loop {
            // Deltas first: one read can carry a real token followed by a
            // malformed sibling, and surfacing the error first discards the
            // token. The turn fails either way; what the session persisted is
            // what differs.
            if let Some(delta) = self.pending.pop_front() {
                return Some((Ok(delta), State::Streaming(self)));
            }
            if let Some(err) = self.failure.take() {
                return Some((Err(err), State::Done));
            }
            if self.saw_done {
                return None;
            }
            let chunk = match self.response.chunk().await {
                Ok(Some(chunk)) => chunk,
                // A stream that stops without `[DONE]` is ordinary for a
                // provider that closes the connection instead.
                Ok(None) => return None,
                Err(err) => {
                    return Some((
                        Err(ProviderError::transport(format!("stream failed: {err}"))),
                        State::Done,
                    ))
                }
            };
            self.decoder.push(&chunk);
            while let Some(event) = self.decoder.next_event() {
                if event == DONE_SENTINEL {
                    self.saw_done = true;
                    break;
                }
                match serde_json::from_str::<ChatChunk>(&event) {
                    Ok(parsed) => {
                        let delta = parsed.to_delta();
                        if !delta.is_empty() {
                            self.pending.push_back(delta);
                        }
                    }
                    Err(err) => {
                        self.failure = Some(ProviderError::decode(format!(
                            "malformed completion chunk: {err}"
                        )));
                        break;
                    }
                }
            }
        }
    }
}

/// Read a non-2xx response into an error, pulling the provider's own code out of
/// the body when it sent one.
async fn http_error(status: u16, response: reqwest::Response) -> ProviderError {
    let body = response.text().await.unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let code = error
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let detail = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| truncate(body.trim(), MAX_ERROR_BODY));
    ProviderError {
        kind: ProviderErrorKind::Http,
        status: Some(status),
        code,
        message: if detail.is_empty() {
            format!("upstream returned {status}")
        } else {
            format!("{status} {detail}")
        },
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "..."
}

/// Build the request body. Public to the module so the sampler and thinking
/// rules can be asserted without a server: a wrong branch there is invisible at
/// runtime and changes what the model does.
pub(super) fn build_body(req: &LlmRequest) -> Value {
    let endpoint = &req.endpoint;
    let temperature = req.overrides.temperature.or(endpoint.temperature);
    let top_p = req.overrides.top_p.or(endpoint.top_p);
    let max_tokens = req.overrides.max_tokens.or(endpoint.max_tokens);

    let mut body = Map::new();
    body.insert("model".into(), json!(endpoint.model));
    body.insert("messages".into(), json!(req.messages));
    if !req.tools.is_empty() {
        body.insert("tools".into(), json!(req.tools));
    }
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));
    if let Some(value) = temperature {
        body.insert("temperature".into(), json!(value));
    }
    if let Some(value) = top_p {
        body.insert("top_p".into(), json!(value));
    }
    if let Some(value) = max_tokens {
        body.insert("max_tokens".into(), json!(value));
    }

    let local = endpoint.llama_body_extras;
    if let Some(budget) = req.overrides.reasoning_budget {
        // A per-call budget overrides the endpoint's reasoning mode: 0 turns
        // thinking off, n > 0 turns it on capped at n.
        let think = budget > 0;
        if local {
            body.insert(
                "chat_template_kwargs".into(),
                json!({ "enable_thinking": think }),
            );
            if think {
                set_local_thinking_budget(&mut body, budget);
            }
        } else if think {
            // OpenAI-compatible servers take no token budget, so the budget
            // degrades to "think hard".
            body.insert("reasoning_effort".into(), json!("high"));
        }
    } else if let Some(reasoning) = endpoint.reasoning {
        if local {
            // Thinking-by-default models keep thinking unless told `false`, so
            // "off" must SEND the flag rather than omit it.
            let think = reasoning == Reasoning::On;
            body.insert(
                "chat_template_kwargs".into(),
                json!({ "enable_thinking": think }),
            );
            // The per-turn cap rides on the request so a changed budget applies
            // without restarting the server. Each client can run its own.
            if think {
                if let Some(budget) = endpoint.reasoning_budget.filter(|b| *b > 0) {
                    set_local_thinking_budget(&mut body, budget);
                }
            }
        } else if reasoning == Reasoning::On {
            // "off" sends nothing at all here: a non-reasoning model rejects the
            // parameter outright.
            body.insert(
                "reasoning_effort".into(),
                json!(endpoint
                    .reasoning_effort
                    .unwrap_or(super::types::ReasoningEffort::High)
                    .as_str()),
            );
        }
    }

    // llama.cpp-only samplers; an OpenAI-style endpoint rejects them. Single-
    // shot utility calls receive them too, but they pin a low temperature where
    // top_k / min_p are near-inert.
    if local {
        if let Some(value) = endpoint.top_k {
            body.insert("top_k".into(), json!(value));
        }
        if let Some(value) = endpoint.min_p {
            body.insert("min_p".into(), json!(value));
        }
        if let Some(value) = endpoint.repeat_penalty {
            body.insert("repeat_penalty".into(), json!(value));
        }
        // DRY curbs the verbatim phrase loops a token-level repeat penalty
        // misses. Base, length and window default on the server, so only the
        // multiplier is sent, and 0 means "off" rather than "zero".
        if let Some(value) = endpoint.dry_multiplier.filter(|v| *v != 0.0) {
            body.insert("dry_multiplier".into(), json!(value));
        }
        if let Some(samplers) = endpoint.samplers.as_ref().filter(|s| !s.is_empty()) {
            body.insert("samplers".into(), json!(samplers));
        }
    }
    // presence_penalty is OpenAI-standard, so it goes to both providers. 0 is
    // its no-op and is omitted, to avoid tripping a minimal external server; a
    // negative value is legal and IS sent.
    if let Some(value) = endpoint.presence_penalty.filter(|v| *v != 0.0) {
        body.insert("presence_penalty".into(), json!(value));
    }
    Value::Object(body)
}

/// Encode a per-request thinking-token cap for llama-server. Both fields are
/// load-bearing: the budget arrives as `thinking_budget_tokens`, and
/// `reasoning_control` is what makes the server create the sampler that ends
/// the `<think>` block at the cap. A top-level `reasoning_budget` is silently
/// dropped, so that spelling looks right and does nothing.
fn set_local_thinking_budget(body: &mut Map<String, Value>, budget: u32) {
    body.insert("thinking_budget_tokens".into(), json!(budget));
    body.insert("reasoning_control".into(), json!(true));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::types::{
        ChatMessage, LlmEndpointConfig, Overrides, Reasoning, ReasoningEffort, ToolDef,
    };
    use serde_json::json;

    fn local() -> LlmEndpointConfig {
        LlmEndpointConfig {
            base_url: "http://127.0.0.1:7701/v1".into(),
            api_key: "sk-local".into(),
            model: "default".into(),
            llama_body_extras: true,
            is_local_provider: true,
            ..LlmEndpointConfig::default()
        }
    }

    fn external() -> LlmEndpointConfig {
        LlmEndpointConfig {
            base_url: "https://api.openai.com/v1".into(),
            api_key: "sk-real".into(),
            model: "gpt-4o-mini".into(),
            ..LlmEndpointConfig::default()
        }
    }

    fn body(endpoint: LlmEndpointConfig) -> Value {
        build_body(&LlmRequest::new(endpoint, vec![ChatMessage::user("hi")]))
    }

    #[test]
    fn the_envelope_always_asks_for_a_stream_with_usage() {
        let body = body(local());
        assert_eq!(body["model"], json!("default"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "hi" }])
        );
        // Absent rather than null: an empty tool list is not an offer of no
        // tools, it is no offer.
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn tools_ride_along_when_there_are_any() {
        let request = LlmRequest::new(local(), vec![ChatMessage::user("hi")])
            .with_tools(vec![ToolDef::function("t", "does a thing", json!({}))]);
        assert_eq!(
            build_body(&request)["tools"],
            json!([{
                "type": "function",
                "function": { "name": "t", "description": "does a thing", "parameters": {} },
            }])
        );
    }

    #[test]
    fn a_local_request_carries_the_thinking_budget_and_the_llama_samplers() {
        let body = body(LlmEndpointConfig {
            reasoning: Some(Reasoning::On),
            temperature: Some(0.5),
            top_p: Some(0.9),
            top_k: Some(20),
            min_p: Some(0.05),
            repeat_penalty: Some(1.1),
            dry_multiplier: Some(0.8),
            presence_penalty: Some(1.5),
            samplers: Some(vec![
                "top_k".into(),
                "temperature".into(),
                "dry".into(),
                "penalties".into(),
            ]),
            reasoning_budget: Some(256),
            ..local()
        });
        assert_eq!(body["temperature"], json!(0.5));
        assert_eq!(body["top_p"], json!(0.9));
        assert_eq!(body["top_k"], json!(20));
        assert_eq!(body["min_p"], json!(0.05));
        assert_eq!(body["repeat_penalty"], json!(1.1));
        assert_eq!(body["dry_multiplier"], json!(0.8));
        assert_eq!(body["presence_penalty"], json!(1.5));
        assert_eq!(
            body["samplers"],
            json!(["top_k", "temperature", "dry", "penalties"])
        );
        assert_eq!(body["thinking_budget_tokens"], json!(256));
        assert_eq!(body["reasoning_control"], json!(true));
        assert_eq!(
            body["chat_template_kwargs"],
            json!({ "enable_thinking": true })
        );
        // A count, not a float: `20.0` is the same JSON number but not the same
        // wire bytes, and integer-typed fields are rejected by strict servers.
        assert_eq!(serde_json::to_string(&body["top_k"]).unwrap(), "20");
    }

    #[test]
    fn an_external_request_uses_reasoning_effort_and_drops_every_llama_field() {
        let body = body(LlmEndpointConfig {
            reasoning: Some(Reasoning::On),
            temperature: Some(0.5),
            top_p: Some(0.9),
            // Present, and must not reach an external endpoint.
            top_k: Some(20),
            min_p: Some(0.05),
            repeat_penalty: Some(1.1),
            dry_multiplier: Some(0.8),
            samplers: Some(vec!["top_k".into()]),
            reasoning_budget: Some(256),
            ..external()
        });
        assert_eq!(body["temperature"], json!(0.5));
        assert_eq!(body["top_p"], json!(0.9));
        assert_eq!(body["reasoning_effort"], json!("high"));
        for field in [
            "top_k",
            "min_p",
            "repeat_penalty",
            "dry_multiplier",
            "samplers",
            "chat_template_kwargs",
            "thinking_budget_tokens",
            "reasoning_control",
        ] {
            assert!(
                body.get(field).is_none(),
                "{field} is llama.cpp-only and an OpenAI-style endpoint rejects it",
            );
        }
    }

    #[test]
    fn a_loopback_gateway_takes_the_llama_fields_even_though_it_is_not_the_local_provider() {
        // Where the two localities disagree: an EXTERNAL provider pointed at a
        // hand-run llama-server. The body fields follow the URL, not the
        // provider setting.
        let body = body(LlmEndpointConfig {
            base_url: "http://127.0.0.1:1234/v1".into(),
            top_k: Some(20),
            dry_multiplier: Some(0.8),
            reasoning: Some(Reasoning::On),
            llama_body_extras: true,
            is_local_provider: false,
            ..external()
        });
        assert_eq!(body["top_k"], json!(20));
        assert_eq!(body["dry_multiplier"], json!(0.8));
        assert_eq!(
            body["chat_template_kwargs"],
            json!({ "enable_thinking": true })
        );
        assert!(
            body.get("reasoning_effort").is_none(),
            "a llama.cpp endpoint takes the flag, not the OpenAI effort level",
        );
    }

    #[test]
    fn an_external_request_forwards_the_configured_effort_level() {
        let body = body(LlmEndpointConfig {
            reasoning: Some(Reasoning::On),
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..external()
        });
        assert_eq!(body["reasoning_effort"], json!("medium"));
    }

    #[test]
    fn thinking_off_sends_the_flag_locally_and_nothing_externally() {
        let local_body = body(LlmEndpointConfig {
            reasoning: Some(Reasoning::Off),
            reasoning_budget: Some(256),
            ..local()
        });
        // The whole point: a thinking-by-default model needs to be TOLD not to.
        assert_eq!(
            local_body["chat_template_kwargs"],
            json!({ "enable_thinking": false })
        );
        // And no cap is sent when there is no thinking to cap.
        assert!(local_body.get("thinking_budget_tokens").is_none());
        assert!(local_body.get("reasoning_control").is_none());

        let external_body = body(LlmEndpointConfig {
            reasoning: Some(Reasoning::Off),
            ..external()
        });
        assert!(
            external_body.get("reasoning_effort").is_none(),
            "a non-reasoning model rejects the parameter, so off sends nothing",
        );
    }

    #[test]
    fn a_per_call_budget_overrides_the_endpoint_reasoning_mode() {
        let endpoint = LlmEndpointConfig {
            reasoning: Some(Reasoning::Off),
            ..local()
        };
        let on = build_body(
            &LlmRequest::new(endpoint.clone(), vec![ChatMessage::user("hi")]).with_overrides(
                Overrides {
                    reasoning_budget: Some(128),
                    ..Overrides::default()
                },
            ),
        );
        assert_eq!(
            on["chat_template_kwargs"],
            json!({ "enable_thinking": true })
        );
        assert_eq!(on["thinking_budget_tokens"], json!(128));

        // And zero is the other direction: an endpoint set to think, turned off
        // for this one call.
        let off = build_body(
            &LlmRequest::new(
                LlmEndpointConfig {
                    reasoning: Some(Reasoning::On),
                    ..local()
                },
                vec![ChatMessage::user("hi")],
            )
            .with_overrides(Overrides {
                reasoning_budget: Some(0),
                ..Overrides::default()
            }),
        );
        assert_eq!(
            off["chat_template_kwargs"],
            json!({ "enable_thinking": false })
        );
        assert!(off.get("thinking_budget_tokens").is_none());
    }

    #[test]
    fn a_per_call_budget_on_an_external_endpoint_asks_for_effort_only() {
        let body = build_body(
            &LlmRequest::new(external(), vec![ChatMessage::user("hi")]).with_overrides(Overrides {
                reasoning_budget: Some(128),
                ..Overrides::default()
            }),
        );
        assert_eq!(body["reasoning_effort"], json!("high"));
        assert!(body.get("thinking_budget_tokens").is_none());
    }

    #[test]
    fn overrides_win_over_the_endpoints_own_sampling() {
        let body = build_body(
            &LlmRequest::new(
                LlmEndpointConfig {
                    temperature: Some(0.7),
                    top_p: Some(0.8),
                    max_tokens: Some(1000),
                    ..external()
                },
                vec![ChatMessage::user("hi")],
            )
            .with_overrides(Overrides {
                temperature: Some(0.2),
                max_tokens: Some(64),
                ..Overrides::default()
            }),
        );
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["max_tokens"], json!(64));
        // Not overridden, so the endpoint's own value stands.
        assert_eq!(body["top_p"], json!(0.8));
    }

    #[test]
    fn zero_omits_the_two_fields_where_zero_means_off() {
        let body = body(LlmEndpointConfig {
            dry_multiplier: Some(0.0),
            presence_penalty: Some(0.0),
            min_p: Some(0.0),
            ..local()
        });
        assert!(body.get("dry_multiplier").is_none());
        assert!(body.get("presence_penalty").is_none());
        // min_p is not one of them: an explicit 0 is a real setting there.
        assert_eq!(body["min_p"], json!(0.0));
    }

    #[test]
    fn a_negative_presence_penalty_is_legal_and_is_sent() {
        let body = body(LlmEndpointConfig {
            presence_penalty: Some(-1.0),
            ..external()
        });
        assert_eq!(body["presence_penalty"], json!(-1.0));
    }

    #[test]
    fn classification_reads_the_status_then_the_code_then_the_prose() {
        let cases = [
            (Some(401), None, "nope", ErrorCode::ProviderUnauthorized),
            (
                None,
                Some("invalid_api_key"),
                "nope",
                ErrorCode::ProviderUnauthorized,
            ),
            (Some(429), None, "slow down", ErrorCode::ProviderRateLimited),
            (
                None,
                Some("rate_limit_exceeded"),
                "slow down",
                ErrorCode::ProviderRateLimited,
            ),
            (Some(503), None, "away", ErrorCode::ServerUnavailable),
            (Some(504), None, "away", ErrorCode::ServerUnavailable),
            (
                None,
                Some("context_length_exceeded"),
                "too long",
                ErrorCode::ContextWindowExceeded,
            ),
            (
                Some(400),
                None,
                "This model's maximum context length is 4096 tokens",
                ErrorCode::ContextWindowExceeded,
            ),
            (Some(500), None, "boom", ErrorCode::ProviderError),
        ];
        for (status, code, message, expected) in cases {
            let err = ProviderError {
                kind: ProviderErrorKind::Http,
                status,
                code: code.map(str::to_string),
                message: message.to_string(),
            };
            assert_eq!(
                classify_provider_error(&err).code,
                expected,
                "status {status:?} code {code:?} message {message:?}",
            );
        }
    }

    #[test]
    fn an_unclassified_provider_error_reaches_a_route_as_internal_error() {
        // What the two utility routes do with it, rather than the chat path's
        // classification.
        let err: AppError = ProviderError {
            kind: ProviderErrorKind::Http,
            status: Some(401),
            code: None,
            message: "401 Incorrect API key".to_string(),
        }
        .into();
        assert_eq!(err.code, ErrorCode::InternalError);
        assert_eq!(err.status(), 500);
        assert_eq!(err.message, "401 Incorrect API key");
    }

    #[test]
    fn an_error_body_is_truncated_rather_than_carried_whole() {
        assert_eq!(truncate("abcdef", 3), "abc...");
        assert_eq!(truncate("abc", 3), "abc");
    }

    // --- over a real socket ---------------------------------------------------
    //
    // These drive `reqwest` and the SSE decoder against a server replaying
    // recorded fixtures, so the parsing they prove is what production does.
    // Real sockets, so never a paused clock.

    use crate::llm::testing::{FakeUpstream, UpstreamScript};

    async fn collect(stream: DeltaStream) -> Vec<LlmDelta> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|item| item.expect("the fixture streams cleanly"))
            .collect()
    }

    async fn run(upstream: &FakeUpstream, endpoint: LlmEndpointConfig) -> Vec<LlmDelta> {
        let endpoint = LlmEndpointConfig {
            base_url: upstream.base_url.clone(),
            ..endpoint
        };
        collect(
            HttpProvider::new().stream(LlmRequest::new(endpoint, vec![ChatMessage::user("hi")])),
        )
        .await
    }

    #[tokio::test]
    async fn content_finish_and_usage_come_back_off_the_wire() {
        let upstream = FakeUpstream::start(UpstreamScript::Fixture("streaming-basic.sse")).await;
        let deltas = run(&upstream, external()).await;
        let text: String = deltas
            .iter()
            .filter_map(|d| d.content_delta.clone())
            .collect();
        assert_eq!(text, "Hello world");
        assert_eq!(
            deltas.iter().find_map(|d| d.finish_reason.clone()),
            Some("stop".to_string()),
        );
        let usage = deltas
            .iter()
            .find_map(|d| d.usage.as_ref())
            .expect("the stream asks for usage and the fixture sends it");
        assert_eq!((usage.prompt, usage.completion, usage.total), (10, 3, 13));
        assert_eq!(
            upstream.header("authorization"),
            Some("Bearer sk-real".to_string()),
        );
    }

    #[tokio::test]
    async fn reasoning_and_tool_calls_come_back_off_the_wire() {
        let upstream =
            FakeUpstream::start(UpstreamScript::Fixture("streaming-reasoning-tools.sse")).await;
        let deltas = run(&upstream, external()).await;
        let reasoning: String = deltas
            .iter()
            .filter_map(|d| d.reasoning_delta.clone())
            .collect();
        assert_eq!(reasoning, "Let me think... about this.");
        let calls: Vec<_> = deltas.iter().flat_map(|d| d.tool_calls.clone()).collect();
        assert_eq!(calls.len(), 2, "two fragments, both for index 0");
        assert_eq!(calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(calls[0].name.as_deref(), Some("do_thing"));
        let arguments: String = calls
            .iter()
            .filter_map(|c| c.arguments_delta.clone())
            .collect();
        assert_eq!(arguments, "{\"x\":42}");
        assert_eq!(
            deltas.iter().find_map(|d| d.finish_reason.clone()),
            Some("tool_calls".to_string()),
        );
    }

    #[tokio::test]
    async fn the_body_that_reaches_the_wire_is_the_body_that_was_built() {
        let upstream = FakeUpstream::start(UpstreamScript::Fixture("streaming-basic.sse")).await;
        run(
            &upstream,
            LlmEndpointConfig {
                reasoning: Some(Reasoning::On),
                temperature: Some(0.5),
                top_k: Some(20),
                reasoning_budget: Some(256),
                llama_body_extras: true,
                ..local()
            },
        )
        .await;
        let sent = &upstream.bodies()[0];
        assert_eq!(sent["stream"], json!(true));
        assert_eq!(sent["temperature"], json!(0.5));
        assert_eq!(sent["top_k"], json!(20));
        assert_eq!(sent["thinking_budget_tokens"], json!(256));
        assert_eq!(sent["reasoning_control"], json!(true));
        assert_eq!(
            sent["messages"],
            json!([{ "role": "user", "content": "hi" }])
        );
    }

    #[tokio::test]
    async fn an_empty_api_key_still_sends_a_bearer() {
        // So a misconfigured external provider answers 401 rather than failing
        // locally in a way that looks like a bug in Core.
        let upstream = FakeUpstream::start(UpstreamScript::Fixture("streaming-basic.sse")).await;
        run(
            &upstream,
            LlmEndpointConfig {
                api_key: String::new(),
                ..external()
            },
        )
        .await;
        assert_eq!(
            upstream.header("authorization"),
            Some("Bearer sk-local".to_string()),
        );
    }

    #[tokio::test]
    async fn an_upstream_error_carries_its_status_and_code_out() {
        let upstream = FakeUpstream::start(UpstreamScript::Error(
            401,
            r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#,
        ))
        .await;
        let endpoint = LlmEndpointConfig {
            base_url: upstream.base_url.clone(),
            ..external()
        };
        let mut stream =
            HttpProvider::new().stream(LlmRequest::new(endpoint, vec![ChatMessage::user("hi")]));
        let err = stream
            .next()
            .await
            .expect("a failed request is the stream's one item")
            .expect_err("401 is not a completion");
        assert_eq!(err.kind, ProviderErrorKind::Http);
        assert_eq!(err.status, Some(401));
        assert_eq!(err.code.as_deref(), Some("invalid_api_key"));
        assert!(err.message.contains("Incorrect API key provided"));
        assert_eq!(
            classify_provider_error(&err).code,
            ErrorCode::ProviderUnauthorized,
        );
        assert!(stream.next().await.is_none(), "and the stream is over");
    }

    #[tokio::test]
    async fn a_malformed_chunk_surfaces_only_after_the_deltas_that_preceded_it() {
        // Both events arrive in one read and the first is a real token.
        // Failing the whole read discards it, and the turn fails either way.
        let upstream = FakeUpstream::start(UpstreamScript::Raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: {oops\n\n",
        ))
        .await;
        let endpoint = LlmEndpointConfig {
            base_url: upstream.base_url.clone(),
            ..external()
        };
        let mut stream =
            HttpProvider::new().stream(LlmRequest::new(endpoint, vec![ChatMessage::user("hi")]));
        let first = stream
            .next()
            .await
            .expect("the good half of the read is still a delta")
            .expect("a decodable chunk is not an error");
        assert_eq!(first.content_delta.as_deref(), Some("Hello"));
        let err = stream
            .next()
            .await
            .expect("and then the malformed sibling")
            .expect_err("a malformed chunk is not a delta");
        assert_eq!(err.kind, ProviderErrorKind::Decode);
        assert!(stream.next().await.is_none(), "and the stream is over");
    }

    #[tokio::test]
    async fn a_200_with_an_empty_body_produces_no_deltas_at_all() {
        // Not one empty delta: they are dropped everywhere else in this file,
        // so one here is a token that never existed.
        let upstream = FakeUpstream::start(UpstreamScript::Raw("")).await;
        assert!(
            run(&upstream, external()).await.is_empty(),
            "an empty body is no completion, not an empty one",
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_is_a_transport_error_naming_the_url() {
        // Port 1 is reserved and never listening, so this is connection refused
        // rather than a timeout.
        let endpoint = LlmEndpointConfig {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            ..external()
        };
        let mut stream =
            HttpProvider::new().stream(LlmRequest::new(endpoint, vec![ChatMessage::user("hi")]));
        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Transport);
        assert!(
            err.message.contains("http://127.0.0.1:1/v1"),
            "{}",
            err.message
        );
        // Unclassified, which is what makes the utility routes answer 500.
        assert_eq!(AppError::from(err).code, ErrorCode::InternalError);
    }
}

//! `GET /api/v1/health`.
//!
//! The one unauthenticated route. A client hits it before pairing to learn that
//! a core is there, what version it is, and whether it is fronted by a reverse
//! proxy, and the LAN discovery sweep uses it as its probe. It is also what a
//! paired client reads on select to seed the core bar without waiting for the
//! first `core.status` frame.
//!
//! Nothing here is a secret. `behindProxy` is a hint the pairing handshake
//! verifies independently, since the certificate binding is folded into the key
//! confirmation.
//!
//! The response also carries [`Capabilities`], so a client asks what the CORE
//! can do rather than inferring it from the device rendering: a phone paired to
//! a desktop core must still offer extensions, schedules and stdio MCP.

use axum::routing::get;
use axum::Router;

use crate::boot::Frontend;
use crate::contract::api::{CoreCapabilities, HealthResponse, HealthResponseStatus};

use super::http::Json;
use super::state::AppContext;

/// Whether this build can spawn a `deno` tool worker. The one predicate behind
/// both the `extensions` capability and the `deno` binary requirement, shared
/// rather than re-derived because two copies of a `cfg` cannot be tested into
/// agreement on either host. It mirrors the gate on
/// `crate::extensions::worker::spawner`, absent on mobile because it starts a
/// downloaded interpreter, which W^X forbids.
pub const TOOL_WORKERS_SUPPORTED: bool = cfg!(not(any(target_os = "android", target_os = "ios")));

/// Whether this build linked llama.cpp. `local-inference`, default OFF.
pub const LOCAL_INFERENCE_COMPILED: bool = cfg!(feature = "local-inference");

/// Whether this build linked sherpa-onnx. `local-speech`, default OFF.
pub const LOCAL_SPEECH_COMPILED: bool = cfg!(feature = "local-speech");

/// What this core can do, as opposed to what the device rendering its UI can
/// do. A client caches the block on connect and hides the affordances whose
/// capability is false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Extension tools can actually RUN. Installing is not the question: an
    /// extension installs fine with no spawner, then fails at every call.
    pub extensions: bool,
    /// A language model can be run in process, so a local model selection is
    /// servable rather than a download with nothing to load it.
    pub local_inference: bool,
    /// STT and TTS can be run in process.
    pub local_speech: bool,
    /// A saved schedule will actually fire. The routes serve either way, so an
    /// existing prompt stays readable; what is false here is the arm loop.
    pub scheduled_prompts: bool,
    /// A local (spawned) MCP server can be started. Remote MCP is unaffected
    /// and is the half that works on a phone.
    pub stdio_mcp: bool,
    /// Inbound only: this core can accept NEW paired clients. A phone pairing
    /// OUT to a remote core is untouched by this flag; an embedded core has no
    /// listener, so nothing can pair TO it.
    pub pairing: bool,
    /// This core can replace its own binary. Named for the core specifically:
    /// on mobile the CLIENT self-updates while the core, inside the app bundle,
    /// cannot.
    pub core_update: bool,
}

impl Capabilities {
    /// The derivation, with every input explicit, so the mapping is testable
    /// over both values of inputs the build pins. [`detect`](Self::detect) is
    /// then the only place a compile-time fact is read.
    pub fn from_parts(
        frontend: Frontend,
        tool_workers: bool,
        local_inference: bool,
        local_speech: bool,
    ) -> Self {
        // A listener is what these four have in common: an embedded boot mints
        // no TLS material to pair against, never starts the arm loop, declares
        // stdio MCP unsupported, and never checks for the updater helper.
        let service = frontend == Frontend::Service;
        Self {
            extensions: tool_workers,
            local_inference,
            local_speech,
            scheduled_prompts: service,
            stdio_mcp: service,
            pairing: service,
            core_update: service,
        }
    }

    /// What this process, built this way, booted this way, can do.
    pub fn detect(frontend: Frontend) -> Self {
        Self::from_parts(
            frontend,
            TOOL_WORKERS_SUPPORTED,
            LOCAL_INFERENCE_COMPILED,
            LOCAL_SPEECH_COMPILED,
        )
    }

    /// The wire shape. Seven same-typed bools means any permutation compiles,
    /// so `each_field_reaches_its_own_wire_field` sets one at a time.
    fn to_wire(self) -> CoreCapabilities {
        CoreCapabilities {
            extensions: self.extensions,
            local_inference: self.local_inference,
            local_speech: self.local_speech,
            scheduled_prompts: self.scheduled_prompts,
            stdio_mcp: self.stdio_mcp,
            pairing: self.pairing,
            core_update: self.core_update,
        }
    }
}

pub fn routes(ctx: &AppContext) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .with_state(ctx.clone())
}

async fn health(
    axum::extract::State(ctx): axum::extract::State<AppContext>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: HealthResponseStatus::Ok,
        version: ctx.config.version.to_string(),
        uptime_ms: ctx.uptime_ms(),
        core: ctx.status.snapshot(),
        admin_password_set: ctx.auth.has_admin_password(),
        behind_proxy: ctx.behind_proxy,
        capabilities: ctx.capabilities.to_wire(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::transport::inproc::InProcRequest;
    use crate::transport::testing::{context_with, StubAuth};
    use crate::transport::Core;

    /// A core over `ctx`, assembled the way boot assembles one.
    fn core_from(ctx: AppContext) -> Core {
        let real = Arc::new(crate::auth::Auth::new(
            ctx.db.clone(),
            Arc::clone(&ctx.paths),
        ));
        let services = crate::transport::router::Services::unwired(&ctx, real).unwrap();
        Core::new(ctx, services)
    }

    /// A core whose context reports `caps`.
    fn core_reporting(caps: Capabilities) -> Core {
        let mut ctx = context_with(Arc::new(StubAuth::new()));
        ctx.capabilities = caps;
        core_from(ctx)
    }

    /// The served `capabilities`, over a context with `caps` and `pin`. `pin`
    /// is a parameter so the two callers below can cross it against the
    /// capabilities in opposite directions.
    async fn served_with_pin(caps: Capabilities, pin: Option<&str>) -> serde_json::Value {
        let mut ctx = context_with(Arc::new(StubAuth::new()));
        ctx.capabilities = caps;
        ctx.tls_pin = pin.map(str::to_string);
        ctx.behind_proxy = pin.is_none();
        let core = core_from(ctx);
        let res = core
            .call(InProcRequest::get("/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        let body: serde_json::Value = serde_json::from_slice(&res.body).unwrap();
        body["capabilities"].clone()
    }

    async fn served_capabilities(caps: Capabilities) -> serde_json::Value {
        let core = core_reporting(caps);
        let res = core
            .call(InProcRequest::get("/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        let body: serde_json::Value = serde_json::from_slice(&res.body).unwrap();
        body["capabilities"].clone()
    }

    /// The route must serve the CONTEXT's capabilities, not a fresh derivation,
    /// which would be silently wrong on every embedded core. Drives the shipped
    /// in-process path, so a handler ignoring `ctx.capabilities` fails.
    #[tokio::test]
    async fn the_route_serves_the_contexts_capabilities() {
        let embedded = Capabilities::from_parts(Frontend::Embedded, false, false, false);
        let served = served_capabilities(embedded).await;
        assert_eq!(served["extensions"], false);
        assert_eq!(served["localInference"], false);
        assert_eq!(served["localSpeech"], false);
        assert_eq!(served["scheduledPrompts"], false);
        assert_eq!(served["stdioMcp"], false);
        assert_eq!(served["pairing"], false);
        assert_eq!(served["coreUpdate"], false);
    }

    /// The other half, so the test above cannot pass by serving all-false.
    #[tokio::test]
    async fn a_service_core_serves_every_capability_as_true() {
        let service = Capabilities::from_parts(Frontend::Service, true, true, true);
        let served = served_capabilities(service).await;
        assert_eq!(served["extensions"], true);
        assert_eq!(served["localInference"], true);
        assert_eq!(served["localSpeech"], true);
        assert_eq!(served["scheduledPrompts"], true);
        assert_eq!(served["stdioMcp"], true);
        assert_eq!(served["pairing"], true);
        assert_eq!(served["coreUpdate"], true);
    }

    /// `tls_pin` must have no say in what this core reports it can do. The
    /// refused shortcut is deriving the block from `ctx.tls_pin.is_none()`. This
    /// half is the real configuration: `boot` writes `behind_proxy ||
    /// frontend.is_embedded()`, so a proxied DESKTOP core has no pin of its own
    /// and would report itself unable to pair, schedule or update.
    #[tokio::test]
    async fn a_core_behind_a_proxy_is_fully_capable_despite_having_no_pin() {
        let served = served_with_pin(
            Capabilities::from_parts(Frontend::Service, true, true, true),
            None,
        )
        .await;
        assert_eq!(
            served["pairing"], true,
            "a proxied core still accepts pairing"
        );
        assert_eq!(served["scheduledPrompts"], true);
        assert_eq!(served["stdioMcp"], true);
        assert_eq!(served["coreUpdate"], true);
    }

    /// The converse crossing. A pin on an embedded core is synthetic, and the
    /// point: the test above catches the shortcut only while `context_with`
    /// defaults `tls_pin` to `None`. Between them no fixture default passes
    /// both.
    #[tokio::test]
    async fn an_embedded_core_stays_incapable_even_when_a_pin_is_present() {
        let served = served_with_pin(
            Capabilities::from_parts(Frontend::Embedded, false, false, false),
            Some("Zm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmE="),
        )
        .await;
        assert_eq!(
            served["pairing"], false,
            "a pin does not give it a listener"
        );
        assert_eq!(served["scheduledPrompts"], false);
        assert_eq!(served["stdioMcp"], false);
        assert_eq!(served["coreUpdate"], false);
    }

    /// The wire names are camelCase, and they are what the client reads.
    #[tokio::test]
    async fn the_wire_names_are_the_camel_case_ones() {
        let served = served_capabilities(Capabilities::detect(Frontend::Service)).await;
        let obj = served.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "coreUpdate",
                "extensions",
                "localInference",
                "localSpeech",
                "pairing",
                "scheduledPrompts",
                "stdioMcp",
            ]
        );
    }

    /// Everything on, so a test can turn exactly one thing off and name it.
    fn all() -> Capabilities {
        Capabilities::from_parts(Frontend::Service, true, true, true)
    }

    #[test]
    fn a_service_core_with_everything_compiled_in_can_do_everything() {
        let caps = all();
        assert!(caps.extensions);
        assert!(caps.local_inference);
        assert!(caps.local_speech);
        assert!(caps.scheduled_prompts);
        assert!(caps.stdio_mcp);
        assert!(caps.pairing);
        assert!(caps.core_update);
    }

    #[test]
    fn an_embedded_core_loses_exactly_the_four_that_need_a_listener_or_a_helper() {
        let caps = Capabilities::from_parts(Frontend::Embedded, true, true, true);
        assert!(!caps.scheduled_prompts);
        assert!(!caps.stdio_mcp);
        assert!(!caps.pairing);
        assert!(!caps.core_update);
        // And keeps the three the frontend has no say over: a desktop process
        // booted embedded still spawns tool workers.
        assert!(caps.extensions);
        assert!(caps.local_inference);
        assert!(caps.local_speech);
    }

    /// The three build-decided flags, one at a time: each case turns exactly
    /// one off, so swapping any pair of `from_parts` arguments fails.
    #[test]
    fn no_tool_workers_costs_extensions_and_nothing_else() {
        let caps = Capabilities::from_parts(Frontend::Service, false, true, true);
        assert!(!caps.extensions);
        assert!(caps.local_inference);
        assert!(caps.local_speech);
        assert_eq!(
            Capabilities {
                extensions: true,
                ..caps
            },
            all()
        );
    }

    #[test]
    fn no_linked_llama_costs_local_inference_and_nothing_else() {
        let caps = Capabilities::from_parts(Frontend::Service, true, false, true);
        assert!(!caps.local_inference);
        assert!(caps.extensions);
        assert!(caps.local_speech);
        assert_eq!(
            Capabilities {
                local_inference: true,
                ..caps
            },
            all()
        );
    }

    #[test]
    fn no_linked_sherpa_costs_local_speech_and_nothing_else() {
        let caps = Capabilities::from_parts(Frontend::Service, true, true, false);
        assert!(!caps.local_speech);
        assert!(caps.extensions);
        assert!(caps.local_inference);
        assert_eq!(
            Capabilities {
                local_speech: true,
                ..caps
            },
            all()
        );
    }

    /// The frontend must not reach the three build-decided flags, which would
    /// let `Frontend::Embedded` silently mean "no extensions" on a desktop
    /// build and hide a working feature.
    #[test]
    fn the_frontend_moves_four_flags_and_only_those_four() {
        let service = Capabilities::from_parts(Frontend::Service, true, false, true);
        let embedded = Capabilities::from_parts(Frontend::Embedded, true, false, true);
        assert_eq!(embedded.extensions, service.extensions);
        assert_eq!(embedded.local_inference, service.local_inference);
        assert_eq!(embedded.local_speech, service.local_speech);
        assert_ne!(embedded.scheduled_prompts, service.scheduled_prompts);
        assert_ne!(embedded.stdio_mcp, service.stdio_mcp);
        assert_ne!(embedded.pairing, service.pairing);
        assert_ne!(embedded.core_update, service.core_update);
    }

    /// One field's domain value, the wire field it must land in, and its name.
    type Case = (Capabilities, fn(&CoreCapabilities) -> bool, &'static str);

    /// Every domain field reaches its OWN wire field. Each case turns exactly
    /// one on and counts how many arrive, so a swap of any pair fails and
    /// `to_wire` returning all-true does not pass.
    #[test]
    fn each_field_reaches_its_own_wire_field() {
        let off = Capabilities::from_parts(Frontend::Embedded, false, false, false);
        assert_eq!(
            off,
            Capabilities {
                extensions: false,
                ..off
            },
            "the fixture is all-false"
        );

        let cases: [Case; 7] = [
            (
                Capabilities {
                    extensions: true,
                    ..off
                },
                |w| w.extensions,
                "extensions",
            ),
            (
                Capabilities {
                    local_inference: true,
                    ..off
                },
                |w| w.local_inference,
                "localInference",
            ),
            (
                Capabilities {
                    local_speech: true,
                    ..off
                },
                |w| w.local_speech,
                "localSpeech",
            ),
            (
                Capabilities {
                    scheduled_prompts: true,
                    ..off
                },
                |w| w.scheduled_prompts,
                "scheduledPrompts",
            ),
            (
                Capabilities {
                    stdio_mcp: true,
                    ..off
                },
                |w| w.stdio_mcp,
                "stdioMcp",
            ),
            (
                Capabilities {
                    pairing: true,
                    ..off
                },
                |w| w.pairing,
                "pairing",
            ),
            (
                Capabilities {
                    core_update: true,
                    ..off
                },
                |w| w.core_update,
                "coreUpdate",
            ),
        ];

        for (caps, read, name) in cases {
            let wire = caps.to_wire();
            assert!(read(&wire), "{name} did not reach its own wire field");
            let on = [
                wire.extensions,
                wire.local_inference,
                wire.local_speech,
                wire.scheduled_prompts,
                wire.stdio_mcp,
                wire.pairing,
                wire.core_update,
            ]
            .iter()
            .filter(|f| **f)
            .count();
            assert_eq!(on, 1, "{name} set more than its own wire field");
        }
    }

    /// `detect` must pass on the frontend it is given: hardcoding
    /// `Frontend::Service` survives every other test here.
    #[test]
    fn detect_passes_its_frontend_through() {
        let service = Capabilities::detect(Frontend::Service);
        let embedded = Capabilities::detect(Frontend::Embedded);
        assert_ne!(service, embedded);
        assert!(service.pairing && service.scheduled_prompts);
        assert!(service.stdio_mcp && service.core_update);
        assert!(!embedded.pairing && !embedded.scheduled_prompts);
        assert!(!embedded.stdio_mcp && !embedded.core_update);
    }

    /// Pins that `detect` feeds the constants to `from_parts` in the right
    /// order. No test on one host can prove each `cfg` names the right thing;
    /// `deno task mobile-check` compiling both mobile triples covers that.
    #[test]
    fn detect_feeds_the_build_constants_through_in_order() {
        assert_eq!(
            Capabilities::detect(Frontend::Service),
            Capabilities::from_parts(
                Frontend::Service,
                TOOL_WORKERS_SUPPORTED,
                LOCAL_INFERENCE_COMPILED,
                LOCAL_SPEECH_COMPILED,
            )
        );
        // The default desktop gate: a spawner, neither engine linked.
        #[cfg(all(
            not(any(target_os = "android", target_os = "ios")),
            not(feature = "local-inference"),
            not(feature = "local-speech")
        ))]
        {
            let caps = Capabilities::detect(Frontend::Service);
            assert!(caps.extensions);
            assert!(!caps.local_inference);
            assert!(!caps.local_speech);
        }
    }
}

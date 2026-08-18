//! `GET /api/v1/health`.
//!
//! The one unauthenticated route. A client hits it before pairing to learn that
//! a core is there, what version it is, and whether it is fronted by a reverse
//! proxy, and the LAN discovery sweep uses it as its probe. It is also what a
//! paired client reads on select to seed the core bar without waiting for the
//! first `core.status` frame.
//!
//! Nothing here is a secret. `adminPasswordSet` is a boolean about
//! configuration, and `behindProxy` is a hint the pairing handshake verifies
//! independently: a forged value cannot complete pairing, because the
//! certificate binding is folded into the key confirmation.
//!
//! # Capabilities
//!
//! The response also carries [`Capabilities`], so a client asks what the core
//! it is talking to can do instead of inferring it from the device it is
//! rendering on. Those are different questions and confusing them gets the
//! interesting case backwards: a phone paired to a desktop core must still
//! offer extensions, scheduled prompts and stdio MCP servers, because all three
//! are properties of the core, not of the screen.

use axum::routing::get;
use axum::Router;

use crate::boot::Frontend;
use crate::contract::api::{CoreCapabilities, HealthResponse, HealthResponseStatus};

use super::http::Json;
use super::state::AppContext;

/// Whether this build can spawn a `deno` tool worker.
///
/// The one predicate behind both the `extensions` capability and the `deno`
/// binary requirement (`crate::requirements::compute::required_binary_kinds`),
/// deliberately shared rather than re-derived: those two must agree, and two
/// copies of a `cfg` cannot be tested into agreement on either host.
///
/// It mirrors the gate on `crate::extensions::worker::spawner`, which is
/// `#[cfg(not(any(target_os = "android", target_os = "ios")))]` because it
/// starts a DOWNLOADED interpreter, which is what Android's W^X policy and App
/// Store rule 2.5.2 forbid. `boot` reads the same `cfg` to decide whether there
/// is a `WorkerPool` at all.
///
/// A `cfg` rather than a value, where [`Frontend`] is deliberately a value: on
/// mobile the spawner module does not exist to be called, so this genuinely is
/// a compile-time fact. A desktop core booted as [`Frontend::Embedded`] can
/// still run tool workers, and reports so.
pub const TOOL_WORKERS_SUPPORTED: bool = cfg!(not(any(target_os = "android", target_os = "ios")));

/// Whether this build linked llama.cpp, so a model can be run in process.
///
/// `local-inference`, default OFF (`Cargo.toml`).
pub const LOCAL_INFERENCE_COMPILED: bool = cfg!(feature = "local-inference");

/// Whether this build linked sherpa-onnx, so speech can be run in process.
///
/// `local-speech`, default OFF (`Cargo.toml`).
pub const LOCAL_SPEECH_COMPILED: bool = cfg!(feature = "local-speech");

/// What this core can do, as opposed to what the device rendering its UI can do.
///
/// Every field is a property of the core that answered `/health`. A client
/// caches the block on connect and hides the affordances whose capability is
/// false; a control it leaves visible either works or fails with a worded
/// reason, which is the better half of that trade (see `stdio_mcp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Extension tools can actually RUN. Installing is not the question: an
    /// extension installs fine with no spawner and then every one of its tools
    /// fails at call time, which is the state worth hiding.
    pub extensions: bool,
    /// A language model can be run in process, so a local model selection is
    /// servable rather than a download with nothing to load it.
    pub local_inference: bool,
    /// STT and TTS can be run in process.
    pub local_speech: bool,
    /// A saved schedule will actually fire. The routes serve either way, so a
    /// prompt already in the table stays readable and deletable rather than
    /// invisible; what is false here is the arm loop.
    pub scheduled_prompts: bool,
    /// A local (spawned) MCP server can be started. Remote MCP is unaffected
    /// and is the half that works on a phone.
    pub stdio_mcp: bool,
    /// **Inbound only: this core can accept NEW paired clients.**
    ///
    /// It is not "this device can pair". A phone pairing OUT to a remote core
    /// is the client's business and is untouched by this flag; what is false on
    /// an embedded core is that anything can pair TO it, because there is no
    /// listener by design. So this gates minting a pairing code and revoking a
    /// device on THIS core, never the wizard that adds a remote one.
    pub pairing: bool,
    /// This core can replace its own binary.
    ///
    /// Named `core_update` rather than `update` because on mobile the CLIENT
    /// self-updates (the signed APK/IPA path) while the core cannot, being
    /// inside the app bundle. A bare `update` reads as a general capability and
    /// invites hiding the wrong affordance.
    pub core_update: bool,
}

impl Capabilities {
    /// The derivation, with every input explicit.
    ///
    /// Separate from [`detect`](Self::detect) so the mapping is exercised over
    /// both values of every input on a host where three of the four are pinned
    /// by the build. `detect` is then the only place a compile-time fact is
    /// read.
    pub fn from_parts(
        frontend: Frontend,
        tool_workers: bool,
        local_inference: bool,
        local_speech: bool,
    ) -> Self {
        // A listener, and therefore a second device that could reach it, is what
        // these four have in common. `boot` skips TLS material entirely for an
        // embedded frontend, so there is nothing to pair against; it does not
        // start the scheduler's arm loop; it declares stdio MCP unsupported; and
        // the updater helper is checked by the DESKTOP frontend rather than by
        // `boot`, so a service core is exactly the core that has one.
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

    /// The wire shape, generated from `@loca/shared`.
    ///
    /// Seven bools copied across is where a transposition hides silently, and
    /// the compiler cannot help: every field has the same type, so any
    /// permutation compiles. `each_field_reaches_its_own_wire_field` sets one
    /// at a time and asserts exactly one arrives.
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

    /// The served `capabilities`, over a context with `caps` and `pin`.
    ///
    /// `pin` is a parameter rather than a fixture default on purpose: the two
    /// callers below cross it against the capabilities deliberately, and a test
    /// that took whatever the fixture happened to hold would be asserting the
    /// fixture rather than the route. See the pair of tests that use it.
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

    /// The route must serve the CONTEXT's capabilities, not a fresh derivation.
    ///
    /// Deriving in the handler is the tempting shortcut, because it compiles
    /// without touching `AppContext` at all. It is also green and silently
    /// wrong on every embedded core, and no other test here sees it: everything
    /// else in this module tests `Capabilities` as a value, never the route.
    /// This drives the shipped in-process path end to end, so a handler that
    /// ignores `ctx.capabilities` fails.
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

    /// `tls_pin` must have NO say in what this core reports it can do.
    ///
    /// Two tests rather than one, and they cross `tls_pin` against the
    /// capabilities in opposite directions on purpose. The refused shortcut is
    /// deriving the block from `ctx.tls_pin.is_none()`, which needs no field on
    /// `AppContext` at all; per D-147 a refusal argued in prose is undefended
    /// until a test defends it.
    ///
    /// This half is the REAL configuration and the real bug. `boot` writes
    /// `behind_proxy || frontend.is_embedded()`, so a proxied DESKTOP core has
    /// no pin of its own: the terminating proxy presents a real certificate and
    /// pairing folds none. Under the shortcut it would report itself unable to
    /// pair, schedule prompts, spawn a stdio server or update. It has a
    /// listener and does all four.
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

    /// The converse crossing, and the reason the pair exists rather than one.
    ///
    /// A pin present on an embedded core is a combination `boot` never
    /// produces, so this is synthetic and says so. It is here because the test
    /// above catches the shortcut only while `context_with` happens to default
    /// `tls_pin` to `None`: flip that default and the derivation would agree
    /// with the frontend there and slip through. This one fails in exactly that
    /// case, so between them no fixture default can make both pass.
    ///
    /// **An assertion that passes because of an unrelated fixture default is
    /// not testing what its name says.** That is D-147 one layer down, and it
    /// is why the pin is a parameter here rather than inherited.
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
    ///
    /// Generated from `@loca/shared`, so this guards the regeneration rather
    /// than the hand-written half: a rename on the TypeScript side that nobody
    /// carried into the client would land here first.
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
        // And keeps the three the frontend has no say over. This half is what
        // stops the derivation collapsing into "embedded means nothing works":
        // a desktop process booted embedded still spawns tool workers.
        assert!(caps.extensions);
        assert!(caps.local_inference);
        assert!(caps.local_speech);
    }

    /// The three build-decided flags, one at a time.
    ///
    /// Three bools in a row is where a transposition hides, and a transposition
    /// is invisible on any host that has all three the same. Each case turns
    /// exactly one off and asserts the other two survive, so swapping any pair
    /// of arguments in `from_parts` fails.
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

    /// The frontend must not reach the three build-decided flags.
    ///
    /// Its own four are covered above; this is the other direction, and it is
    /// the one that would let `Frontend::Embedded` silently mean "no
    /// extensions" on a desktop build, which is false and would hide a working
    /// feature.
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

    /// Every domain field reaches its OWN wire field.
    ///
    /// Seven bools of the same type means any permutation of `to_wire`
    /// compiles, and a permutation is a core telling a client it can do
    /// something it cannot. Each case turns exactly one on and reads exactly one
    /// off, so a swap of any pair fails. The `false` half matters as much as the
    /// `true` half: without it, `to_wire` returning all-true would pass.
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

    /// `detect` must PASS ON the frontend it is given.
    ///
    /// Without this, `detect` hardcoding `Frontend::Service` survives every
    /// other test here, because every other one either calls `from_parts`
    /// directly or calls `detect(Frontend::Service)`. That mutation is the
    /// exact bug this whole design exists to prevent: a core that cannot pair,
    /// schedule or update reporting that it can.
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

    /// `detect` reads the build, and the build this test runs under is known.
    ///
    /// Tier 2, stated rather than implied: this pins that `detect` feeds the
    /// constants to `from_parts` in the right ORDER, using the values the
    /// default desktop gate actually has. What no test on one host can prove is
    /// that each `cfg` expression itself names the right thing, because they are
    /// compile-time. `deno task mobile-check` compiling both mobile triples and
    /// the constants living beside the gates they mirror are what cover that.
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
        // The default desktop gate: a spawner, and neither engine linked. If
        // this ever fails, the gate is running with a feature on and the two
        // engine flags below stop being meaningful rather than the code being
        // wrong.
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

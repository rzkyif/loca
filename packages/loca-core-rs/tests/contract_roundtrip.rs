//! Round-trips every fixture in `tests/fixtures/contract.json` through the
//! generated wire types: deserialize the JSON the contract says is valid, then
//! serialize it back and compare against what Zod itself parsed it to.
//!
//! This is the guard on the generated bridge. A normalisation pass that silently
//! degrades a tagged enum to `#[serde(untagged)]` still compiles, still passes
//! `cargo check`, and only shows up here or in production.
//!
//! The fixtures and the types come from the same `deno task contract` run, so
//! they cannot drift from each other; the staleness check keeps both in step
//! with `@loca/shared`.

// A test's job here is to fail loudly on a bad fixture, so the crate's
// unwrap/expect/panic restrictions do not apply.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use loca_core::contract::{api, inbound};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

#[derive(serde::Deserialize)]
struct Fixture {
    #[serde(rename = "type")]
    ty: String,
    value: Value,
    /// Zod's parsed output, which is the oracle. It differs from `value`
    /// wherever the schema applies a `.default()`.
    expected: Value,
}

fn roundtrip<T: DeserializeOwned + Serialize>(fixture: &Fixture) {
    let parsed: T = serde_json::from_value(fixture.value.clone()).unwrap_or_else(|e| {
        panic!(
            "{}: deserialize failed: {e}\ninput: {}",
            fixture.ty, fixture.value
        )
    });
    let back = serde_json::to_value(&parsed)
        .unwrap_or_else(|e| panic!("{}: serialize failed: {e}", fixture.ty));
    assert_eq!(
        back, fixture.expected,
        "{}: round trip changed the value\ninput:    {}\nexpected: {}\nactual:   {}",
        fixture.ty, fixture.value, fixture.expected, back
    );
}

#[test]
fn every_fixture_round_trips() {
    let raw = include_str!("fixtures/contract.json");
    let fixtures: Vec<Fixture> = serde_json::from_str(raw).expect("fixtures parse");
    assert!(!fixtures.is_empty(), "no fixtures");

    for fixture in &fixtures {
        match fixture.ty.as_str() {
            "api::ServerToClientFrame" => roundtrip::<api::ServerToClientFrame>(fixture),
            "inbound::ChatStartWs" => roundtrip::<inbound::ChatStartWs>(fixture),
            "inbound::ChatSubscribeWs" => roundtrip::<inbound::ChatSubscribeWs>(fixture),
            "inbound::ToolPermissionResponse" => {
                roundtrip::<inbound::ToolPermissionResponse>(fixture)
            }
            "inbound::ToolAskUserResponse" => roundtrip::<inbound::ToolAskUserResponse>(fixture),
            "inbound::PairingCodeRequest" => roundtrip::<inbound::PairingCodeRequest>(fixture),
            "inbound::MessageInput" => roundtrip::<inbound::MessageInput>(fixture),
            "inbound::ScheduledPromptDraft" => roundtrip::<inbound::ScheduledPromptDraft>(fixture),
            other => panic!("no match arm for fixture type {other}; add one here"),
        }
    }
}

/// The point of generating the inbound half from Zod rather than from the
/// TypeScript: the constraints survive into `Deserialize`. If these start
/// passing, the bridge has quietly stopped validating and every `.min()` /
/// `.regex()` in `@loca/shared` is decorative on the Rust side.
#[test]
fn inbound_types_enforce_the_zod_constraints() {
    let empty_stream_id = serde_json::json!({
        "kind": "chat.start",
        "streamId": "",
        "sessionId": "sess1",
    });
    assert!(
        serde_json::from_value::<inbound::ChatStartWs>(empty_stream_id).is_err(),
        "streamId is .min(1) in Zod; an empty one must not deserialize"
    );

    let unknown_kind = serde_json::json!({
        "kind": "chat.definitely_not_a_kind",
        "streamId": "s1",
    });
    assert!(
        serde_json::from_value::<api::ServerToClientFrame>(unknown_kind).is_err(),
        "an unknown frame kind must not deserialize"
    );
}

/// The other half of the policy: an inbound body must IGNORE a field it does
/// not know, because a newer client adding one has to be processed on the
/// fields the core does know. `ts-json-schema-generator` writes
/// `additionalProperties: false` for every interface, so the generator strips
/// `deny_unknown_fields` from `*Request` types; without that strip this body is
/// a 400 where the TypeScript returned 200.
///
/// Domain types keep their strictness, which is what makes the untagged unions
/// distinguishable by field set, so this is deliberately scoped to bodies.
#[test]
fn inbound_request_bodies_ignore_unknown_fields() {
    let with_extra = serde_json::json!({
        "categoryId": "cache",
        "aFieldFromANewerClient": true,
    });
    let parsed: api::ClearStorageCategoryRequest =
        serde_json::from_value(with_extra).expect("an unknown field must not reject the body");
    assert_eq!(parsed.category_id, "cache");

    // The constraint above still applies: a REQUIRED field missing is an error.
    assert!(
        serde_json::from_value::<api::ClearStorageCategoryRequest>(serde_json::json!({})).is_err(),
        "a missing required field must still fail"
    );
}

/// A credential must not survive a `{:?}`.
///
/// typify derives `Debug` on everything it emits, so one `tracing::debug!(?x)`
/// anywhere in the core is enough to write a plaintext credential to
/// `core.log`. The generator redacts them; this is the assertion that the
/// redaction is still there after a regeneration, checked by `cargo test`
/// rather than only by the generator that produced it.
///
/// `PakeFinishResponse.token` is the strongest case in the contract: it is the
/// bearer token the client uses for every subsequent request, and it is a plain
/// `string` with no Zod constraint, so it gets no newtype wrapper to hang a
/// redacting `Debug` on. The log scrubber cannot cover for it either, because a
/// bare token in a struct dump has no surrounding key to match against.
#[test]
fn a_credential_field_is_redacted_in_debug_output() {
    let response = api::PakeFinishResponse {
        client_id: "client-1".to_string(),
        confirm_s: "confirm".to_string(),
        core_version: "0.1.0".to_string(),
        token: "tok_live_do_not_log_me".to_string(),
    };
    let rendered = format!("{response:?}");
    assert!(
        !rendered.contains("tok_live_do_not_log_me"),
        "the bearer token reached a Debug rendering: {rendered}"
    );
    assert!(
        rendered.contains("<REDACTED>"),
        "expected the redaction marker: {rendered}"
    );
    // The rest of the struct must still be legible, or the redaction has cost
    // the diagnostic value the Debug exists for.
    assert!(
        rendered.contains("client-1"),
        "non-secret fields must still print: {rendered}"
    );
}

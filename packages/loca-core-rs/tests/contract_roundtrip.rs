//! Round-trips every fixture in `tests/fixtures/contract.json` through the
//! generated wire types: deserialize the JSON the contract says is valid, then
//! serialize it back and compare against what Zod itself parsed it to.
//!
//! The guard on the generated bridge: a normalisation pass that silently
//! degrades a tagged enum to `#[serde(untagged)]` still compiles and only shows
//! up here or in production. Fixtures and types come from the same
//! `deno task contract` run, so they cannot drift from each other.

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
    /// Zod's parsed output, which differs from `value` wherever the schema
    /// applies a `.default()`.
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

/// The point of generating the inbound half from Zod: the constraints survive
/// into `Deserialize`. If these start passing, every `.min()` / `.regex()` in
/// `@loca/shared` is decorative on the Rust side.
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

/// An inbound body must IGNORE a field it does not know, so a newer client
/// adding one is still processed on the fields the core does know. The generator
/// strips `deny_unknown_fields` from `*Request` types for this. Domain types
/// keep their strictness, which is what makes the untagged unions
/// distinguishable by field set.
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

/// A credential must not survive a `{:?}`. `Debug` is derived on everything the
/// generator emits, so one `tracing::debug!(?x)` writes a plaintext credential
/// to `core.log`; this asserts the redaction survives a regeneration.
///
/// `PakeFinishResponse.token` is the strongest case: a plain `string` with no
/// Zod constraint, so no newtype wrapper to hang a redacting `Debug` on, and the
/// log scrubber cannot help because a bare token in a struct dump has no
/// surrounding key to match.
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
    // The rest must stay legible, or the redaction costs the diagnostic value
    // the Debug exists for.
    assert!(
        rendered.contains("client-1"),
        "non-secret fields must still print: {rendered}"
    );
}

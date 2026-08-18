//! The request and response shapes `@loca/shared` does not declare, and the
//! two it declares wrongly.
//!
//! D-031's precedent: an interim hand-written shape is allowed where the
//! contract has no counterpart, provided the single declaration site says so
//! and the request is on the queue. Six route bodies in this slice have no
//! `@loca/shared` type at all (their shapes exist only in the client wrapper),
//! and `ToolFilterResponse` declares one field where the handler returns three.
//!
//! Everything the contract DOES declare is imported from `contract::api` at the
//! route site and is not restated here.

use serde::{Deserialize, Serialize};

use crate::contract::api::{ToolDescriptor, UndeclaredPolicy};

/// `POST /extensions/{id}/tools/{tool}/always-available`.
///
/// TODO(F2): no `@loca/shared` type; the shape is `{ value: boolean }` in
/// `routes/extensions.ts:228`.
#[derive(Debug, Clone, Deserialize)]
pub struct AlwaysAvailableRequest {
    pub value: bool,
}

/// `POST /extensions/{id}/undeclared-policy`.
///
/// TODO(F2): no `@loca/shared` type. `UndeclaredPolicy` itself IS generated,
/// so only the envelope is hand-written, and an unknown policy string is
/// refused by the enum's deserializer rather than by a hand-written check.
#[derive(Debug, Clone, Deserialize)]
pub struct UndeclaredPolicyRequest {
    pub policy: UndeclaredPolicy,
}

/// `POST /extensions/builtin-install`.
///
/// TODO(F2): no `@loca/shared` type. `queued` tells the client the install is
/// waiting on the `deno` runtime download rather than having failed.
#[derive(Debug, Clone, Serialize)]
pub struct BuiltinInstallResponse {
    pub queued: bool,
}

/// `POST /extensions/rescan`.
///
/// TODO(F2): no `@loca/shared` type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RescanResponse {
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
}

/// `POST /extensions/reindex`.
///
/// TODO(F2): no `@loca/shared` type. `skipped` is present only when the embed
/// model is not downloaded, which is a clean skip rather than an error: the
/// requirements flow prompts for that download separately.
#[derive(Debug, Clone, Serialize)]
pub struct ReindexResponse {
    pub embedded: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

/// The three toggle routes' body.
///
/// TODO(F2): no `@loca/shared` type; the handlers return `{ ok: true }`.
#[derive(Debug, Clone, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

impl OkResponse {
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

/// `POST /extensions/filter`.
///
/// NOT the generated `ToolFilterResponse`, which declares `candidates` alone
/// while the handler returns all three fields of `Phase1Result`
/// (`tool-filter.ts:40-48`) and the client casts around the mismatch
/// (`loca-client/src/ui/lib/core/extensions.ts:134`). Emitting the generated
/// shape would drop two fields the oracle sends, so conformance would fail on a
/// shape bug that lives in `@loca/shared`. Filed with F2; when it is fixed this
/// struct goes and the import changes.
#[derive(Debug, Clone, Serialize)]
pub struct Phase1Response {
    pub candidates: Vec<ToolDescriptor>,
    #[serde(rename = "alwaysAvailable")]
    pub always_available: Vec<ToolDescriptor>,
    #[serde(rename = "nameMatched")]
    pub name_matched: Vec<ToolDescriptor>,
}

/// Read an optional string discriminator the way JavaScript's truthiness does.
///
/// D-043 by name. Every optional string on `/download` and `/update` is a
/// truthiness test in the handler, so `""` must read as ABSENT rather than as
/// "the empty version". The field stays private behind this function so a new
/// discriminator cannot be added with the other semantics by accident.
pub fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|raw| !raw.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_discriminator_reads_as_an_omitted_one() {
        // `{"version": ""}` and an absent `version` are the same request on the
        // oracle, because the handler tests truthiness. A port that passed `""`
        // through would look up a version nobody published.
        assert_eq!(present(Some("")), None);
        assert_eq!(present(None), None);
        assert_eq!(present(Some("1.2.3")), Some("1.2.3"));
        // Whitespace is NOT empty: the oracle's test is truthiness, not a trim,
        // and inventing a trim here would accept a request the oracle refuses.
        assert_eq!(present(Some(" ")), Some(" "));
    }

    #[test]
    fn a_clean_reindex_skip_is_visible_and_a_real_one_is_not() {
        // `skipped` is present only when it is true, matching the oracle's two
        // object literals. A always-present `"skipped": false` is a diff on
        // every successful reindex.
        let skipped = serde_json::to_value(ReindexResponse {
            embedded: 0,
            skipped: true,
        })
        .unwrap();
        assert_eq!(skipped["skipped"], serde_json::json!(true));
        let ran = serde_json::to_value(ReindexResponse {
            embedded: 3,
            skipped: false,
        })
        .unwrap();
        assert!(ran.get("skipped").is_none(), "got {ran}");
        assert_eq!(ran["embedded"], serde_json::json!(3));
    }

    #[test]
    fn the_filter_response_carries_all_three_lists_the_oracle_sends() {
        let body = serde_json::to_value(Phase1Response {
            candidates: Vec::new(),
            always_available: Vec::new(),
            name_matched: Vec::new(),
        })
        .unwrap();
        for key in ["candidates", "alwaysAvailable", "nameMatched"] {
            assert!(body.get(key).is_some(), "{key} is missing from {body}");
        }
    }

    #[test]
    fn an_unknown_undeclared_policy_is_refused_by_the_deserializer() {
        assert!(serde_json::from_str::<UndeclaredPolicyRequest>(r#"{"policy":"ask"}"#).is_ok());
        assert!(serde_json::from_str::<UndeclaredPolicyRequest>(r#"{"policy":"deny"}"#).is_ok());
        assert!(
            serde_json::from_str::<UndeclaredPolicyRequest>(r#"{"policy":"allow"}"#).is_err(),
            "a third policy would be persisted and then matched by no branch"
        );
    }

    #[test]
    fn a_non_boolean_always_available_value_is_refused() {
        assert!(serde_json::from_str::<AlwaysAvailableRequest>(r#"{"value":true}"#).is_ok());
        assert!(serde_json::from_str::<AlwaysAvailableRequest>(r#"{"value":"yes"}"#).is_err());
        assert!(serde_json::from_str::<AlwaysAvailableRequest>("{}").is_err());
    }
}

//! Route bodies `@loca/shared` does not declare (TODO(F2): their shapes exist
//! only in the client wrapper), plus the filter response it declares wrongly.
//! Everything the contract DOES declare is imported from `contract::api` at the
//! route site.

use serde::{Deserialize, Serialize};

use crate::contract::api::{ToolDescriptor, UndeclaredPolicy};

/// `POST /extensions/{id}/tools/{tool}/always-available`.
#[derive(Debug, Clone, Deserialize)]
pub struct AlwaysAvailableRequest {
    pub value: bool,
}

/// `POST /extensions/{id}/undeclared-policy`. An unknown policy string is
/// refused by the enum's deserializer.
#[derive(Debug, Clone, Deserialize)]
pub struct UndeclaredPolicyRequest {
    pub policy: UndeclaredPolicy,
}

/// `POST /extensions/builtin-install`. `queued` means the install is waiting
/// on the `deno` runtime download rather than having failed.
#[derive(Debug, Clone, Serialize)]
pub struct BuiltinInstallResponse {
    pub queued: bool,
}

/// `POST /extensions/rescan`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RescanResponse {
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
}

/// `POST /extensions/reindex`. `skipped` is present only when the embed model
/// is not downloaded, a clean skip rather than an error.
#[derive(Debug, Clone, Serialize)]
pub struct ReindexResponse {
    pub embedded: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

/// The three toggle routes' body.
#[derive(Debug, Clone, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

impl OkResponse {
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

/// `POST /extensions/filter`. NOT the generated `ToolFilterResponse`, which
/// declares `candidates` alone while the handler returns all three lists (the
/// client casts around the mismatch). Filed with F2; goes when it is fixed.
#[derive(Debug, Clone, Serialize)]
pub struct Phase1Response {
    pub candidates: Vec<ToolDescriptor>,
    #[serde(rename = "alwaysAvailable")]
    pub always_available: Vec<ToolDescriptor>,
    #[serde(rename = "nameMatched")]
    pub name_matched: Vec<ToolDescriptor>,
}

/// Read an optional string discriminator the way JavaScript truthiness does:
/// `""` is ABSENT rather than "the empty version". No trim.
pub fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|raw| !raw.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_discriminator_reads_as_an_omitted_one() {
        assert_eq!(present(Some("")), None);
        assert_eq!(present(None), None);
        assert_eq!(present(Some("1.2.3")), Some("1.2.3"));
        assert_eq!(present(Some(" ")), Some(" "));
    }

    #[test]
    fn a_clean_reindex_skip_is_visible_and_a_real_one_is_not() {
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
    fn the_filter_response_carries_all_three_lists() {
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

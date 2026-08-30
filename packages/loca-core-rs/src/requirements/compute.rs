//! The single authoritative "what does this configuration need on disk"
//! computation. Presence is a purely local check; only the size and version
//! hints reach the network, and only behind `probe_sizes` (the confirm modal),
//! so the always-on snapshot makes no outbound request.

use std::collections::{HashMap, HashSet};

use crate::contract::api::{
    BinaryKind, DownloadStatus, RequiredFile, RequiredFileGroup, RequiredFileType,
    RequirementGroup, RequirementsSnapshot,
};
use crate::error::Result;
use crate::models::Models;
use crate::settings::SettingsRecord;
use crate::transport::health::TOOL_WORKERS_SUPPORTED;

use super::deps::Binaries;

/// The synthetic `source` prefix that lets a binary sit in the same list as a
/// model file.
pub const BINARY_SOURCE_PREFIX: &str = "binary:";

pub fn binary_source(kind: BinaryKind) -> String {
    format!("{BINARY_SOURCE_PREFIX}{kind}")
}

/// The kind a `binary:<kind>` source names, or `None` for anything else.
pub fn binary_source_to_kind(source: &str) -> Option<BinaryKind> {
    source.strip_prefix(BINARY_SOURCE_PREFIX)?.parse().ok()
}

/// The binary kinds this configuration requires: `deno` alone (`llama-server`
/// is in-process). A build with no tool-worker spawner requires nothing, since
/// a row for an executable it cannot run would park the gate on "missing"
/// forever (and mobile forbids downloading executables outright).
pub fn required_binary_kinds() -> &'static [BinaryKind] {
    required_binary_kinds_where(TOOL_WORKERS_SUPPORTED)
}

/// [`required_binary_kinds`] with its one input explicit, for tests.
fn required_binary_kinds_where(tool_workers: bool) -> &'static [BinaryKind] {
    if tool_workers {
        crate::downloads::binaries::installer::INSTALLABLE_KINDS
    } else {
        &[]
    }
}

/// Whether `kind` can never be installed on this host (`unavailable`: visible,
/// non-blocking). Always false today: the triple data lives in a fetched
/// manifest, which the always-on snapshot must not reach for, and the pinned
/// deno release covers every triple this core builds for. A wrong answer
/// surfaces as a retryable install failure, not a stuck gate.
fn unavailable_on_triple(kind: BinaryKind) -> bool {
    match kind {
        BinaryKind::Deno => false,
        // Explicit so widening `required_binary_kinds` is a compile error here.
        BinaryKind::LlamaServer => false,
    }
}

/// Everything the snapshot reads.
pub struct Sources<'a> {
    pub settings: &'a SettingsRecord,
    pub models: &'a Models,
    pub binaries: &'a dyn Binaries,
}

/// The full required-files list plus the missing subset. Order is observable:
/// models in `required_model_refs` order, then binaries.
pub async fn compute(sources: Sources<'_>, probe_sizes: bool) -> Result<RequirementsSnapshot> {
    let mut required = models(&sources, probe_sizes).await?;
    required.extend(binaries(&sources, probe_sizes).await);
    let missing = required
        .iter()
        .filter(|r| !r.present && r.unavailable != Some(true))
        .cloned()
        .collect();
    Ok(RequirementsSnapshot { missing, required })
}

async fn models(sources: &Sources<'_>, probe_sizes: bool) -> Result<Vec<RequiredFile>> {
    // Deduped by source, keeping the first group seen.
    let mut seen = HashSet::new();
    let refs: Vec<_> = crate::models::required::required_model_refs(sources.settings)
        .into_iter()
        .filter(|r| seen.insert(r.source.clone()))
        .collect();

    let specs: Vec<String> = refs.iter().map(|r| r.source.clone()).collect();
    let plans = sources
        .models
        .deps()
        .downloads
        .probe(&specs, probe_sizes)
        .await?;
    let present: HashMap<&str, &crate::contract::api::DownloadPlan> = plans
        .iter()
        .map(|plan| (plan.source.as_str(), plan))
        .collect();

    // A failed download lives on as an `Error` row keyed by source; surfacing
    // it makes the row a retryable error, and a retry clears it by itself.
    let mut download_error: HashMap<String, String> = HashMap::new();
    for entry in sources.models.deps().downloads.models_snapshot() {
        if entry.status == DownloadStatus::Error {
            download_error.insert(
                entry.source.clone(),
                entry.error.unwrap_or_else(|| "download failed".to_string()),
            );
        }
    }

    Ok(refs
        .into_iter()
        .map(|r| {
            let plan = present.get(r.source.as_str());
            let is_present = plan.is_some_and(|plan| plan.already_have);
            RequiredFile {
                // Hints are noise on a present row.
                error: (!is_present)
                    .then(|| download_error.get(&r.source).cloned())
                    .flatten(),
                size_hint: (!is_present)
                    .then(|| plan.and_then(|p| p.size_hint))
                    .flatten(),
                group: group_of(r.group),
                present: is_present,
                source: r.source,
                type_: RequiredFileType::Model,
                unavailable: None,
                version: None,
            }
        })
        .collect())
}

async fn binaries(sources: &Sources<'_>, probe_sizes: bool) -> Vec<RequiredFile> {
    let kinds = required_binary_kinds();
    // An unreadable manifest must not take the model rows down with it.
    let status = sources.binaries.list().await.unwrap_or_default();
    let present: HashMap<BinaryKind, bool> = kinds
        .iter()
        .map(|kind| {
            let installed = status
                .iter()
                .find(|s| s.kind == *kind)
                .is_some_and(is_present);
            (*kind, installed)
        })
        .collect();

    let wanted: Vec<BinaryKind> = kinds
        .iter()
        .copied()
        .filter(|kind| !present[kind] && !unavailable_on_triple(*kind))
        .collect();
    let probes = if probe_sizes && !wanted.is_empty() {
        sources.binaries.probe(&wanted).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    kinds
        .iter()
        .map(|kind| {
            let is_present = present[kind];
            let unavailable = !is_present && unavailable_on_triple(*kind);
            let probe = (!is_present)
                .then(|| probes.iter().find(|p| p.kind == *kind))
                .flatten();
            RequiredFile {
                // A recorded install failure makes this retryable rather than
                // a reasonless, permanently missing entry.
                error: (!is_present && !unavailable)
                    .then(|| sources.binaries.failure(*kind))
                    .flatten(),
                group: RequiredFileGroup::Binary,
                present: is_present,
                size_hint: probe.and_then(|p| p.size_bytes),
                source: binary_source(*kind),
                type_: RequiredFileType::Binary,
                // `Some(true)` or absent, never `Some(false)`.
                unavailable: unavailable.then_some(true),
                version: probe
                    .map(|p| p.version.clone())
                    .filter(|v| v != UNKNOWN_VERSION),
            }
        })
        .collect()
}

/// The version a probe reports when it could not resolve one; dropped rather
/// than shown.
const UNKNOWN_VERSION: &str = "unknown";

/// Installed AND the last install aimed for this device's variant. Compares
/// `target` (what the install aimed for), not `variant` (what landed): a
/// degraded fallback must not read as perpetually missing. Unknown on either
/// side counts as present, so the gate never blocks on an upgrade it cannot name.
fn is_present(status: &crate::contract::api::BinaryStatus) -> bool {
    if !status.installed {
        return false;
    }
    match (status.desired_variant, status.target) {
        (Some(desired), Some(target)) => target == desired,
        _ => true,
    }
}

fn group_of(group: RequirementGroup) -> RequiredFileGroup {
    match group {
        RequirementGroup::Llm => RequiredFileGroup::Llm,
        RequirementGroup::Stt => RequiredFileGroup::Stt,
        RequirementGroup::Tts => RequiredFileGroup::Tts,
        RequirementGroup::Embed => RequiredFileGroup::Embed,
    }
}

/// The `RequirementGroup` a missing model row belongs to, for the download
/// route's per-group `ensure`.
pub fn group_from_file(group: RequiredFileGroup) -> Option<RequirementGroup> {
    match group {
        RequiredFileGroup::Llm => Some(RequirementGroup::Llm),
        RequiredFileGroup::Stt => Some(RequirementGroup::Stt),
        RequiredFileGroup::Tts => Some(RequirementGroup::Tts),
        RequiredFileGroup::Embed => Some(RequirementGroup::Embed),
        RequiredFileGroup::Binary => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_core_with_no_tool_worker_spawner_requires_no_binaries() {
        assert!(required_binary_kinds_where(false).is_empty());
    }

    #[test]
    fn a_core_that_can_spawn_a_tool_worker_requires_the_interpreter() {
        assert_eq!(
            required_binary_kinds_where(true),
            crate::downloads::binaries::installer::INSTALLABLE_KINDS
        );
        assert!(required_binary_kinds_where(true).contains(&BinaryKind::Deno));
    }

    #[test]
    fn the_binary_requirement_and_the_extensions_capability_read_one_predicate() {
        let caps = crate::transport::health::Capabilities::detect(crate::boot::Frontend::Service);
        assert_eq!(caps.extensions, !required_binary_kinds().is_empty());
    }
}

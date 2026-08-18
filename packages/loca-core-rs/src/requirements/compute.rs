//! The single authoritative "what does this configuration need on disk"
//! computation.
//!
//! Ports `services/requirements.ts:30-136`. The client renders the `missing`
//! subset as one pending-downloads popup and gates the app on it.
//!
//! # Consent
//!
//! Presence is a purely LOCAL check and always is. The size and version hints
//! are the only things that reach the network, and they are gated behind
//! `probe_sizes`, which only `?probe=1` sets and which the client sends only
//! when the user opens the confirm modal. So the always-on snapshot (client
//! connect, settings broadcast, download edge) makes no outbound request, which
//! is what keeps this inside the repo's absolute no-non-consented-network rule.

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

/// The synthetic `source` prefix that lets a sidecar binary appear in the same
/// list as a HuggingFace model file. `domain/model.ts:279`.
pub const BINARY_SOURCE_PREFIX: &str = "binary:";

pub fn binary_source(kind: BinaryKind) -> String {
    format!("{BINARY_SOURCE_PREFIX}{kind}")
}

/// The kind a `binary:<kind>` source names, or `None` for anything else.
///
/// Fallible where `binarySourceToKind` (`domain/model.ts:287`) is a bare cast:
/// the cast is only sound because the caller filters on `type === "binary"`
/// first, and an unparseable kind here means this core built a row it cannot
/// read back.
pub fn binary_source_to_kind(source: &str) -> Option<BinaryKind> {
    source.strip_prefix(BINARY_SOURCE_PREFIX)?.parse().ok()
}

/// The binary kinds this core's configuration requires.
///
/// `requiredBinaryKinds` (`domain/model.ts:358-362`) answers `deno` plus
/// `llama-server`, plus `loca-core-speech` when local speech is on. This core
/// answers `deno` alone, and the difference is architectural rather than
/// temporary: `llama-server` and `loca-core-speech` are absorbed in-process by
/// Stage 3, so nothing here can execute either one. Offering them would put two
/// permanently unsatisfiable rows in `missing`, which is the client's app-level
/// pending gate, and the app would never leave the pending state.
///
/// Settings are therefore not consulted, where the oracle consults them for the
/// speech kind.
///
/// # A core that cannot spawn a worker requires nothing
///
/// `deno` is here for exactly one reason: it is the interpreter every extension
/// tool worker runs in. On a build with no spawner
/// ([`TOOL_WORKERS_SUPPORTED`]) there is nothing that could execute it, and
/// asking for it anyway is wrong three times over. It is a request to download
/// an **executable**, which Android's W^X policy and App Store rule 2.5.2 both
/// forbid outright. The user cannot satisfy it in any useful sense. And because
/// this snapshot also feeds the aggregate core status and the greetings gate,
/// the row parks the core bar on "something is missing" with no action that
/// clears it, whatever the client chooses to render.
///
/// So it is not produced, rather than produced and refused at the download.
/// The capability block on `/health` hides the affordance; this is the half
/// underneath it, and neither substitutes for the other.
pub fn required_binary_kinds() -> &'static [BinaryKind] {
    required_binary_kinds_where(TOOL_WORKERS_SUPPORTED)
}

/// [`required_binary_kinds`] with its one input explicit, so the branch a
/// desktop build never takes is still exercised.
fn required_binary_kinds_where(tool_workers: bool) -> &'static [BinaryKind] {
    if tool_workers {
        crate::downloads::binaries::installer::INSTALLABLE_KINDS
    } else {
        &[]
    }
}

/// Whether `kind` can never be installed on this host, so it is marked
/// `unavailable` (visible, non-blocking) rather than perpetually `missing`.
///
/// **Always false today, and the reason is data rather than laziness.** The
/// oracle (`domain/model.ts:377-381`) looks the triple up in a compiled-in asset
/// map; the Rust core's equivalent data lives in a fetched manifest, and
/// consulting it would put an outbound request on the always-on snapshot. With
/// `deno` the only required kind, the lookup is answerable without the manifest:
/// the pinned deno release publishes an asset for all six triples this core
/// builds for (`domain/model.ts:335-342`).
///
/// What would change it: a required kind whose upstream skips a triple. That
/// does not go unnoticed if it happens, because the install fails and
/// `Binaries::failure` renders the row as a retryable error with the reason
/// attached, rather than as a silent, sizeless, permanently missing entry. So
/// the cost of being wrong here is a worse message, not a stuck gate.
fn unavailable_on_triple(kind: BinaryKind) -> bool {
    match kind {
        BinaryKind::Deno => false,
        // Unreachable while `required_binary_kinds` is deno-only. Kept explicit
        // so widening that list is a compile error here rather than a silent
        // "available everywhere".
        BinaryKind::LlamaServer | BinaryKind::LocaCoreSpeech => false,
    }
}

/// Everything the snapshot reads.
pub struct Sources<'a> {
    pub settings: &'a SettingsRecord,
    pub models: &'a Models,
    pub binaries: &'a dyn Binaries,
}

/// The full required-files list plus the missing subset.
///
/// Order is observable: models in `required_model_refs` order, then binaries in
/// `required_binary_kinds` order.
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
    // Deduped by source, keeping the first group seen. The embed and TTS base
    // files are fixed, but an LLM and an STT selection could in principle name
    // the same file, and a duplicated row would be offered for download twice.
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

    // A model download that failed lives on as an `Error` row in the queue,
    // keyed by the same source spec. Surfacing it makes a stalled download a
    // retryable error rather than a silent, perpetually-"downloading" gate.
    // Reading the LIVE queue is what keeps this self-consistent: a retry flips
    // the row off `Error` and the message clears on the next recompute, with
    // nothing to reset by hand.
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
                // Both hints are suppressed once the file is there: a present
                // row is not offered for download, so a size or an error on it
                // is noise the modal would have to filter out again.
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
    // `.catch(() => [])`: a manifest this core cannot read must not take the
    // whole snapshot down, because every model row in it is still correct.
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
                // A recorded install failure (upstream unresolvable, download
                // or extract errored) makes this a retryable error rather than
                // a sizeless, reasonless, permanently missing entry. Only
                // meaningful while the binary is still missing and installable.
                error: (!is_present && !unavailable)
                    .then(|| sources.binaries.failure(*kind))
                    .flatten(),
                group: RequiredFileGroup::Binary,
                present: is_present,
                size_hint: probe.and_then(|p| p.size_bytes),
                source: binary_source(*kind),
                type_: RequiredFileType::Binary,
                // `Some(true)` or absent, never `Some(false)`: the oracle writes
                // `unavailable || undefined`, so the key is omitted rather than
                // sent as false.
                unavailable: unavailable.then_some(true),
                version: probe
                    .map(|p| p.version.clone())
                    .filter(|v| v != UNKNOWN_VERSION),
            }
        })
        .collect()
}

/// The version a probe reports when it could not resolve one. Dropped rather
/// than shown, so the modal renders no version instead of the word "unknown".
const UNKNOWN_VERSION: &str = "unknown";

/// Present means installed AND the last install already AIMED for the variant
/// this device should have.
///
/// The comparison is against `target` (what the install aimed for), NOT
/// `variant` (what landed on disk). They differ only when the ideal was not
/// resolvable upstream and the install degraded, and comparing `variant` would
/// make that unavoidable fallback read as perpetually missing, re-offering the
/// same download in the popup forever. Ports `requirements.ts:97-102`.
///
/// When either side is unknown (offline, no manifest entry, an install from
/// before the field existed) a plain install counts as present, so the gate
/// never blocks on an upgrade it cannot even name.
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

    /// The whole point of the spawner gate: a core that cannot run a tool
    /// worker must not ask for the interpreter one would run in.
    ///
    /// Tier 2, stated rather than implied. `required_binary_kinds()` reads a
    /// `cfg`, so on a desktop host only the `true` branch is ever taken by the
    /// shipping call; this exercises both by taking the input explicitly. What
    /// no test on one host can prove is that `TOOL_WORKERS_SUPPORTED` itself
    /// names the right `cfg`, which is why it is defined once, beside the
    /// capability that has to agree with it, rather than copied here.
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

    /// The two must not drift into disagreeing, because they answer the same
    /// question for two different consumers: this one decides whether `deno` is
    /// offered for download, and the capability decides whether the client
    /// renders extensions at all. A core that offers the download while
    /// reporting `extensions: false` is asking for an executable it has just
    /// said it cannot use.
    #[test]
    fn the_binary_requirement_and_the_extensions_capability_read_one_predicate() {
        let caps = crate::transport::health::Capabilities::detect(crate::boot::Frontend::Service);
        assert_eq!(caps.extensions, !required_binary_kinds().is_empty());
    }
}

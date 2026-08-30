//! The on-device fit engine.
//!
//! Given detected hardware and the signed catalog, pick the best model for each
//! adaptive preset (Smallest / Half / Full) and decide which models fit. The
//! budget is TOTAL memory scaled by the catalog's `budgetFraction` less a safety
//! margin; a footprint is weights plus KV cache plus compute overhead plus the
//! vision module when the model ships one.
//!
//! The arithmetic is a bit-exact parity surface, so the operation order matters:
//! footprints stay `u64` throughout (every term is far below 2^53, and the cast
//! to the contract's `i64` happens once, where the answer leaves this file),
//! limits stay `f64` as `budget * fraction - margin`, the fit test is
//! `footprint as f64 <= limit`, and `quant_rank` is compared for exact equality.
//!
//! Tuning lives in the catalog: `budget_fraction`, `safety_margin_bytes`,
//! `primary_score`, `smallest_quality_floor_ref` and `tie_breakers` are read off
//! [`FitConfig`] at call time, so a mobile profile is a different value passed
//! at the route rather than a branch in this file.

use indexmap::IndexSet;

use crate::contract::api::{
    AppliedModelSettings, BucketRecommendation, CatalogModelView, GpuBackend, HardwareInfo,
    PresetBucket, QuantOption, RecommendationSet, RecommendationSetApplied,
    RecommendationSetBuckets,
};
use crate::contract::inbound::{
    CatalogModel, CatalogPayload, CatalogQuant, CatalogVariant, FitConfig, Sampling,
};

use super::catalog::{flat_models, primary_score};

/// fp16 K and V cache.
const BYTES_PER_KV_ELEM: u64 = 2;
const MAX_THREADS: i64 = 8;
const FULL_IDLE_UNLOAD_SECONDS: i64 = 300;

/// The fallback sampling for a model with no catalog tuning. Typed as the
/// CATALOG's `Sampling` so a model's own tuning and this fallback merge field by
/// field below.
fn default_sampling() -> Sampling {
    Sampling {
        temperature: 0.7,
        top_p: 0.8,
        top_k: 20,
        min_p: 0.0,
        // On by default and deliberately gentle: small local models loop
        // without it, and 0.8 leaves normal prose untouched.
        dry_multiplier: Some(0.8),
        presence_penalty: Some(0.0),
        repeat_penalty: 1.05,
        samplers: None,
    }
}

/// Desktop tuning, and the value a catalog that omits these fields resolves to.
/// `ctxCeiling` arrives as `Option` on the generated type, so the fallback lives
/// here rather than in serde.
const DESKTOP_COMPUTE_OVERHEAD_BYTES: u64 = 600_000_000;
const DESKTOP_CTX_CEILING: CtxCeiling = CtxCeiling {
    smallest: 8192,
    half: 16384,
    full: 32768,
};
const DESKTOP_CTX_LADDER: [u64; 4] = [32768, 16384, 8192, 4096];

/// Which device class the engine is fitting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitProfile {
    Desktop,
    Mobile,
}

/// The profile a Core running on this platform should use.
///
/// A fact about THIS process, not a field of `HardwareInfo`: putting it on the
/// wire would let a client assert it. Not `Frontend::Embedded` either, because a
/// desktop process booted embedded still has desktop memory.
pub fn fit_profile_for_os(os: &str) -> FitProfile {
    match os {
        "android" | "ios" => FitProfile::Mobile,
        _ => FitProfile::Desktop,
    }
}

/// This build's profile, from the target it was compiled for.
pub fn fit_profile() -> FitProfile {
    fit_profile_for_os(std::env::consts::OS)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CtxCeiling {
    pub smallest: u64,
    pub half: u64,
    pub full: u64,
}

/// A [`FitConfig`] with a profile applied and every field concrete, so nothing
/// downstream sees an `Option` or reaches for a constant.
#[derive(Clone, Debug)]
pub struct ResolvedFit {
    pub budget_half: f64,
    pub budget_full: f64,
    pub safety_margin_bytes: i64,
    pub smallest_quality_floor_ref: String,
    pub tie_breakers: Vec<String>,
    pub primary_score: crate::contract::inbound::ScoreSelector,
    pub compute_overhead_bytes: u64,
    pub ctx_ceiling: CtxCeiling,
    pub ctx_ladder: Vec<u64>,
    /// Whether this profile preselects vision and budgets for the projector.
    /// Never gates the projector path: see [`vision_bytes`].
    pub vision: bool,
}

/// Apply a profile's overrides to the catalog's base policy.
///
/// `Desktop` is the base unchanged, which is what makes adding a mobile profile
/// inert on every existing install: there is no desktop branch to get wrong,
/// because desktop IS the absence of one.
///
/// A ladder is sorted descending and de-duplicated here rather than trusted, and
/// an empty one falls back to the desktop ladder rather than making every model
/// unfittable.
/// The compute overhead, with the schema's `nonnegative` bound re-asserted:
/// typify drops every numeric bound, and a negative overhead would make the
/// biggest model on the shelf "fit" a phone. Zero is a valid author choice.
fn non_negative_overhead(value: i64) -> u64 {
    if value < 0 {
        DESKTOP_COMPUTE_OVERHEAD_BYTES
    } else {
        value as u64
    }
}

pub fn resolve_fit_profile(fit: &FitConfig, profile: FitProfile) -> ResolvedFit {
    let mut out = ResolvedFit {
        budget_half: fit.budget_fraction.half,
        budget_full: fit.budget_fraction.full,
        safety_margin_bytes: fit.safety_margin_bytes,
        smallest_quality_floor_ref: fit.smallest_quality_floor_ref.clone(),
        tie_breakers: fit.tie_breakers.clone().unwrap_or_default(),
        primary_score: fit.primary_score.clone(),
        compute_overhead_bytes: non_negative_overhead(fit.compute_overhead_bytes),
        ctx_ceiling: fit
            .ctx_ceiling
            .as_ref()
            .map_or(DESKTOP_CTX_CEILING, |c| CtxCeiling {
                smallest: c.smallest.get(),
                half: c.half.get(),
                full: c.full.get(),
            }),
        ctx_ladder: fit.ctx_ladder.iter().map(|n| n.get()).collect(),
        vision: fit.vision,
    };
    if profile == FitProfile::Mobile {
        if let Some(over) = fit.profiles.as_ref().map(|p| &p.mobile) {
            if let Some(b) = over.budget_fraction.as_ref() {
                out.budget_half = b.half;
                out.budget_full = b.full;
            }
            if let Some(v) = over.safety_margin_bytes {
                out.safety_margin_bytes = v;
            }
            if let Some(v) = over.smallest_quality_floor_ref.as_ref() {
                out.smallest_quality_floor_ref = v.clone();
            }
            if let Some(v) = over.compute_overhead_bytes {
                out.compute_overhead_bytes = non_negative_overhead(v);
            }
            if let Some(c) = over.ctx_ceiling.as_ref() {
                out.ctx_ceiling = CtxCeiling {
                    smallest: c.smallest.get(),
                    half: c.half.get(),
                    full: c.full.get(),
                };
            }
            if let Some(l) = over.ctx_ladder.as_ref() {
                out.ctx_ladder = l.iter().map(|n| n.get()).collect();
            }
            if let Some(v) = over.vision {
                out.vision = v;
            }
        }
    }
    out.ctx_ladder.sort_unstable_by(|a, b| b.cmp(a));
    out.ctx_ladder.dedup();
    if out.ctx_ladder.is_empty() {
        out.ctx_ladder = DESKTOP_CTX_LADDER.to_vec();
    }
    out
}

/// Token budget for thinking, scaled to the context window. The inference layer
/// injects the end-of-thinking tag once the budget is hit, which is what stops
/// the model spending the whole window reasoning.
fn reasoning_budget_for(ctx: u64) -> i64 {
    if ctx <= 8192 {
        512
    } else if ctx <= 16384 {
        1024
    } else {
        2048
    }
}

/// Relative quality of a quant by bits per weight. QAT builds punch above their
/// bits, so [`config_quant_rank`] adds a bonus on top of this.
fn quant_bpw(quant: &str) -> f64 {
    match quant {
        "Q8_0" => 8.0,
        "Q6_K" => 6.5,
        "Q5_K_M" => 5.5,
        "Q5_K_S" => 5.3,
        "Q4_K_M" | "UD-Q4_K_XL" | "UD-Q4_K_M" => 4.8,
        "Q4_0" => 4.5,
        "UD-Q3_K_XL" => 4.0,
        "Q3_K_M" | "UD-Q3_K_M" => 3.9,
        "UD-Q2_K_XL" => 2.7,
        "Q2_K" => 2.6,
        _ => 4.0,
    }
}

const QAT_QUALITY_BONUS: f64 = 1.5;

/// Gains above roughly Q6_K are negligible (+0.13% perplexity against fp16,
/// versus +0.03% for Q8_0) while Q8_0 costs about 30% more memory, so the
/// RECOMMENDED quant caps here. Above the ceiling is a fallback only, for when
/// nothing at or below it fits.
const SWEET_SPOT_MAX_BPW: f64 = 6.5;

/// The single memory pool the model has to fit into. Apple unified memory and
/// CPU-only inference both use system RAM; a discrete GPU targets VRAM, since
/// every layer is offloaded there.
fn memory_budget_bytes(hw: &HardwareInfo) -> i64 {
    if hw.unified_memory {
        return hw.total_ram_bytes;
    }
    if hw.gpu.backend == GpuBackend::Cpu || hw.gpu.vram_bytes == 0 {
        return hw.total_ram_bytes;
    }
    hw.gpu.vram_bytes
}

/// 0 offloads nothing; 999 offloads every layer.
fn gpu_layers_for(hw: &HardwareInfo) -> i64 {
    if hw.gpu.backend == GpuBackend::Cpu {
        0
    } else {
        999
    }
}

#[allow(clippy::manual_clamp)]
/// One thread per physical core, capped. Public because speech selection
/// applies the same policy.
pub fn threads_for(hw: &HardwareInfo) -> i64 {
    // A zero core count is the detector's failure mode, not a real machine.
    let cores = if hw.cpu_cores_physical == 0 {
        4
    } else {
        hw.cpu_cores_physical
    };
    // `clamp` would panic if the bounds ever inverted; this order cannot.
    cores.min(MAX_THREADS).max(1)
}

fn kv_cache_bytes(model: &CatalogModel, ctx: u64) -> u64 {
    2 * model.arch.block_count.get()
        * ctx
        * model.arch.head_count_kv.get()
        * model.arch.head_dim.get()
        * BYTES_PER_KV_ELEM
}

/// Zero when the profile does not preselect vision. Must move together with
/// `support_images` in [`applied_settings`]. The projector PATH is written
/// either way, so images-off is a default the user can overturn rather than a
/// capability the build took away.
fn vision_bytes(model: &CatalogModel, variant: &CatalogVariant, fit: &ResolvedFit) -> u64 {
    if has_vision(model, variant, fit) {
        variant.mmproj_size_bytes.map_or(0, |n| n.get())
    } else {
        0
    }
}

fn has_vision(model: &CatalogModel, variant: &CatalogVariant, fit: &ResolvedFit) -> bool {
    fit.vision && model.capabilities.vision && variant.mmproj_spec.is_some()
}

fn footprint_bytes(
    model: &CatalogModel,
    variant: &CatalogVariant,
    quant: &CatalogQuant,
    ctx: u64,
    fit: &ResolvedFit,
) -> u64 {
    quant.file_size_bytes.get()
        + kv_cache_bytes(model, ctx)
        + vision_bytes(model, variant, fit)
        + fit.compute_overhead_bytes
}

fn config_quant_rank(variant: &CatalogVariant, quant: &CatalogQuant) -> f64 {
    let base = quant_bpw(&quant.quant);
    if variant.tags.iter().flatten().any(|t| t == "qat") {
        base + QAT_QUALITY_BONUS
    } else {
        base
    }
}

/// One resolved model, variant, quant and context, with what it costs.
#[derive(Clone)]
struct Choice<'a> {
    model: &'a CatalogModel,
    variant: &'a CatalogVariant,
    quant: &'a CatalogQuant,
    ctx: u64,
    footprint_bytes: u64,
    quant_rank: f64,
}

/// Largest context at or below the ceiling and the model's maximum whose
/// footprint fits the limit, or `None` when even the smallest overflows.
fn best_ctx(
    model: &CatalogModel,
    variant: &CatalogVariant,
    quant: &CatalogQuant,
    limit: f64,
    ceiling: u64,
    fit: &ResolvedFit,
) -> Option<(u64, u64)> {
    let cap = ceiling.min(model.context_max.get());
    for ctx in fit.ctx_ladder.iter().copied() {
        if ctx > cap {
            continue;
        }
        let fp = footprint_bytes(model, variant, quant, ctx, fit);
        if (fp as f64) <= limit {
            return Some((ctx, fp));
        }
    }
    None
}

/// Within one model: prefer higher quant quality, then the smaller footprint.
fn better(a: &Choice<'_>, b: &Choice<'_>) -> bool {
    // Exact equality on purpose: both ranks come from the same table lookup
    // plus the same constant, so an epsilon would be wrong rather than careful.
    if a.quant_rank != b.quant_rank {
        return a.quant_rank > b.quant_rank;
    }
    a.footprint_bytes < b.footprint_bytes
}

/// The best-quality configuration of one model that fits the limit.
fn best_config_for_model<'a>(
    model: &'a CatalogModel,
    limit: f64,
    ceiling: u64,
    cfg: &ResolvedFit,
) -> Option<Choice<'a>> {
    let mut best: Option<Choice<'a>> = None;
    for variant in &model.variants {
        for quant in &variant.quants {
            let Some((ctx, footprint_bytes)) = best_ctx(model, variant, quant, limit, ceiling, cfg)
            else {
                continue;
            };
            let candidate = Choice {
                model,
                variant,
                quant,
                ctx,
                footprint_bytes,
                quant_rank: config_quant_rank(variant, quant),
            };
            if best.as_ref().is_none_or(|b| better(&candidate, b)) {
                best = Some(candidate);
            }
        }
    }
    best
}

/// The quant to recommend, and apply by default, for a model on this device:
/// the best-quality quant that fits AND sits at or below the sweet-spot ceiling,
/// so memory is not burned on Q8's diminishing returns. Falls back to a lower
/// quant when the sweet spot does not fit, and to the biggest that fits only
/// when nothing under the ceiling does. `None` when the model does not fit.
fn recommended_config_for_model<'a>(
    model: &'a CatalogModel,
    limit: f64,
    ceiling: u64,
    cfg: &ResolvedFit,
) -> Option<Choice<'a>> {
    let mut sweet: Option<Choice<'a>> = None;
    let mut any: Option<Choice<'a>> = None;
    for variant in &model.variants {
        for quant in &variant.quants {
            let Some((ctx, footprint_bytes)) = best_ctx(model, variant, quant, limit, ceiling, cfg)
            else {
                continue;
            };
            let candidate = Choice {
                model,
                variant,
                quant,
                ctx,
                footprint_bytes,
                quant_rank: config_quant_rank(variant, quant),
            };
            if any.as_ref().is_none_or(|a| better(&candidate, a)) {
                any = Some(candidate.clone());
            }
            // The QAT bonus deliberately does NOT lift a quant over the
            // ceiling: the cap is about memory spent per unit of quality, and
            // the bonus is about quality, so the base rate is what is capped.
            if quant_bpw(&quant.quant) <= SWEET_SPOT_MAX_BPW
                && sweet.as_ref().is_none_or(|s| better(&candidate, s))
            {
                sweet = Some(candidate);
            }
        }
    }
    sweet.or(any)
}

/// The configuration for one specific quant: the largest fitting context, or
/// the lowest context when even that overflows, in which case `fits` is false.
fn config_for_quant<'a>(
    model: &'a CatalogModel,
    variant: &'a CatalogVariant,
    quant: &'a CatalogQuant,
    limit: f64,
    cfg: &ResolvedFit,
) -> (Choice<'a>, bool) {
    let fit = best_ctx(model, variant, quant, limit, cfg.ctx_ceiling.full, cfg);
    let ctx = match fit {
        Some((ctx, _)) => ctx,
        None => lowest_ctx(model, cfg),
    };
    let footprint_bytes = match fit {
        Some((_, fp)) => fp,
        None => footprint_bytes(model, variant, quant, ctx, cfg),
    };
    (
        Choice {
            model,
            variant,
            quant,
            ctx,
            footprint_bytes,
            quant_rank: config_quant_rank(variant, quant),
        },
        fit.is_some(),
    )
}

/// The bottom rung of the ladder, capped to what the model was trained for.
fn lowest_ctx(model: &CatalogModel, fit: &ResolvedFit) -> u64 {
    // Non-empty by construction; `resolve_fit_profile` guarantees it.
    fit.ctx_ladder
        .last()
        .copied()
        .unwrap_or(DESKTOP_CTX_LADDER[DESKTOP_CTX_LADDER.len() - 1])
        .min(model.context_max.get())
}

/// Apply the catalog's ordered tie-breakers between two equally scored
/// configurations. True when `a` should win.
fn tie_break(a: &Choice<'_>, b: &Choice<'_>, fit: &ResolvedFit) -> bool {
    for rule in fit.tie_breakers.iter() {
        if rule == "lowerFootprint" {
            if a.footprint_bytes != b.footprint_bytes {
                return a.footprint_bytes < b.footprint_bytes;
            }
        } else if let Some(tag) = rule.strip_prefix("preferTag:") {
            let at = a.variant.tags.iter().flatten().any(|t| t == tag);
            let bt = b.variant.tags.iter().flatten().any(|t| t == tag);
            if at != bt {
                return at;
            }
        }
        // An unknown rule is ignored, so a catalog can ship a rule an older
        // core does not understand without the fit engine refusing to run.
    }
    false
}

fn applied_settings(
    c: &Choice<'_>,
    hw: &HardwareInfo,
    idle_unload_seconds: i64,
    fit: &ResolvedFit,
) -> AppliedModelSettings {
    let vision = has_vision(c.model, c.variant, fit);
    // Written to the `llm.*` settings on select so each model gets its own
    // tuning rather than inheriting the previous model's.
    let sampling = c.model.sampling.clone().unwrap_or_else(default_sampling);
    let defaults = default_sampling();
    AppliedModelSettings {
        model_path: c.quant.model_spec.clone(),
        // A fact about the MODEL, so it is written on every profile: erasing it
        // would leave `llm.supportImages` a switch wired to nothing.
        mmproj_path: if c.model.capabilities.vision {
            c.variant.mmproj_spec.clone()
        } else {
            None
        },
        context_size: c.ctx as i64,
        threads: threads_for(hw),
        gpu_layers: gpu_layers_for(hw),
        flash_attn: true,
        support_images: vision,
        idle_unload_seconds,
        reasoning_budget: reasoning_budget_for(c.ctx),
        temperature: sampling.temperature,
        top_p: sampling.top_p,
        top_k: sampling.top_k,
        min_p: sampling.min_p,
        repeat_penalty: sampling.repeat_penalty,
        dry_multiplier: sampling.dry_multiplier.or(defaults.dry_multiplier),
        presence_penalty: sampling.presence_penalty.or(defaults.presence_penalty),
        // Only catalog models carry a sampler order; anything else keeps the
        // server's default chain.
        samplers: sampling.samplers.or(defaults.samplers),
    }
}

fn to_recommendation(
    bucket: PresetBucket,
    c: &Choice<'_>,
    hw: &HardwareInfo,
    fit: &ResolvedFit,
) -> BucketRecommendation {
    let idle = if bucket == PresetBucket::Full {
        FULL_IDLE_UNLOAD_SECONDS
    } else {
        0
    };
    BucketRecommendation {
        bucket,
        model_id: c.model.id.clone(),
        name: c.model.name.clone(),
        quality: primary_score(c.model, &fit.primary_score).unwrap_or(0.0),
        quant: c.quant.quant.clone(),
        footprint_bytes: c.footprint_bytes as i64,
        vision: has_vision(c.model, c.variant, fit),
        variant_label: c.variant.label.clone(),
        apply: applied_settings(c, hw, idle, fit),
    }
}

/// The highest-scoring model whose configuration fits. `config_for` decides the
/// quant policy per bucket: sweet spot, or biggest that fits.
fn pick_best_model<'a>(
    models: &[&'a CatalogModel],
    fit: &ResolvedFit,
    config_for: impl Fn(&'a CatalogModel) -> Option<Choice<'a>>,
) -> Option<Choice<'a>> {
    let mut winner: Option<Choice<'a>> = None;
    let mut winner_score = f64::NEG_INFINITY;
    for model in models {
        let Some(score) = primary_score(model, &fit.primary_score) else {
            continue;
        };
        let Some(config) = config_for(model) else {
            continue;
        };
        // Exact, for the same reason as `better`.
        let tied = score == winner_score;
        let wins = score > winner_score
            || (tied && winner.as_ref().is_some_and(|w| tie_break(&config, w, fit)));
        if wins {
            winner = Some(config);
            winner_score = score;
        }
    }
    winner
}

/// The lowest-footprint model that clears the quality floor and fits, which is
/// the Smallest bucket. `config_for` supplies the recommended quant.
fn pick_smallest<'a>(
    models: &[&'a CatalogModel],
    fit: &ResolvedFit,
    floor: f64,
    config_for: impl Fn(&'a CatalogModel) -> Option<Choice<'a>>,
) -> Option<Choice<'a>> {
    let mut winner: Option<Choice<'a>> = None;
    for model in models {
        let Some(score) = primary_score(model, &fit.primary_score) else {
            continue;
        };
        if score < floor {
            continue;
        }
        let Some(config) = config_for(model) else {
            continue;
        };
        if winner
            .as_ref()
            .is_none_or(|w| config.footprint_bytes < w.footprint_bytes)
        {
            winner = Some(config);
        }
    }
    winner
}

/// The Smallest bucket's quality floor: the primary score of the model the
/// catalog names as the reference. Zero when that model is not in the catalog,
/// which admits everything rather than nothing.
fn floor_score(catalog: &CatalogPayload, fit: &ResolvedFit) -> f64 {
    flat_models(catalog)
        .iter()
        .find(|m| m.model.id == fit.smallest_quality_floor_ref)
        .and_then(|m| primary_score(m.model, &fit.primary_score))
        .unwrap_or(0.0)
}

/// Compute the three adaptive presets for this device.
pub fn compute_recommendations(
    catalog: &CatalogPayload,
    fit: &FitConfig,
    hw: &HardwareInfo,
    applied: RecommendationSetApplied,
    profile: FitProfile,
) -> RecommendationSet {
    let fit = &resolve_fit_profile(fit, profile);
    let flat = flat_models(catalog);
    let models: Vec<&CatalogModel> = flat.iter().map(|m| m.model).collect();
    let budget = memory_budget_bytes(hw) as f64;
    let margin = fit.safety_margin_bytes as f64;
    let half_limit = budget * fit.budget_half - margin;
    let full_limit = budget * fit.budget_full - margin;

    // Smallest and Half apply the recommended (sweet-spot) quant; Full maxes
    // out. Smallest is budgeted against the FULL limit on purpose: it is the
    // lowest-footprint model clearing the quality floor, not the model for a
    // smaller budget, so squeezing its limit would only cost it context.
    let smallest = pick_smallest(&models, fit, floor_score(catalog, fit), |m| {
        recommended_config_for_model(m, full_limit, fit.ctx_ceiling.smallest, fit)
    });
    let half = pick_best_model(&models, fit, |m| {
        recommended_config_for_model(m, half_limit, fit.ctx_ceiling.half, fit)
    });
    let full = pick_best_model(&models, fit, |m| {
        best_config_for_model(m, full_limit, fit.ctx_ceiling.full, fit)
    });

    RecommendationSet {
        hardware: hw.clone(),
        catalog_generated_at: catalog.generated_at.clone(),
        buckets: RecommendationSetBuckets {
            smallest: smallest.map(|c| to_recommendation(PresetBucket::Smallest, &c, hw, fit)),
            half: half.map(|c| to_recommendation(PresetBucket::Half, &c, hw, fit)),
            full: full.map(|c| to_recommendation(PresetBucket::Full, &c, hw, fit)),
        },
        applied,
    }
}

/// One browser row per catalog model: the quant recommended (and applied by
/// default) for this device, plus every quant the user can pick manually. When
/// nothing fits, falls back to the cheapest configuration and reports
/// `fits: false`.
pub fn build_catalog_views(
    catalog: &CatalogPayload,
    fit: &FitConfig,
    hw: &HardwareInfo,
    profile: FitProfile,
) -> Vec<CatalogModelView> {
    let fit = &resolve_fit_profile(fit, profile);
    let budget = memory_budget_bytes(hw) as f64;
    let full_limit = budget * fit.budget_full - fit.safety_margin_bytes as f64;

    let mut out = Vec::new();
    for flat in flat_models(catalog) {
        let model = flat.model;
        let fitting = recommended_config_for_model(model, full_limit, fit.ctx_ceiling.full, fit);
        let fits = fitting.is_some();
        let Some(config) = fitting.or_else(|| smallest_config(model, fit)) else {
            // No variants or no quants: unreachable from a validated catalog.
            continue;
        };

        let mut quants = Vec::new();
        for variant in &model.variants {
            for quant in &variant.quants {
                let (choice, fits) = config_for_quant(model, variant, quant, full_limit, fit);
                quants.push(QuantOption {
                    model_spec: quant.model_spec.clone(),
                    quant: quant.quant.clone(),
                    variant_label: variant.label.clone(),
                    footprint_bytes: choice.footprint_bytes as i64,
                    fits,
                    recommended: quant.model_spec == config.quant.model_spec,
                });
            }
        }

        // A user-visible badge row, so the dedupe must preserve insertion order.
        let mut tags: IndexSet<&str> = IndexSet::new();
        for variant in &model.variants {
            for tag in variant.tags.iter().flatten() {
                tags.insert(tag.as_str());
            }
        }

        out.push(CatalogModelView {
            id: model.id.clone(),
            family: flat.family.to_string(),
            name: model.name.clone(),
            quality: primary_score(model, &fit.primary_score),
            params_b: model.params_b,
            vision: fit.vision && model.capabilities.vision,
            tags: tags.into_iter().map(str::to_string).collect(),
            quant: config.quant.quant.clone(),
            variant_label: config.variant.label.clone(),
            footprint_bytes: config.footprint_bytes as i64,
            fits,
            apply: applied_settings(&config, hw, 0, fit),
            quants,
        });
    }
    out
}

/// Resolve apply-settings for one specific quant by its unique `modelSpec`,
/// which is what the manual quantization picker sends. `None` when the spec is
/// not in the catalog.
pub fn applied_for_model_spec(
    catalog: &CatalogPayload,
    fit: &FitConfig,
    hw: &HardwareInfo,
    model_spec: &str,
    profile: FitProfile,
) -> Option<(String, AppliedModelSettings)> {
    let fit = &resolve_fit_profile(fit, profile);
    let budget = memory_budget_bytes(hw) as f64;
    let full_limit = budget * fit.budget_full - fit.safety_margin_bytes as f64;
    for flat in flat_models(catalog) {
        let model = flat.model;
        for variant in &model.variants {
            for quant in &variant.quants {
                if quant.model_spec != model_spec {
                    continue;
                }
                let (choice, _) = config_for_quant(model, variant, quant, full_limit, fit);
                return Some((model.id.clone(), applied_settings(&choice, hw, 0, fit)));
            }
        }
    }
    None
}

/// The cheapest configuration of a model, used for the browser row when nothing
/// fits: the smallest-footprint quant at the lowest context.
fn smallest_config<'a>(model: &'a CatalogModel, fit: &ResolvedFit) -> Option<Choice<'a>> {
    let mut best: Option<Choice<'a>> = None;
    let ctx = lowest_ctx(model, fit);
    for variant in &model.variants {
        for quant in &variant.quants {
            let fp = footprint_bytes(model, variant, quant, ctx, fit);
            if best.as_ref().is_none_or(|b| fp < b.footprint_bytes) {
                best = Some(Choice {
                    model,
                    variant,
                    quant,
                    ctx,
                    footprint_bytes: fp,
                    quant_rank: config_quant_rank(variant, quant),
                });
            }
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::api::GpuInfo;
    use crate::models::catalog::validate;
    use serde_json::json;

    const GB: i64 = 1024 * 1024 * 1024;

    /// The real dev catalog. Parsing it is load-bearing beyond the fit
    /// assertions: the generated types carry `deny_unknown_fields`, so a field
    /// added to `@loca/model-catalog` fails here rather than being dropped.
    fn dev_catalog() -> CatalogPayload {
        let file: serde_json::Value =
            serde_json::from_str(include_str!("catalog.dev.json")).expect("the payload parses");
        let payload: CatalogPayload =
            serde_json::from_value(file["payload"].clone()).expect("the payload deserialises");
        validate(&payload).expect("the shipped catalog is valid");
        payload
    }

    fn unified(gb: i64) -> HardwareInfo {
        let total = gb * GB;
        HardwareInfo {
            total_ram_bytes: total,
            available_ram_bytes: total * 6 / 10,
            cpu_cores_physical: 10,
            gpu: GpuInfo {
                backend: GpuBackend::Metal,
                name: "Apple".into(),
                vram_bytes: total,
            },
            unified_memory: true,
        }
    }

    fn recommend(catalog: &CatalogPayload, hw: &HardwareInfo) -> RecommendationSet {
        compute_recommendations(
            catalog,
            &catalog.fit,
            hw,
            RecommendationSetApplied::default(),
            FitProfile::Desktop,
        )
    }

    fn views(catalog: &CatalogPayload, hw: &HardwareInfo) -> Vec<CatalogModelView> {
        build_catalog_views(catalog, &catalog.fit, hw, FitProfile::Desktop)
    }

    fn view<'a>(views: &'a [CatalogModelView], id: &str) -> &'a CatalogModelView {
        views
            .iter()
            .find(|v| v.id == id)
            .unwrap_or_else(|| panic!("{id} should be in the catalog"))
    }

    #[test]
    fn every_bucket_resolves_on_a_typical_16gb_device() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(16));
        assert!(rec.buckets.smallest.is_some(), "smallest should resolve");
        assert!(rec.buckets.half.is_some(), "half should resolve");
        assert!(rec.buckets.full.is_some(), "full should resolve");
    }

    #[test]
    fn smallest_clears_the_quality_floor() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(16));
        let smallest = rec.buckets.smallest.expect("smallest resolves");
        // The floor reference is Qwen/Qwen3.5-2B at 16.3; the 0.8B scores 10.5
        // and must never be Smallest despite being the cheapest thing here.
        assert!(smallest.quality >= 16.3);
        assert_ne!(smallest.model_id, "Qwen/Qwen3.5-0.8B");
    }

    #[test]
    fn more_memory_never_lowers_the_full_models_score() {
        let catalog = dev_catalog();
        let scores: Vec<f64> = [8i64, 16, 32, 64]
            .into_iter()
            .map(|gb| {
                recommend(&catalog, &unified(gb))
                    .buckets
                    .full
                    .map_or(0.0, |b| b.quality)
            })
            .collect();
        for i in 1..scores.len() {
            assert!(
                scores[i] >= scores[i - 1],
                "full score regressed at index {i}: {scores:?}"
            );
        }
    }

    #[test]
    fn full_stays_under_the_safety_ceiling() {
        let catalog = dev_catalog();
        let hw = unified(32);
        let rec = recommend(&catalog, &hw);
        let limit = hw.total_ram_bytes as f64 * catalog.fit.budget_fraction.full
            - catalog.fit.safety_margin_bytes as f64;
        let full = rec.buckets.full.expect("full resolves");
        assert!(full.footprint_bytes as f64 <= limit);
    }

    /// The QAT bonus, isolated. The real-catalog case below cannot see it: at
    /// 32GB Gemma's QAT build wins on footprint alone, since both quants are
    /// 4.8 bits and the QAT file is the smaller one, so it stays green with the
    /// bonus set to zero. Here the QAT variant has the LOWER base rate and the
    /// LARGER file, so only the bonus can lift it.
    #[test]
    fn the_qat_bonus_can_outrank_a_higher_base_quant() {
        const TOTAL: i64 = 100_000_000_000;
        let mut m = model("m", 1.0, json!([quant("Q6_K", 1_000_000_000u64)]));
        push_variant(
            &mut m,
            json!({
                "label": "QAT",
                "tags": ["qat"],
                "provider": "test",
                // 5.5 base + 1.5 bonus = 7.0, against Q6_K's 6.5, and deliberately
                // the bigger file so the footprint tie-break cannot decide it.
                "quants": [{
                    "quant": "Q5_K_M",
                    "modelSpec": "@t/r/main/qat.gguf",
                    "fileSizeBytes": 2_000_000_000u64,
                }],
            }),
        );
        let catalog = synthetic(json!([m]), json!([]));
        let all = views(&catalog, &budget(TOTAL));
        let picked = view(&all, "m");
        assert_eq!(picked.variant_label, "QAT");
        assert_eq!(picked.quant, "Q5_K_M");
        assert!(
            picked.footprint_bytes > 2_000_000_000,
            "and it won while costing more"
        );
    }

    #[test]
    fn the_qat_variant_wins_on_quality_per_byte() {
        let catalog = dev_catalog();
        // At 32GB the standard Q8 does not fit but the QAT UD-Q4 build does,
        // and its QAT bonus outranks the standard low-bpw quants that also fit.
        let all = views(&catalog, &unified(32));
        let gemma = view(&all, "google/gemma-4-31b-it");
        assert_eq!(gemma.variant_label, "QAT");
        assert_eq!(gemma.quant, "UD-Q4_K_XL");
        assert!(gemma.fits);
        assert_eq!(gemma.footprint_bytes, 27_139_688_768);
        assert_eq!(gemma.tags, vec!["qat".to_string()]);
    }

    #[test]
    fn the_recommended_quant_is_the_sweet_spot_and_is_applied_by_default() {
        let catalog = dev_catalog();
        let all = views(&catalog, &unified(16));
        let qwen = view(&all, "Qwen/Qwen3.5-4B");
        let q8 = qwen
            .quants
            .iter()
            .find(|q| q.quant == "Q8_0")
            .expect("Q8_0 is offered");
        let recommended = qwen
            .quants
            .iter()
            .find(|q| q.recommended)
            .expect("a quant is flagged recommended");
        // Q8_0 fits at 16GB, so the ceiling is what keeps it from being the
        // recommendation. Without this the next assertion proves nothing.
        assert!(q8.fits, "Q8_0 should fit on 16GB");
        assert_ne!(recommended.quant, "Q8_0");
        assert_eq!(recommended.quant, "Q4_K_M");
        // Picking the model with no explicit quant applies the recommended one.
        assert_eq!(qwen.apply.model_path, recommended.model_spec);
    }

    #[test]
    fn full_maxes_the_quant_while_smallest_and_half_cap_at_the_sweet_spot() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(16));
        // Full takes the biggest quant that fits.
        assert_eq!(rec.buckets.full.expect("full").quant, "Q8_0");
        // Smallest uses the recommended sweet spot, never Q8.
        assert_eq!(rec.buckets.smallest.expect("smallest").quant, "Q4_K_M");
        // Half caps at the sweet spot too, and steps DOWN to a lower quant when
        // the sweet-spot Q4 does not fit its half budget.
        let half = rec.buckets.half.expect("half");
        assert_ne!(half.quant, "Q8_0", "half never uses Q8");
        assert_eq!(half.quant, "Q3_K_M");
    }

    #[test]
    fn full_enables_idle_unload_and_the_others_do_not() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(16));
        // The literal, not the constant: `== FULL_IDLE_UNLOAD_SECONDS` would
        // pass with the constant set to anything.
        assert_eq!(
            rec.buckets.full.expect("full").apply.idle_unload_seconds,
            300
        );
        assert_eq!(rec.buckets.half.expect("half").apply.idle_unload_seconds, 0);
        assert_eq!(
            rec.buckets
                .smallest
                .expect("smallest")
                .apply
                .idle_unload_seconds,
            0
        );
    }

    // The desktop byte-identical guard. Every other test here asserts a
    // property (a bucket resolves, Full maxes the quant) that a retuned constant
    // satisfies unchanged, so a move in the ladder, the ceilings or the overhead
    // is invisible without these literals. Measured off the engine, deliberately
    // NOT derived from the constants.

    /// (model id, quant, ctx, footprint, gpu layers, threads, idle unload,
    /// vision, reasoning budget)
    type Pinned<'a> = (&'a str, &'a str, i64, i64, i64, i64, i64, bool, i64);

    fn discrete(total_gb: i64, backend: GpuBackend, vram_gb: i64) -> HardwareInfo {
        HardwareInfo {
            total_ram_bytes: total_gb * GB,
            available_ram_bytes: total_gb * GB * 6 / 10,
            cpu_cores_physical: 16,
            gpu: GpuInfo {
                backend,
                name: "GPU".into(),
                vram_bytes: vram_gb * GB,
            },
            unified_memory: false,
        }
    }

    fn cpu_only(total_gb: i64) -> HardwareInfo {
        HardwareInfo {
            total_ram_bytes: total_gb * GB,
            available_ram_bytes: total_gb * GB * 6 / 10,
            cpu_cores_physical: 8,
            gpu: GpuInfo {
                backend: GpuBackend::Cpu,
                name: "CPU".into(),
                vram_bytes: 0,
            },
            unified_memory: false,
        }
    }

    fn pin_of(r: &BucketRecommendation) -> Pinned<'_> {
        (
            r.model_id.as_str(),
            r.quant.as_str(),
            r.apply.context_size,
            r.footprint_bytes,
            r.apply.gpu_layers,
            r.apply.threads,
            r.apply.idle_unload_seconds,
            r.vision,
            r.apply.reasoning_budget,
        )
    }

    fn assert_buckets<'a, P: PartialEq + std::fmt::Debug>(
        label: &str,
        set: &'a RecommendationSet,
        want: [Option<P>; 3],
        pin: impl Fn(&'a BucketRecommendation) -> P,
    ) {
        let got = [
            set.buckets.smallest.as_ref(),
            set.buckets.half.as_ref(),
            set.buckets.full.as_ref(),
        ];
        for (name, (got, want)) in ["smallest", "half", "full"]
            .into_iter()
            .zip(got.into_iter().zip(want))
        {
            match (got, want) {
                (None, None) => {}
                (Some(_), None) => panic!("{label}/{name}: expected nothing to fit"),
                (None, Some(_)) => panic!("{label}/{name}: expected a recommendation, got none"),
                (Some(g), Some(w)) => assert_eq!(pin(g), w, "{label}/{name} moved"),
            }
        }
    }

    #[test]
    fn desktop_recommendations_are_byte_identical_to_the_pinned_baseline() {
        let catalog = dev_catalog();
        // The cuda row is the only one budgeted against VRAM rather than system
        // RAM, so the only one that catches a change to memory_budget_bytes.
        #[rustfmt::skip]
        let rows: [(&str, HardwareInfo, [Pinned<'_>; 3]); 5] = [
            ("unified 16GB", unified(16), [
                ("Qwen/Qwen3.5-2B", "Q4_K_M",  8192,  2951716288, 999, 8,   0, true,  512),
                ("Qwen/Qwen3.5-9B", "Q3_K_M",  4096,  6728680736, 999, 8,   0, true,  512),
                ("Qwen/Qwen3.5-9B", "Q8_0",   16384, 13193151776, 999, 8, 300, true, 1024),
            ]),
            ("unified 32GB", unified(32), [
                ("Qwen/Qwen3.5-2B", "Q4_K_M",     8192,  2951716288, 999, 8,   0, true,  512),
                ("Qwen/Qwen3.6-27B", "UD-Q2_K_XL", 8192, 15524870432, 999, 8,   0, true,  512),
                ("Qwen/Qwen3.6-27B", "Q4_K_M",    32768, 26934786336, 999, 8, 300, true, 2048),
            ]),
            ("unified 64GB", unified(64), [
                ("Qwen/Qwen3.5-2B", "Q4_K_M",   8192,  2951716288, 999, 8,   0, true,  512),
                ("Qwen/Qwen3.6-27B", "Q4_K_M", 16384, 22639819040, 999, 8,   0, true, 1024),
                ("Qwen/Qwen3.6-27B", "Q8_0",   32768, 38713305376, 999, 8, 300, true, 2048),
            ]),
            ("cpu-only 16GB", cpu_only(16), [
                ("Qwen/Qwen3.5-2B", "Q4_K_M",  8192,  2951716288, 0, 8,   0, true,  512),
                ("Qwen/Qwen3.5-9B", "Q3_K_M",  4096,  6728680736, 0, 8,   0, true,  512),
                ("Qwen/Qwen3.5-9B", "Q8_0",   16384, 13193151776, 0, 8, 300, true, 1024),
            ]),
            ("cuda 24GB VRAM in a 64GB box", discrete(64, GpuBackend::Cuda, 24), [
                ("Qwen/Qwen3.5-2B", "Q4_K_M",   8192,  2951716288, 999, 8,   0, true,  512),
                ("Qwen/Qwen3.5-9B", "Q4_K_M",  16384,  9346172192, 999, 8,   0, true, 1024),
                ("Qwen/Qwen3.6-27B", "Q4_K_M",  8192, 20492335392, 999, 8, 300, true,  512),
            ]),
        ];
        for (label, hw, want) in rows {
            assert_buckets(label, &recommend(&catalog, &hw), want.map(Some), pin_of);
        }
    }

    #[test]
    fn the_browser_view_is_byte_identical_to_the_pinned_baseline() {
        // build_catalog_views shares best_ctx / recommended_config_for_model /
        // footprint_bytes with compute_recommendations but has its own budget
        // line, so a refactor can move it while the three buckets stay put.
        let catalog = dev_catalog();
        let all = views(&catalog, &unified(16));
        let v = view(&all, "Qwen/Qwen3.5-4B");
        assert_eq!(
            (v.quant.as_str(), v.footprint_bytes, v.fits, v.quants.len()),
            ("Q4_K_M", 8308328800, true, 4),
        );
    }

    // The mobile profile, pinned the same way as the desktop guard above.

    fn phone(total_gb: i64) -> HardwareInfo {
        HardwareInfo {
            total_ram_bytes: total_gb * GB,
            available_ram_bytes: total_gb * GB / 2,
            cpu_cores_physical: 8,
            gpu: GpuInfo {
                backend: GpuBackend::Cpu,
                name: "CPU".into(),
                vram_bytes: 0,
            },
            unified_memory: true,
        }
    }

    fn recommend_mobile(catalog: &CatalogPayload, hw: &HardwareInfo) -> RecommendationSet {
        compute_recommendations(
            catalog,
            &catalog.fit,
            hw,
            RecommendationSetApplied::default(),
            FitProfile::Mobile,
        )
    }

    /// (model id, quant, ctx, footprint, support images)
    type MobilePin<'a> = (&'a str, &'a str, i64, i64, bool);

    fn mobile_pin(r: &BucketRecommendation) -> MobilePin<'_> {
        (
            r.model_id.as_str(),
            r.quant.as_str(),
            r.apply.context_size,
            r.footprint_bytes,
            r.apply.support_images,
        )
    }

    #[test]
    fn the_mobile_profile_at_2_4_6_8_and_12_gib_empty_buckets_included() {
        let catalog = dev_catalog();
        // 2 GiB fits nothing at all; 4 GiB leaves Balanced empty while the other
        // two resolve, which is the case a naive port reports as "no models".
        #[rustfmt::skip]
        let rows: [(&str, HardwareInfo, [Option<MobilePin<'_>>; 3]); 5] = [
            ("2 GiB phone", phone(2), [None, None, None]),
            ("4 GiB phone", phone(4), [
                Some(("Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false)),
                None,
                Some(("Qwen/Qwen3.5-2B", "UD-Q2_K_XL", 2048, 1467196672, false)),
            ]),
            ("6 GiB phone", phone(6), [
                Some(("Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false)),
                Some(("Qwen/Qwen3.5-0.8B", "Q4_K_M", 4096, 1133843712, false)),
                Some(("Qwen/Qwen3.5-2B", "Q4_K_M", 4096, 1882162432, false)),
            ]),
            ("8 GiB phone", phone(8), [
                Some(("Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false)),
                Some(("Qwen/Qwen3.5-2B", "Q3_K_M", 4096, 1708475648, false)),
                Some(("Qwen/Qwen3.5-4B", "Q3_K_M", 2048, 2961823904, false)),
            ]),
            ("12 GiB phone", phone(12), [
                Some(("Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false)),
                Some(("Qwen/Qwen3.5-2B", "Q4_K_M", 4096, 1882162432, false)),
                Some(("Qwen/Qwen3.5-4B", "Q4_K_M", 4096, 3677808800, false)),
            ]),
        ];
        for (label, hw, want) in rows {
            assert_buckets(label, &recommend_mobile(&catalog, &hw), want, mobile_pin);
        }
    }

    #[test]
    fn the_profile_argument_is_observed_not_decoration() {
        // Every other test here passes Desktop, so `resolve_fit_profile`
        // ignoring its argument has to go red somewhere.
        let catalog = dev_catalog();
        let hw = phone(8);
        let desktop = recommend(&catalog, &hw);
        let mobile = recommend_mobile(&catalog, &hw);
        let d = desktop.buckets.full.expect("desktop full");
        let m = mobile.buckets.full.expect("mobile full");
        assert!(
            d.model_id != m.model_id || d.apply.context_size != m.apply.context_size,
            "the same hardware produced the same answer under both profiles",
        );
    }

    #[test]
    fn a_context_below_the_desktop_ladder_floor_is_reachable_on_mobile() {
        // The desktop ladder bottoms out at 4096, so no phone-sized context is
        // reachable unless the ladder is read from the profile.
        let catalog = dev_catalog();
        let smallest = recommend_mobile(&catalog, &phone(6))
            .buckets
            .smallest
            .expect("the smallest bucket resolves on a 6 GiB phone");
        assert_eq!(smallest.apply.context_size, 2048);
    }

    #[test]
    fn mobile_defaults_vision_off_without_taking_it_away() {
        let catalog = dev_catalog();
        let hw = phone(6);
        let half = recommend_mobile(&catalog, &hw)
            .buckets
            .half
            .expect("the Balanced bucket resolves on a 6 GiB phone");

        assert!(
            !half.apply.support_images,
            "mobile must not preselect vision"
        );
        assert!(!half.vision, "the recommendation must not report vision");

        // An identity against catalog data, not a cross-profile subtraction:
        // the two profiles legitimately pick different contexts. The mmproj is
        // 204_987_232, so a footprint still carrying it cannot equal this sum.
        let kv_per_token: i64 = 2 * 24 * 2 * 256 * 2;
        assert_eq!(half.model_id, "Qwen/Qwen3.5-0.8B");
        assert_eq!(half.apply.context_size, 4096);
        assert_eq!(
            half.footprint_bytes,
            532_517_120 + 4096 * kv_per_token + 400_000_000,
            "the mobile footprint still carries the vision add-on",
        );

        // And the browser rows, which have their own budget line.
        let all = build_catalog_views(&catalog, &catalog.fit, &hw, FitProfile::Mobile);
        assert!(
            !view(&all, "Qwen/Qwen3.5-2B").vision,
            "the browser row must not advertise vision",
        );
    }

    #[test]
    fn mobile_still_writes_the_projector_path_so_the_setting_can_be_turned_on() {
        // `apply_to_patch` writes `llm.mmprojPath`, and that path is the only
        // thing between "images off by default" and "a switch wired to nothing".
        let catalog = dev_catalog();
        let all = build_catalog_views(&catalog, &catalog.fit, &phone(12), FitProfile::Mobile);
        let qwen = view(&all, "Qwen/Qwen3.5-4B");

        assert!(!qwen.vision, "the mobile row must not preselect vision");
        assert!(!qwen.apply.support_images, "nor turn the setting on");
        assert_eq!(
            qwen.apply.mmproj_path.as_deref(),
            Some("@unsloth/Qwen3.5-4B-GGUF/main/mmproj-F16.gguf"),
            "but it must still point at the projector it did not preselect",
        );
    }

    #[test]
    fn fit_profile_for_os_maps_the_two_phone_platforms_and_nothing_else() {
        // Its one production caller passes a desktop OS on a desktop machine, so
        // returning Desktop unconditionally leaves every other test green.
        assert_eq!(fit_profile_for_os("android"), FitProfile::Mobile);
        assert_eq!(fit_profile_for_os("ios"), FitProfile::Mobile);
        for os in ["macos", "linux", "windows", "freebsd", ""] {
            assert_eq!(
                fit_profile_for_os(os),
                FitProfile::Desktop,
                "{os} is not a phone"
            );
        }
    }

    #[test]
    fn a_negative_compute_overhead_falls_back_rather_than_shrinking_every_footprint() {
        // typify drops the schema's `nonnegative`, so this bound exists only
        // here. Zero stays legal; a negative would shrink every footprint.
        let catalog = dev_catalog();
        let mut fit = catalog.fit.clone();
        fit.compute_overhead_bytes = -1;
        assert_eq!(
            resolve_fit_profile(&fit, FitProfile::Desktop).compute_overhead_bytes,
            DESKTOP_COMPUTE_OVERHEAD_BYTES,
        );
        fit.compute_overhead_bytes = 0;
        assert_eq!(
            resolve_fit_profile(&fit, FitProfile::Desktop).compute_overhead_bytes,
            0,
        );
    }

    #[test]
    fn a_catalog_without_the_new_fields_resolves_to_the_desktop_constants() {
        // The additive-by-construction claim, checked rather than asserted in
        // prose. `ctxCeiling` in particular arrives as `Option` on the generated
        // type because typify does not materialise an object default, so the
        // fallback lives in `resolve_fit_profile` and a wrong one would move
        // every desktop recommendation.
        let catalog = dev_catalog();
        let mut bare = catalog.fit.clone();
        bare.ctx_ceiling = None;
        bare.compute_overhead_bytes = 600_000_000;
        bare.ctx_ladder = DESKTOP_CTX_LADDER
            .iter()
            .map(|n| std::num::NonZeroU64::new(*n).expect("nonzero"))
            .collect();
        bare.vision = true;
        bare.profiles = None;

        let resolved = resolve_fit_profile(&bare, FitProfile::Desktop);
        assert_eq!(resolved.ctx_ceiling, DESKTOP_CTX_CEILING);
        assert_eq!(
            resolved.compute_overhead_bytes,
            DESKTOP_COMPUTE_OVERHEAD_BYTES
        );
        assert_eq!(resolved.ctx_ladder, DESKTOP_CTX_LADDER.to_vec());
        assert!(resolved.vision);

        // And asking for Mobile on a catalog with no profiles is the base too,
        // rather than a panic or an empty ladder.
        let mobile = resolve_fit_profile(&bare, FitProfile::Mobile);
        assert_eq!(mobile.ctx_ladder, DESKTOP_CTX_LADDER.to_vec());
        assert_eq!(
            mobile.compute_overhead_bytes,
            DESKTOP_COMPUTE_OVERHEAD_BYTES
        );
    }

    #[test]
    fn a_cpu_only_backend_offloads_no_layers() {
        let catalog = dev_catalog();
        let hw = HardwareInfo {
            total_ram_bytes: 16 * GB,
            available_ram_bytes: 10 * GB,
            cpu_cores_physical: 8,
            gpu: GpuInfo {
                backend: GpuBackend::Cpu,
                name: "CPU".into(),
                vram_bytes: 0,
            },
            unified_memory: false,
        };
        let rec = recommend(&catalog, &hw);
        assert_eq!(rec.buckets.smallest.expect("smallest").apply.gpu_layers, 0);
    }

    #[test]
    fn each_models_catalog_sampling_is_applied() {
        let catalog = dev_catalog();
        let all = views(&catalog, &unified(32));
        let qwen = view(&all, "Qwen/Qwen3.5-4B");
        let gemma = view(&all, "google/gemma-4-31b-it");
        // Qwen ships temperature 0.6 and top_k 20; Gemma ships 1.0 and 64.
        assert_eq!(qwen.apply.temperature, 0.6);
        assert_eq!(qwen.apply.top_k, 20);
        assert_eq!(qwen.apply.repeat_penalty, 1.0);
        assert_eq!(gemma.apply.temperature, 1.0);
        assert_eq!(gemma.apply.top_k, 64);
    }

    /// Every term of the footprint at once, against a value derived by hand
    /// from the catalog's own numbers: weights 2_740_937_888, KV cache
    /// 2*32*32768*4*256*2 = 4_294_967_296, the vision module 672_423_616, and
    /// the fixed 600_000_000 overhead.
    ///
    /// The vision term is the one worth having a test for. It is only counted
    /// when the MODEL is vision-capable AND the VARIANT ships a projector, and
    /// nothing else here would notice it being dropped.
    #[test]
    fn a_footprint_is_weights_plus_kv_plus_vision_plus_overhead() {
        let catalog = dev_catalog();
        let all = views(&catalog, &unified(16));
        let qwen = view(&all, "Qwen/Qwen3.5-4B");
        assert_eq!(qwen.apply.context_size, 32768);
        assert_eq!(
            qwen.footprint_bytes,
            2_740_937_888 + 4_294_967_296 + 672_423_616 + 600_000_000
        );
        assert_eq!(qwen.footprint_bytes, 8_308_328_800);
    }

    /// The per-bucket context ceilings, which no TypeScript test covers. At
    /// 16GB the Smallest model could afford far more context than 8192; the
    /// ceiling is the only thing holding it there.
    #[test]
    fn each_bucket_caps_its_context_at_its_own_ceiling() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(16));
        assert_eq!(
            rec.buckets.smallest.expect("smallest").apply.context_size,
            8192
        );
        assert_eq!(rec.buckets.half.expect("half").apply.context_size, 4096);
        assert_eq!(rec.buckets.full.expect("full").apply.context_size, 16384);
    }

    /// Smallest is budgeted against the FULL limit, not the half limit: it is
    /// the lowest-footprint model that clears the quality floor, not the model
    /// for a smaller budget, so squeezing its limit would only cost it context.
    ///
    /// 8GB is where the two budgets give different answers. The half limit
    /// there is 2_794_967_296, just under the Smallest pick's 2_951_716_288, so
    /// budgeting it against the half limit would silently drop its context to
    /// 4096 while leaving every other assertion in this file green.
    #[test]
    fn smallest_is_budgeted_against_the_full_limit() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(8));
        let smallest = rec.buckets.smallest.expect("smallest");
        let half = rec.buckets.half.expect("half");
        assert_eq!(
            smallest.model_id, half.model_id,
            "the same model, so only the budget differs"
        );
        assert_eq!(
            smallest.apply.context_size, 8192,
            "the Smallest ceiling, under the full limit"
        );
        assert_eq!(
            half.apply.context_size, 4096,
            "the half limit cannot afford more"
        );
    }

    #[test]
    fn the_reasoning_budget_tracks_the_context_window() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(16));
        let smallest = rec.buckets.smallest.expect("smallest");
        let full = rec.buckets.full.expect("full");
        assert_eq!(smallest.apply.context_size, 8192);
        assert_eq!(smallest.apply.reasoning_budget, 512);
        assert_eq!(full.apply.context_size, 16384);
        assert_eq!(full.apply.reasoning_budget, 1024);
    }

    /// A device too small for the quality floor. Smallest and Half are `null`,
    /// which is a real answer the wire has to be able to carry, and Full still
    /// resolves because it has no floor.
    #[test]
    fn a_low_memory_device_reports_null_buckets_rather_than_failing() {
        let catalog = dev_catalog();
        let rec = recommend(&catalog, &unified(4));
        assert!(rec.buckets.smallest.is_none());
        assert!(rec.buckets.half.is_none());
        let full = rec.buckets.full.expect("full still resolves");
        assert_eq!(full.model_id, "Qwen/Qwen3.5-0.8B");
        assert_eq!(full.quant, "Q8_0");
    }

    /// When nothing fits, the browser row falls back to the cheapest quant at
    /// the lowest context and says so, rather than disappearing.
    #[test]
    fn a_model_that_does_not_fit_still_gets_a_row() {
        let catalog = dev_catalog();
        let all = views(&catalog, &unified(2));
        let big = view(&all, "Qwen/Qwen3.6-27B");
        assert!(!big.fits);
        assert_eq!(big.quant, "UD-Q2_K_XL", "the cheapest quant");
        assert_eq!(big.apply.context_size, 4096, "the lowest rung");
        assert_eq!(big.footprint_bytes, 14_451_128_608);
        assert!(
            big.quants.iter().all(|q| !q.fits),
            "no quant of a 27B fits in 2GB"
        );
    }

    #[test]
    fn the_memory_budget_follows_the_pool_the_model_runs_in() {
        let unified_hw = unified(16);
        assert_eq!(memory_budget_bytes(&unified_hw), 16 * GB);

        let discrete = HardwareInfo {
            unified_memory: false,
            gpu: GpuInfo {
                backend: GpuBackend::Cuda,
                name: "NVIDIA".into(),
                vram_bytes: 24 * GB,
            },
            ..unified(64)
        };
        assert_eq!(memory_budget_bytes(&discrete), 24 * GB, "VRAM, not RAM");

        // A non-CPU backend reporting no VRAM is a failed probe, not a GPU with
        // nothing in it, so the budget falls back to system RAM rather than to
        // zero. Without this branch every such machine gets no recommendation.
        let vramless = HardwareInfo {
            unified_memory: false,
            gpu: GpuInfo {
                backend: GpuBackend::Vulkan,
                name: "Intel".into(),
                vram_bytes: 0,
            },
            ..unified(64)
        };
        assert_eq!(memory_budget_bytes(&vramless), 64 * GB);
    }

    // --- the boundaries ------------------------------------------------------
    //
    // A threshold test whose fixture sits far from the threshold passes with the
    // comparison deleted, so each of these sits exactly ON its boundary.

    /// A synthetic catalog with the fit policy set to whole numbers, so a limit
    /// is an exact integer and a footprint can be placed exactly on it. The real
    /// catalog's 0.9 fraction cannot express an exact boundary in binary64.
    fn synthetic(models: serde_json::Value, tie_breakers: serde_json::Value) -> CatalogPayload {
        let payload = json!({
            "schemaVersion": 1,
            "generatedAt": "",
            "families": [{ "family": "Synthetic", "publisher": "Test", "models": models }],
            "fit": {
                "budgetFraction": { "half": 1.0, "full": 1.0 },
                "safetyMarginBytes": 0,
                "primaryScore": { "source": "test", "metric": "score" },
                "smallestQualityFloorRef": "floor-ref",
                "tieBreakers": tie_breakers,
            },
            "stt": { "models": [], "presets": [] },
            "tts": { "models": [], "presets": [] },
        });
        serde_json::from_value(payload).expect("the synthetic catalog deserialises")
    }

    /// One model. `arch` is all ones except the block count, so the KV cache is
    /// `4 * ctx` bytes and a footprint is easy to place exactly.
    fn model(id: &str, score: f64, quants: serde_json::Value) -> serde_json::Value {
        json!({
            "id": id,
            "name": id,
            "scores": [{ "source": "test", "metric": "score", "value": score }],
            "paramsB": 1,
            "contextMax": 262144,
            "arch": {
                "blockCount": 1, "embeddingLength": 1,
                "headCount": 1, "headCountKv": 1, "headDim": 1,
            },
            "capabilities": { "tools": true, "vision": false, "reasoning": false },
            "variants": [{
                "label": "standard", "tags": [], "provider": "test", "quants": quants,
            }],
        })
    }

    fn quant(name: &str, size: u64) -> serde_json::Value {
        json!({
            "quant": name,
            "modelSpec": format!("@t/r/main/{name}.gguf"),
            "fileSizeBytes": size,
        })
    }

    /// serde_json panics on an out-of-range array index assignment, so a
    /// second variant is pushed rather than written at `[1]`.
    fn push_variant(model: &mut serde_json::Value, variant: serde_json::Value) {
        model["variants"]
            .as_array_mut()
            .expect("variants is an array")
            .push(variant);
    }

    fn budget(total: i64) -> HardwareInfo {
        HardwareInfo {
            total_ram_bytes: total,
            available_ram_bytes: total,
            cpu_cores_physical: 8,
            gpu: GpuInfo {
                backend: GpuBackend::Metal,
                name: "test".into(),
                vram_bytes: total,
            },
            unified_memory: true,
        }
    }

    /// BOUNDARY: `footprint <= limit`. A configuration costing exactly the
    /// budget fits.
    #[test]
    fn a_footprint_exactly_equal_to_the_limit_fits() {
        // kv(32768) = 4 * 32768 = 131_072, overhead = 600_000_000.
        const TOTAL: i64 = 1_000_000_000;
        let weights = TOTAL as u64 - 131_072 - DESKTOP_COMPUTE_OVERHEAD_BYTES;
        let catalog = synthetic(
            json!([model("m", 1.0, json!([quant("Q4_K_M", weights)]))]),
            json!([]),
        );
        let rec = recommend(&catalog, &budget(TOTAL));
        let full = rec.buckets.full.expect("the model fits exactly");
        assert_eq!(full.footprint_bytes, TOTAL, "exactly on the limit");
        assert_eq!(full.apply.context_size, 32768, "the top rung, not the next");
    }

    /// BOUNDARY: `quant_bpw <= SWEET_SPOT_MAX_BPW`. Q6_K sits exactly ON the
    /// ceiling at 6.5 bits and must be recommendable.
    #[test]
    fn a_quant_exactly_at_the_sweet_spot_ceiling_is_recommendable() {
        const TOTAL: i64 = 100_000_000_000;
        let catalog = synthetic(
            json!([model(
                "m",
                1.0,
                json!([
                    quant("Q8_0", 8_000_000_000u64),
                    quant("Q6_K", 6_500_000_000u64)
                ])
            )]),
            json!([]),
        );
        let all = views(&catalog, &budget(TOTAL));
        let m = view(&all, "m");
        assert_eq!(m.quant, "Q6_K", "6.5 bpw is at the ceiling, not over it");
        assert!(
            m.quants
                .iter()
                .find(|q| q.quant == "Q8_0")
                .expect("Q8_0")
                .fits,
            "Q8_0 fits, so the ceiling is what excluded it"
        );
    }

    /// BOUNDARY: `score < floor`. The floor-reference model's own score is
    /// exactly the floor and must qualify.
    #[test]
    fn a_score_exactly_at_the_quality_floor_qualifies() {
        const TOTAL: i64 = 100_000_000_000;
        let catalog = synthetic(
            json!([
                // Cheaper, and below the floor: it must lose despite being the
                // smallest thing in the catalog.
                model("below", 9.0, json!([quant("Q4_K_M", 100_000_000u64)])),
                model("floor-ref", 10.0, json!([quant("Q4_K_M", 900_000_000u64)])),
            ]),
            json!([]),
        );
        let rec = recommend(&catalog, &budget(TOTAL));
        let smallest = rec.buckets.smallest.expect("the floor reference qualifies");
        assert_eq!(smallest.model_id, "floor-ref");
    }

    /// BOUNDARY: `ctx > cap`. A model trained to exactly 8192 must be offered
    /// 8192.
    #[test]
    fn a_context_exactly_at_the_models_maximum_is_offered() {
        const TOTAL: i64 = 100_000_000_000;
        let mut m = model("m", 1.0, json!([quant("Q4_K_M", 1_000_000u64)]));
        m["contextMax"] = json!(8192);
        let catalog = synthetic(json!([m]), json!([]));
        let rec = recommend(&catalog, &budget(TOTAL));
        assert_eq!(rec.buckets.full.expect("full").apply.context_size, 8192);
    }

    /// BOUNDARY: the two `ctx <=` steps in the reasoning budget. 8192 is the
    /// last context that gets 512 and 16384 the last that gets 1024.
    #[test]
    fn the_reasoning_budget_steps_exactly_at_its_thresholds() {
        assert_eq!(reasoning_budget_for(8191), 512);
        assert_eq!(reasoning_budget_for(8192), 512);
        assert_eq!(reasoning_budget_for(8193), 1024);
        assert_eq!(reasoning_budget_for(16384), 1024);
        assert_eq!(reasoning_budget_for(16385), 2048);
    }

    /// BOUNDARY: `better()` falls through to the footprint only when the ranks
    /// are EQUAL. Q4_K_M and UD-Q4_K_M are both 4.8 bits, so the tie is real.
    #[test]
    fn equal_quant_ranks_break_towards_the_smaller_footprint() {
        const TOTAL: i64 = 100_000_000_000;
        let catalog = synthetic(
            json!([model(
                "m",
                1.0,
                json!([
                    quant("Q4_K_M", 5_000_000_000u64),
                    quant("UD-Q4_K_M", 4_000_000_000u64),
                ])
            )]),
            json!([]),
        );
        let all = views(&catalog, &budget(TOTAL));
        assert_eq!(view(&all, "m").quant, "UD-Q4_K_M");
    }

    /// Two models with IDENTICAL primary scores, the later one tagged `qat`:
    /// without the `preferTag:qat` rule the first one wins by arriving first.
    #[test]
    fn a_tie_on_primary_score_is_broken_by_the_catalogs_rules() {
        const TOTAL: i64 = 100_000_000_000;
        let mut tagged = model("tagged", 10.0, json!([quant("Q4_K_M", 1_000_000_000u64)]));
        tagged["variants"][0]["tags"] = json!(["qat"]);
        tagged["variants"][0]["label"] = json!("QAT");
        let plain = model("plain", 10.0, json!([quant("Q4_K_M", 1_000_000_000u64)]));

        // Same footprint, so `lowerFootprint` cannot decide and `preferTag`
        // must. Declaration order puts the untagged model first.
        let with_rules = synthetic(
            json!([plain.clone(), tagged.clone()]),
            json!(["lowerFootprint", "preferTag:qat"]),
        );
        assert_eq!(
            recommend(&with_rules, &budget(TOTAL))
                .buckets
                .full
                .expect("full")
                .model_id,
            "tagged"
        );

        // With no rules at all the first-declared model keeps the tie, which is
        // what makes the assertion above about the rule rather than the order.
        let without = synthetic(json!([plain, tagged]), json!([]));
        assert_eq!(
            recommend(&without, &budget(TOTAL))
                .buckets
                .full
                .expect("full")
                .model_id,
            "plain"
        );
    }

    /// An unknown tie-breaker is ignored rather than fatal, so a catalog can
    /// ship a rule an older core does not understand.
    #[test]
    fn an_unknown_tie_breaker_is_ignored() {
        const TOTAL: i64 = 100_000_000_000;
        let catalog = synthetic(
            json!([model("m", 1.0, json!([quant("Q4_K_M", 1_000_000u64)]))]),
            json!(["preferMagic", "lowerFootprint"]),
        );
        assert!(recommend(&catalog, &budget(TOTAL)).buckets.full.is_some());
    }

    /// The badge row is user-visible, so the dedupe must preserve insertion
    /// order. Needs a fixture: every shipped model carries at most one tag, and
    /// with one tag a `BTreeSet` looks identical.
    #[test]
    fn tags_keep_their_catalog_order_through_the_dedupe() {
        const TOTAL: i64 = 100_000_000_000;
        let mut m = model("m", 1.0, json!([quant("Q4_K_M", 1_000_000u64)]));
        m["variants"][0]["tags"] = json!(["moe", "qat"]);
        push_variant(
            &mut m,
            json!({
                "label": "second",
                // `qat` repeats, and `abridged` sorts before both: an ordered
                // dedupe gives moe, qat, abridged, and a sorted one does not.
                "tags": ["qat", "abridged"],
                "provider": "test",
                "quants": [quant("Q3_K_M", 900_000u64)],
            }),
        );
        let catalog = synthetic(json!([m]), json!([]));
        let all = views(&catalog, &budget(TOTAL));
        assert_eq!(view(&all, "m").tags, vec!["moe", "qat", "abridged"]);
    }

    #[test]
    fn a_manual_quant_pick_resolves_by_its_unique_spec() {
        let catalog = dev_catalog();
        let hw = unified(32);
        let (model_id, apply) = applied_for_model_spec(
            &catalog,
            &catalog.fit,
            &hw,
            "@unsloth/Qwen3.5-4B-GGUF/main/Qwen3.5-4B-Q8_0.gguf",
            FitProfile::Desktop,
        )
        .expect("the spec is in the catalog");
        assert_eq!(model_id, "Qwen/Qwen3.5-4B");
        assert_eq!(
            apply.model_path,
            "@unsloth/Qwen3.5-4B-GGUF/main/Qwen3.5-4B-Q8_0.gguf"
        );
        // A manual pick never enables idle-unload; only the Full bucket does.
        assert_eq!(apply.idle_unload_seconds, 0);
        assert!(applied_for_model_spec(
            &catalog,
            &catalog.fit,
            &hw,
            "@nobody/nothing/main/x.gguf",
            FitProfile::Desktop,
        )
        .is_none());
    }

    /// `config_for_quant` reads two constants nothing else in this file pins:
    /// the FULL budget fraction and the FULL context ceiling. 24GB is the size
    /// where the full limit, the half limit and the Smallest ceiling each give a
    /// different answer, so the fixture discriminates all three.
    #[test]
    fn a_manual_quant_pick_is_budgeted_at_the_full_fraction_and_the_full_ceiling() {
        const SPEC: &str = "@unsloth/Qwen3.5-9B-GGUF/main/Qwen3.5-9B-Q8_0.gguf";
        let catalog = dev_catalog();
        let hw = unified(24);

        let (model_id, apply) =
            applied_for_model_spec(&catalog, &catalog.fit, &hw, SPEC, FitProfile::Desktop)
                .expect("the spec is in the catalog");
        assert_eq!(model_id, "Qwen/Qwen3.5-9B");
        assert_eq!(
            apply.context_size, 32768,
            "the Full ceiling, at the full limit"
        );
        assert_eq!(apply.reasoning_budget, 2048);

        // The same two constants decide the browser's per-quant footprint
        // column, through the second call site.
        let all = views(&catalog, &hw);
        let q8 = view(&all, "Qwen/Qwen3.5-9B")
            .quants
            .iter()
            .find(|q| q.model_spec == SPEC)
            .expect("Q8_0 is offered");
        assert_eq!(q8.footprint_bytes, 15_340_635_424);
        assert!(q8.fits);
    }

    /// The per-bucket ceilings with memory taken out of the picture: the real
    /// catalog never makes `ctx_ceiling.half` the binding constraint, so a wrong
    /// value ships green. Here the fractions are 1.0 and the model costs
    /// kilobytes, so the ceiling is the only thing deciding.
    #[test]
    fn each_bucket_stops_at_its_own_ceiling_when_memory_is_not_the_constraint() {
        const TOTAL: i64 = 100_000_000_000;
        let catalog = synthetic(
            json!([model("m", 1.0, json!([quant("Q4_K_M", 1_000_000u64)]))]),
            json!([]),
        );
        let rec = recommend(&catalog, &budget(TOTAL));
        assert_eq!(
            rec.buckets.smallest.expect("smallest").apply.context_size,
            8192
        );
        assert_eq!(rec.buckets.half.expect("half").apply.context_size, 16384);
        assert_eq!(rec.buckets.full.expect("full").apply.context_size, 32768);
    }

    /// The `lowerFootprint` arm no other test reads. The LARGER model is
    /// declared first and tagged `qat`, so it wins by arrival order, by an
    /// inverted comparison, and by the `preferTag:qat` rule that follows: only a
    /// correct `lowerFootprint`, applied first, picks the smaller one.
    #[test]
    fn a_tie_breaks_towards_the_lower_footprint_before_any_tag_is_consulted() {
        const TOTAL: i64 = 100_000_000_000;
        let mut bigger = model("bigger", 10.0, json!([quant("Q4_K_M", 4_000_000_000u64)]));
        bigger["variants"][0]["tags"] = json!(["qat"]);
        bigger["variants"][0]["label"] = json!("QAT");
        let smaller = model("smaller", 10.0, json!([quant("Q4_K_M", 1_000_000_000u64)]));

        let catalog = synthetic(
            json!([bigger, smaller]),
            json!(["lowerFootprint", "preferTag:qat"]),
        );
        assert_eq!(
            recommend(&catalog, &budget(TOTAL))
                .buckets
                .full
                .expect("full")
                .model_id,
            "smaller"
        );
    }

    #[test]
    fn threads_are_capped_and_a_failed_probe_falls_back() {
        let mut hw = unified(16);
        hw.cpu_cores_physical = 32;
        assert_eq!(threads_for(&hw), MAX_THREADS);
        hw.cpu_cores_physical = 2;
        assert_eq!(threads_for(&hw), 2);
        // The helper's failure value, which must not become zero threads.
        hw.cpu_cores_physical = 0;
        assert_eq!(threads_for(&hw), 4);
    }

    /// Vision is only applied when the model is capable AND the chosen variant
    /// ships a projector, and `mmprojPath` is set even though `supportImages`
    /// is a separate user choice the select route deliberately never writes.
    #[test]
    fn a_vision_model_carries_its_projector_path() {
        let catalog = dev_catalog();
        let all = views(&catalog, &unified(32));
        let qwen = view(&all, "Qwen/Qwen3.5-4B");
        assert!(qwen.vision);
        assert!(qwen.apply.support_images);
        assert_eq!(
            qwen.apply.mmproj_path.as_deref(),
            Some("@unsloth/Qwen3.5-4B-GGUF/main/mmproj-F16.gguf")
        );
    }
}

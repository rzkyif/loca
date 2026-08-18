//! Settings to load parameters, in this module's own types.
//!
//! These mirror the shim's `ModelOptions` and `ContextOptions` without naming
//! them, so the mapping is testable in the DEFAULT gate and so the shim's
//! surface (being redesigned around `common_sampler`, D-136) can move without
//! touching the decisions here. The gated half converts, field by field.
//!
//! # Read the RESOLVED record
//!
//! Every read here uses `Settings::load_effective_resolved`, per D-072: anything
//! that ACTS on a setting reads the resolved form, so the schema's default
//! arrives rather than whatever fallback a call site happens to hardcode. Only
//! `llm/resolver.rs` takes the sparse record, and it documents at its head why.
//!
//! The fallbacks passed below are therefore belt-and-braces for a record that
//! failed to resolve, not the operative defaults. Where one differs from the
//! schema it is called out at the site.

use crate::settings::access::{bool_setting, num_setting, str_setting};
use crate::settings::SettingsRecord;

/// Layers to offload to the GPU when the user has expressed no preference.
///
/// **`-1`, not `0`, and this is the whole reason the constant exists.**
///
/// `llm.gpuLayers` defaults to `""` and its description says blank "lets Loca
/// decide". Behind `llama-server` blank meant the `--n-gpu-layers` flag was
/// simply omitted (`sidecars/llama.ts:122-124`), so llama.cpp's own default
/// applied, and that default is `-1` (`src/llama-model.cpp:2473`), which
/// `llama_model::n_gpu_layers()` resolves to every layer (`:1735-1737`).
///
/// In process there is no omitting anything: a number is always passed. The
/// shim's `ModelOptions::default()` uses `0`, documented there as "CPU only",
/// which is the correct default for a STRUCT and the wrong answer for a blank
/// SETTING. Spreading that default over this field would move every desktop user
/// from full GPU offload to CPU with no type, no test and no log line noticing.
/// That is the D-071 family exactly: a defaulted value has as many defaults as
/// it has hops.
const GPU_LAYERS_AUTO: i32 = -1;

/// Context window when the record carries none.
///
/// Desktop matches the schema's 4096. Mobile halves it: the KV cache alone is
/// hundreds of MB for a 2B model at 4096, and an out-of-memory kill is not a
/// degraded answer, it is no answer. This is a DEFAULT and not a cap, so a user
/// who sets the value still gets it.
///
/// Two constants and an injected platform answer rather than one `#[cfg]`
/// constant, for the reason [`context_spec_where`] gives: a `cfg`-selected value can
/// only be asserted against on the target that selects it, so on a desktop
/// runner the mobile half is not merely untested, it is uncompilable-into-the-
/// test. Reverting this default is an out-of-memory kill on a phone, which is
/// too expensive to leave to a build that no CI job runs.
const DESKTOP_CONTEXT_SIZE: f64 = 4096.0;
const MOBILE_CONTEXT_SIZE: f64 = 2048.0;

const DEFAULT_THREADS: f64 = 4.0;

/// Prefill chunk size. Not a setting, and it is load-bearing beyond throughput:
/// it is the granularity at which a cancelled turn can stop during prefill, so a
/// single-shot prefill would hold the context for seconds after the user pressed
/// stop. Matches llama.cpp's own default.
const DEFAULT_BATCH_SIZE: u32 = 512;

/// Everything that decides WHICH model is loaded and how.
///
/// Compared as a unit to answer "does the loaded model still match the
/// settings", which is what replaces restarting the sidecar process on a
/// settings change. It is also what the KV cache is keyed on: reusing a cache
/// across a spec change would condition a turn on another configuration's state
/// and produce fluent, confident, wrong output with nothing failing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadSpec {
    /// The `llm.modelPath` value as configured, before HuggingFace resolution.
    /// Kept unresolved so the comparison does not depend on the models
    /// directory, which cannot change without a restart anyway.
    pub model_spec: String,
    /// `llm.mmprojPath`, the multimodal projector that lets this model see an
    /// image. Empty when the model is text-only or the user has not selected a
    /// vision model.
    ///
    /// Part of the SPEC, so changing it reloads. It has to be: the projector is
    /// loaded against a specific model, and a pair that has drifted apart
    /// produces embeddings the language model attends to as if they meant
    /// something, which is fluent nonsense rather than an error.
    pub mmproj_spec: String,
    pub n_gpu_layers: i32,
    pub use_mmap: bool,
    pub context: ContextSpec,
}

/// Everything that decides how the context is sized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextSpec {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_threads: i32,
    pub flash_attn: bool,
}

/// What this record asks to be loaded, or `None` when it asks for nothing.
///
/// Two records ask for nothing, and neither is a failure:
///
/// - **`llm.provider` is not `local`.** The turn is going to an external
///   endpoint, so loading is not merely unnecessary, it is a multi-GB read and
///   the memory that goes with it on behalf of a user who is not using the
///   local model at all. `llamaStartArgsFromSettings` refuses on the same key
///   first (`sidecars/llama.ts:33-35`), before it looks at anything else.
/// - **No model is configured.** Blank here is blank after RESOLUTION, which is
///   what makes the check meaningful: the schema default has already been
///   applied, so this is a user with no model rather than a record that merely
///   never mentioned one. The TypeScript has to reach for the schema default by
///   hand at this point (`llama.ts:36-40`) precisely because its record is
///   sparse.
///
/// `gpu_layers_override` is [`super::backends::gpu_layers_override`]'s answer,
/// passed in rather than read so this stays one pure function and so the mobile
/// build, which has no backend policy at all, has nothing to pass. It is part of
/// the returned spec rather than applied later because the spec is what decides
/// whether a reload is needed: a user switching `llm.binaryBackend` to `cpu`
/// must get a model that is actually back on the CPU, and a comparison that did
/// not include the override would report the loaded model as still matching.
pub fn requested_load(
    settings: &SettingsRecord,
    gpu_layers_override: Option<i32>,
) -> Option<LoadSpec> {
    if str_setting(settings, "llm.provider", "local") != "local" {
        return None;
    }
    let spec = load_spec_for(settings, gpu_layers_override);
    if spec.model_spec.trim().is_empty() {
        return None;
    }
    Some(spec)
}

/// Read a load specification from a RESOLVED settings record.
pub fn load_spec(settings: &SettingsRecord) -> LoadSpec {
    load_spec_for(settings, None)
}

/// [`load_spec`], with the acceleration preference's layer count applied.
pub fn load_spec_for(settings: &SettingsRecord, gpu_layers_override: Option<i32>) -> LoadSpec {
    LoadSpec {
        model_spec: str_setting(settings, "llm.modelPath", ""),
        // Only when images are actually enabled. `llm.supportImages` is what the
        // user toggles, and a projector configured but not enabled must not be
        // loaded, because it is often as large as the model itself. The mobile
        // fit profile does not preselect vision, but it still writes the path,
        // so this key is the whole gate (D-178).
        mmproj_spec: if bool_setting(settings, "llm.supportImages", false) {
            str_setting(settings, "llm.mmprojPath", "")
        } else {
            String::new()
        },
        n_gpu_layers: gpu_layers_override.unwrap_or_else(|| gpu_layers(settings)),
        use_mmap: use_mmap(settings),
        context: context_spec(settings),
    }
}

/// Whether to mmap the weights.
///
/// On by default everywhere, and that default is what carries the phone: mmap'd
/// weight pages are file-backed and evictable, which is the condition the iOS
/// increased-memory-limit entitlement and the mobile fit budget both assume.
/// Turning it off on a phone makes them resident anonymous memory and invites a
/// jetsam kill.
///
/// **A default, not a cap.** The platform is not consulted here, on the same
/// ruling as the projector path: the risk argues for the default being on, not
/// for the setting being unreachable. A user who turns it off has said something
/// about their own device, and a build that silently ignores that is a switch
/// wired to nothing. The description carries the warning; the code carries the
/// default. See `context_spec_where`, which has made the same trade since it was
/// written.
fn use_mmap(settings: &SettingsRecord) -> bool {
    bool_setting(settings, "llm.mmap", true)
}

pub fn context_spec(settings: &SettingsRecord) -> ContextSpec {
    context_spec_where(
        settings,
        cfg!(any(target_os = "android", target_os = "ios")),
    )
}

/// [`context_spec`] with the platform answer injected, so both defaults are
/// reachable from any host's test run.
fn context_spec_where(settings: &SettingsRecord, mobile: bool) -> ContextSpec {
    let default = if mobile {
        MOBILE_CONTEXT_SIZE
    } else {
        DESKTOP_CONTEXT_SIZE
    };
    ContextSpec {
        n_ctx: whole(num_setting(settings, "llm.contextSize", default)),
        n_batch: DEFAULT_BATCH_SIZE,
        n_threads: whole_i32(num_setting(settings, "llm.threads", DEFAULT_THREADS)),
        flash_attn: bool_setting(settings, "llm.flashAttn", false),
    }
}

/// How many layers to offload.
///
/// Blank, absent or unreadable means [`GPU_LAYERS_AUTO`]; an explicit `0` is a
/// real choice (stay off the GPU) and is honoured. `num_setting` already keeps
/// those apart, and the mechanism is worth naming because it looks like it
/// should not: blank arrives as the STRING `""`, which fails its
/// `!s.is_empty()` guard and takes the fallback, while a numeric `0` parses to
/// zero. So "blank" and "zero" are distinguished by their JSON TYPE, not by
/// their value, and no second reader is needed to tell them apart.
fn gpu_layers(settings: &SettingsRecord) -> i32 {
    whole_i32(num_setting(
        settings,
        "llm.gpuLayers",
        f64::from(GPU_LAYERS_AUTO),
    ))
}

fn whole(value: f64) -> u32 {
    if value <= 0.0 {
        return 0;
    }
    // `as` saturates at the bounds and the negative case is handled above.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = value.floor() as u32;
    out
}

fn whole_i32(value: f64) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    let out = value.trunc() as i32;
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn record(pairs: Value) -> SettingsRecord {
        match pairs {
            Value::Object(map) => map,
            _ => panic!("test records are objects"),
        }
    }

    #[test]
    fn the_projector_is_requested_only_when_image_support_is_on() {
        // Both halves matter and they fail in opposite directions.
        //
        // Loading a projector nobody asked for costs the memory of a second
        // model: mmproj files routinely rival the language model in size, which
        // is exactly why M4 forces `llm.supportImages` off on mobile. Not
        // loading one when images ARE on means an image turn is refused (or,
        // before libmtmd was linked, silently answered as if the image were not
        // there) on a correctly configured install.
        //
        // Read through `load_spec` rather than the setting, because the spec is
        // what decides a reload: a projector that changes without the spec
        // changing would be a stale pair, and a pair that has drifted apart
        // produces embeddings the language model attends to as if they meant
        // something.
        let on = load_spec(&record(json!({
            "llm.modelPath": "@u/r/main/m.gguf",
            "llm.mmprojPath": "@u/r/main/mmproj.gguf",
            "llm.supportImages": true,
        })));
        assert_eq!(on.mmproj_spec, "@u/r/main/mmproj.gguf");

        let off = load_spec(&record(json!({
            "llm.modelPath": "@u/r/main/m.gguf",
            "llm.mmprojPath": "@u/r/main/mmproj.gguf",
            "llm.supportImages": false,
        })));
        assert_eq!(
            off.mmproj_spec, "",
            "a projector was requested with image support off, which loads a second model's              worth of weights for a capability the user turned off",
        );

        // The default, which is what a fresh install and every text model has.
        let unset = load_spec(&record(json!({ "llm.modelPath": "@u/r/main/m.gguf" })));
        assert_eq!(unset.mmproj_spec, "");
    }

    #[test]
    fn a_blank_gpu_layers_setting_means_every_layer_not_cpu_only() {
        // The ruling's test, and it pins the MAPPING rather than the default so
        // a later change to the shim's `ModelOptions::default()` cannot
        // reintroduce it.
        //
        // Blank is what ships: `llm.gpuLayers` defaults to "" and its help text
        // says blank lets Loca decide. Behind llama-server that omitted the flag
        // and llama.cpp offloaded everything. Answering 0 here would move every
        // desktop user to CPU-only, and nothing else in this crate would notice.
        for blank in [json!(""), json!("   "), json!(null)] {
            let spec = load_spec(&record(json!({ "llm.gpuLayers": blank })));
            assert_eq!(
                spec.n_gpu_layers, -1,
                "blank gpuLayers ({blank}) must mean auto, which llama.cpp \
                 resolves to every layer",
            );
        }
        // And absent entirely, which is the actual on-disk state for a user who
        // never touched the field.
        assert_eq!(load_spec(&record(json!({}))).n_gpu_layers, -1);
    }

    #[test]
    fn an_explicit_zero_is_honoured_and_is_not_confused_with_blank() {
        // The other direction, and the reason blank cannot simply be defaulted
        // through `num_setting`: 0 is a real choice (stay off the GPU) and must
        // survive, while blank must become -1. A single fallback cannot express
        // both, and collapsing them silently disables the CPU escape hatch for a
        // user with a broken GPU driver.
        assert_eq!(
            load_spec(&record(json!({ "llm.gpuLayers": 0 }))).n_gpu_layers,
            0,
        );
        assert_eq!(
            load_spec(&record(json!({ "llm.gpuLayers": "0" }))).n_gpu_layers,
            0,
        );
    }

    #[test]
    fn a_configured_layer_count_is_read_from_a_number_or_a_numeric_string() {
        // The settings file carries whichever the client last wrote, and a
        // number-typed field arriving as a string is ordinary here.
        assert_eq!(
            load_spec(&record(json!({ "llm.gpuLayers": 24 }))).n_gpu_layers,
            24,
        );
        assert_eq!(
            load_spec(&record(json!({ "llm.gpuLayers": "24" }))).n_gpu_layers,
            24,
        );
    }

    #[test]
    fn junk_in_gpu_layers_falls_back_to_auto_rather_than_to_cpu() {
        // A hand-edited file. Auto is the safe landing: it is what the user had
        // before they broke the field.
        assert_eq!(
            load_spec(&record(json!({ "llm.gpuLayers": "lots" }))).n_gpu_layers,
            -1,
        );
    }

    #[test]
    fn the_context_default_is_halved_on_mobile() {
        // BOTH branches, on whatever host runs this. The earlier version of this
        // test asserted under `#[cfg]`, so on a desktop runner the mobile
        // assertion compiled to nothing and reverting the mobile default
        // survived it; the mutation table below recorded that as "host cannot
        // fail" rather than as a caught mutation. M4 hit the same trap in this
        // file from the other side (D-163's note), and the fix is the same one.
        assert_eq!(
            context_spec_where(&record(json!({})), true).n_ctx,
            2048,
            "the KV cache alone is hundreds of MB for a 2B model at 4096, and \
             an out-of-memory kill is not a degraded answer",
        );
        assert_eq!(
            context_spec_where(&record(json!({})), false).n_ctx,
            4096,
            "desktop matches the settings schema",
        );
        // And the one this host actually compiles, so the wiring from the `cfg`
        // to the decision is covered too.
        assert_eq!(
            context_spec(&record(json!({}))).n_ctx,
            if cfg!(any(target_os = "android", target_os = "ios")) {
                2048
            } else {
                4096
            },
        );
    }

    #[test]
    fn a_configured_context_size_wins_on_every_target() {
        // The mobile number is a DEFAULT, not a cap. A user who asks for 8192
        // gets it, on the phone too.
        assert_eq!(
            context_spec(&record(json!({ "llm.contextSize": 8192 }))).n_ctx,
            8192,
        );
        assert_eq!(
            context_spec(&record(json!({ "llm.contextSize": "8192" }))).n_ctx,
            8192,
        );
    }

    #[test]
    fn threads_and_flash_attention_follow_the_schema_defaults() {
        let spec = context_spec(&record(json!({})));
        assert_eq!(spec.n_threads, 4);
        assert!(!spec.flash_attn);
        assert_eq!(spec.n_batch, 512);
    }

    #[test]
    fn every_field_makes_the_spec_compare_unequal() {
        // The spec is compared as a unit to decide "reload the model" and "reuse
        // the KV cache". A field missing from the comparison means a settings
        // change that silently keeps the old model, or a cache reused across a
        // configuration it does not belong to. Asserting one field would leave
        // the rest free.
        let base = load_spec(&record(json!({})));
        let cases = [
            json!({ "llm.modelPath": "@u/r/main/other.gguf" }),
            // Both keys together, because the projector only reaches the spec
            // when image support is on. Asserting the path alone would compare
            // two empty strings and pass while proving nothing; asserting the
            // toggle alone would (correctly) NOT force a reload, since turning
            // image support on with no projector configured changes nothing
            // there is to load.
            json!({ "llm.mmprojPath": "@u/r/main/mmproj.gguf", "llm.supportImages": true }),
            json!({ "llm.gpuLayers": 8 }),
            json!({ "llm.mmap": false }),
            json!({ "llm.contextSize": 1024 }),
            json!({ "llm.threads": 8 }),
            json!({ "llm.flashAttn": true }),
        ];
        for changed in cases {
            assert_ne!(
                base,
                load_spec(&record(changed.clone())),
                "changing {changed} must force a reload",
            );
        }
    }

    #[test]
    fn an_identical_record_compares_equal_so_a_turn_does_not_reload() {
        // The other half: if the spec compared unequal to itself, every turn
        // would reload a multi-GB model and the reuse machinery would never
        // engage. A test asserting only inequality would pass with that bug.
        let settings = record(json!({ "llm.modelPath": "@u/r/main/m.gguf" }));
        assert_eq!(load_spec(&settings), load_spec(&settings));
    }

    #[test]
    fn an_external_provider_asks_for_nothing_to_be_loaded() {
        // The expensive half of this file. A user on OpenAI who happens to have
        // a model path in their settings must not have that model read off disk
        // and held in memory for a turn that never touches it, and the check has
        // to come FIRST because such a user usually does have a path configured:
        // the schema ships one, and switching provider does not clear it.
        assert_eq!(
            requested_load(
                &record(json!({
                    "llm.provider": "external",
                    "llm.modelPath": "@u/r/main/m.gguf",
                })),
                None,
            ),
            None,
        );
        // And the same record on `local` does ask, or the assertion above would
        // pass for a function that never loads anything at all.
        assert!(requested_load(
            &record(json!({
                "llm.provider": "local",
                "llm.modelPath": "@u/r/main/m.gguf",
            })),
            None,
        )
        .is_some());
    }

    #[test]
    fn no_configured_model_asks_for_nothing_and_is_not_an_error() {
        // A fresh core before the user has chosen a model. Blank here is blank
        // after resolution, so it is a user with no model rather than a record
        // that never mentioned one, and it must be a quiet no-op: a turn then
        // fails on the endpoint with its own message instead of the load failing
        // first with a worse one.
        for blank in [json!(""), json!("   ")] {
            assert_eq!(
                requested_load(&record(json!({ "llm.modelPath": blank })), None),
                None,
                "a blank model path ({blank}) is nothing to load",
            );
        }
        assert_eq!(requested_load(&record(json!({})), None), None);
    }

    #[test]
    fn the_default_provider_is_local_so_a_record_that_never_said_still_loads() {
        // `llm.provider` defaults to local, and a resolved record carries it.
        // Defaulting the other way here would leave every local user's model
        // unloaded whenever the key was missing, with the turn failing against a
        // port nothing listens on.
        assert!(
            requested_load(&record(json!({ "llm.modelPath": "/models/m.gguf" })), None).is_some(),
        );
    }

    #[test]
    fn the_acceleration_override_lands_in_the_spec_and_forces_a_reload() {
        // `llm.binaryBackend: cpu` pins zero layers over whatever
        // `llm.gpuLayers` says. Applying that AFTER the comparison would leave a
        // user who just switched to CPU still running on the GPU they are
        // escaping, because the spec would compare equal and no reload would
        // happen. The override is therefore part of the spec, not a step after
        // it.
        let settings = record(json!({
            "llm.modelPath": "@u/r/main/m.gguf",
            "llm.gpuLayers": 24,
        }));
        let auto = requested_load(&settings, None).expect("a configured model loads");
        let cpu = requested_load(&settings, Some(0)).expect("a configured model loads");
        assert_eq!(auto.n_gpu_layers, 24);
        assert_eq!(cpu.n_gpu_layers, 0);
        assert_ne!(auto, cpu, "switching to CPU must force a reload");
    }

    #[test]
    fn no_override_leaves_the_configured_layer_count_alone() {
        // The other direction: every preference but `cpu` answers `None`, and
        // `None` must mean "the setting decides", including the blank that means
        // every layer. An override applied unconditionally would be the
        // CPU-only regression this file's first test exists to prevent.
        assert_eq!(load_spec_for(&record(json!({})), None).n_gpu_layers, -1);
        assert_eq!(
            load_spec_for(&record(json!({ "llm.gpuLayers": 24 })), None).n_gpu_layers,
            24,
        );
    }

    /// On by default, and off when the user says off, on every target.
    ///
    /// The second assertion is the one with history. mmap was briefly forced on
    /// for the mobile targets, on the argument that a value whose wrong setting
    /// gets the app OS-killed is a footgun rather than a choice. That is the
    /// same argument that took the projector away from a phone, and it was
    /// overruled the same way: the risk sets the DEFAULT, and the platform is
    /// not consulted at the point that decides.
    #[test]
    fn mmap_defaults_on_and_an_explicit_off_is_honoured_everywhere() {
        assert!(load_spec(&record(json!({}))).use_mmap);
        assert!(!load_spec(&record(json!({ "llm.mmap": false }))).use_mmap);
        // Named separately from the `load_spec` pair so a future platform
        // branch inside `use_mmap` has something to fail: the two above would
        // both still pass on a desktop test host if one were reintroduced.
        assert!(!use_mmap(&record(json!({ "llm.mmap": false }))));
        assert!(use_mmap(&record(json!({}))));
    }
}

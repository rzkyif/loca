//! Settings to load parameters, in this module's own types. These mirror the
//! shim's `ModelOptions` and `ContextOptions` without naming them, so the
//! mapping is testable in the default gate; the gated half converts field by
//! field.
//!
//! Every read here expects a RESOLVED record, so the fallbacks below are
//! belt-and-braces rather than the operative defaults. Where one differs from
//! the schema it is called out at the site.

use crate::settings::access::{bool_setting, num_setting, str_setting};
use crate::settings::SettingsRecord;

/// Layers to offload when `llm.gpuLayers` is blank, which its description says
/// "lets Loca decide". `-1`, not `0`: llama.cpp resolves -1 to every layer,
/// while the shim's `ModelOptions::default()` of `0` means CPU only, which is
/// right for a struct and wrong for a blank setting.
const GPU_LAYERS_AUTO: i32 = -1;

/// Context window when the record carries none. Mobile halves the schema's 4096
/// because the KV cache alone is hundreds of MB for a 2B model at 4096. A
/// default and not a cap. Two constants plus an injected platform answer rather
/// than one `#[cfg]`, so both halves are assertable from any host.
const DESKTOP_CONTEXT_SIZE: f64 = 4096.0;
const MOBILE_CONTEXT_SIZE: f64 = 2048.0;

const DEFAULT_THREADS: f64 = 4.0;

/// Prefill chunk size. Not a setting, and it is load-bearing beyond throughput:
/// it is the granularity at which a cancelled turn can stop during prefill, so a
/// single-shot prefill would hold the context for seconds after the user pressed
/// stop. Matches llama.cpp's own default.
const DEFAULT_BATCH_SIZE: u32 = 512;

/// Everything that decides WHICH model is loaded and how. Compared as a unit to
/// answer "does the loaded model still match the settings", and it is what the
/// KV cache is keyed on: reusing a cache across a spec change would condition a
/// turn on another configuration's state with nothing failing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadSpec {
    /// As configured, before HuggingFace resolution, so the comparison does not
    /// depend on the models directory.
    pub model_spec: String,
    /// The multimodal projector, empty for a text-only model. Part of the spec
    /// so changing it reloads: a projector and model that have drifted apart
    /// produce embeddings the model attends to as if they meant something.
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

/// What this record asks to be loaded, or `None` when it asks for nothing:
/// either `llm.provider` is not `local` (a multi-GB read for a user not using
/// the local model) or no model is configured. Neither is a failure.
///
/// `gpu_layers_override` is [`super::backends::gpu_layers_override`]'s answer,
/// passed in so this stays pure and mobile has nothing to pass. It belongs in
/// the spec rather than being applied after: the spec decides whether a reload
/// is needed, so switching `llm.binaryBackend` to `cpu` must change it.
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
        // Only when images are enabled: a projector is often as large as the
        // model itself, and the mobile fit profile writes the path without
        // preselecting vision, so this key is the whole gate.
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

/// Whether to mmap the weights. On by default everywhere: mmap'd pages are
/// file-backed and evictable, which the iOS memory entitlement and the mobile
/// fit budget both assume. A default and not a cap, so the platform is not
/// consulted: a user who turns it off has said something about their device.
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

/// How many layers to offload. Blank, absent or unreadable means
/// [`GPU_LAYERS_AUTO`]; an explicit `0` is a real choice and is honoured.
/// `num_setting` keeps them apart by JSON type: blank arrives as `""` and fails
/// its `!s.is_empty()` guard, while a numeric `0` parses to zero.
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

        let unset = load_spec(&record(json!({ "llm.modelPath": "@u/r/main/m.gguf" })));
        assert_eq!(unset.mmproj_spec, "");
    }

    #[test]
    fn a_blank_gpu_layers_setting_means_every_layer_not_cpu_only() {
        for blank in [json!(""), json!("   "), json!(null)] {
            let spec = load_spec(&record(json!({ "llm.gpuLayers": blank })));
            assert_eq!(
                spec.n_gpu_layers, -1,
                "blank gpuLayers ({blank}) must mean auto, which llama.cpp \
                 resolves to every layer",
            );
        }
        // Absent entirely: the on-disk state for a user who never touched it.
        assert_eq!(load_spec(&record(json!({}))).n_gpu_layers, -1);
    }

    #[test]
    fn an_explicit_zero_is_honoured_and_is_not_confused_with_blank() {
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
        assert_eq!(
            load_spec(&record(json!({ "llm.gpuLayers": "lots" }))).n_gpu_layers,
            -1,
        );
    }

    #[test]
    fn the_context_default_is_halved_on_mobile() {
        // Both branches, on whatever host runs this: asserting under `#[cfg]`
        // would compile the mobile half to nothing on a desktop runner.
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
        // And the one this host compiles, so the `cfg` wiring is covered.
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
        // The mobile number is a default, not a cap.
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
        let base = load_spec(&record(json!({})));
        let cases = [
            json!({ "llm.modelPath": "@u/r/main/other.gguf" }),
            // Both keys together: the projector only reaches the spec when
            // image support is on, so either alone proves nothing.
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
        let settings = record(json!({ "llm.modelPath": "@u/r/main/m.gguf" }));
        assert_eq!(load_spec(&settings), load_spec(&settings));
    }

    #[test]
    fn an_external_provider_asks_for_nothing_to_be_loaded() {
        // The provider check must come first: switching provider does not clear
        // the configured model path.
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
        // The same record on `local` does ask, or the assertion above would pass
        // for a function that never loads anything.
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
        // Blank after resolution is a user with no model, and must be a quiet
        // no-op so the turn fails on the endpoint with its own message.
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
        assert!(
            requested_load(&record(json!({ "llm.modelPath": "/models/m.gguf" })), None).is_some(),
        );
    }

    #[test]
    fn the_acceleration_override_lands_in_the_spec_and_forces_a_reload() {
        // Applying the override after the comparison would leave a user who
        // just switched to CPU on the GPU, since the spec would compare equal.
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
        // `None` means "the setting decides", including the blank that means
        // every layer.
        assert_eq!(load_spec_for(&record(json!({})), None).n_gpu_layers, -1);
        assert_eq!(
            load_spec_for(&record(json!({ "llm.gpuLayers": 24 })), None).n_gpu_layers,
            24,
        );
    }

    /// On by default, and off when the user says off, on every target.
    #[test]
    fn mmap_defaults_on_and_an_explicit_off_is_honoured_everywhere() {
        assert!(load_spec(&record(json!({}))).use_mmap);
        assert!(!load_spec(&record(json!({ "llm.mmap": false }))).use_mmap);
        // Named separately so a platform branch inside `use_mmap` has something
        // to fail: the two above would still pass on a desktop test host.
        assert!(!use_mmap(&record(json!({ "llm.mmap": false }))));
        assert!(use_mmap(&record(json!({}))));
    }
}

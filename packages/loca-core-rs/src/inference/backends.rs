//! Which backend modules are loaded, and what to do about what registers.
//!
//! Desktop only. Mobile links llama.cpp statically, loads no module and
//! downloads no executable code, which is what W^X and App Store 2.5.2 require;
//! the shim makes the shared mode a compile error there.
//!
//! Pure: the directories are computed and the registration outcome is judged
//! here, with no llama.cpp and no filesystem access, so both run in the DEFAULT
//! gate. The gated half does the loading and counts what came back.
//!
//! # The three rules this cannot break
//!
//! 1. **Nothing searches anywhere we did not name** (D-107). The shim enumerates
//!    an explicit directory and poisons `ggml_backend_load_all` and
//!    `ggml_backend_load_all_from_path`, because one scans the process working
//!    directory and the other honours `GGML_BACKEND_PATH`. Both are arbitrary
//!    code execution for a service whose working directory and environment are
//!    not the operator's. This module only ever produces OUR directories.
//! 2. **The CPU backend is itself a loadable module** (D-106), so an empty
//!    directory means zero registered devices and a core that cannot infer at
//!    all. That is a hard error here, never a quiet degradation.
//! 3. **A missing GPU module is not an error.** The consent-download directory
//!    is absent on every machine that never accepted a GPU download, which is
//!    the normal case, and refusing to answer because an OPTIONAL download is
//!    missing would turn a preference into an outage.

use std::path::{Path, PathBuf};

use crate::error::{internal, Result};
use crate::settings::access::str_setting;
use crate::settings::SettingsRecord;

/// What the user asked for in `llm.binaryBackend`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acceleration {
    /// Load everything present and use whatever registers.
    Auto,
    /// Stay off the GPU. The escape hatch for a broken driver, and it must stay
    /// reachable without deleting a file.
    Cpu,
    /// A specific backend (`vulkan`, `cuda`, `rocm`, `hip`, `metal`). A
    /// preference, not a requirement: see [`review`].
    Named(String),
}

/// Read the acceleration preference from a RESOLVED settings record (D-072).
///
/// The setting is a select over seven values, so only a hand-edited file reaches
/// an unrecognised string, and an unrecognised string is treated as a named
/// backend rather than as an error: it then behaves as the preference it looks
/// like, which is a warning and CPU, rather than failing a turn over a typo.
pub fn acceleration(settings: &SettingsRecord) -> Acceleration {
    match str_setting(settings, "llm.binaryBackend", "auto").as_str() {
        "cpu" => Acceleration::Cpu,
        // Blank is not one of the select's values, but a blank string setting is
        // how "never touched" reads on disk for other fields in this group, and
        // auto is the schema default.
        "auto" | "" => Acceleration::Auto,
        named => Acceleration::Named(named.to_string()),
    }
}

/// The directories to load backend modules from, in load order.
///
/// - `install` sits beside the core binary and the llama.cpp libraries that
///   D-116's relative rpath resolves through. **The CPU baseline ships here**,
///   so it is loaded for every preference including `cpu`.
/// - `root` is the channel's state directory; `<root>/backends/` is where a GPU
///   module fetched through the consent-gated download flow lands. It is absent
///   on a machine that never accepted one, and the loader treats a missing
///   directory as zero modules rather than as a failure.
///
/// `cpu` omits the second directory entirely. Loading a GPU module and then
/// declining to offload would still register the device, still allocate its
/// context, and still be reported to the user as present, which is not what
/// "stay off the GPU" means to someone whose driver crashes the process.
pub fn directories(accel: &Acceleration, install: &Path, root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![install.to_path_buf()];
    if *accel != Acceleration::Cpu {
        dirs.push(root.join("backends"));
    }
    dirs
}

/// The layer count this preference forces, if it forces one.
///
/// `cpu` pins zero regardless of `llm.gpuLayers`, because the two settings can
/// disagree and this one is the deliberate escape hatch. Every other preference
/// leaves [`super::options::load_spec`]'s answer alone, including the blank
/// `llm.gpuLayers` that means "every layer".
pub fn gpu_layers_override(accel: &Acceleration) -> Option<i32> {
    match accel {
        Acceleration::Cpu => Some(0),
        Acceleration::Auto | Acceleration::Named(_) => None,
    }
}

/// What registered, once the directories have been loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Registered {
    /// Every device llama.cpp reports, CPU included.
    pub total: usize,
    /// Those that are not the CPU device. llama.cpp's device list carries a
    /// coarse kind (cpu / gpu / accel) and NOT the backend's name, so "did the
    /// backend the user named turn up" is only answerable as "did anything but
    /// the CPU turn up". Naming the specific backend would need the device
    /// description parsed, which is a string upstream is free to change.
    pub accelerated: usize,
}

/// Judge the registration outcome.
///
/// Returns the warning to log, if any. Errors only in the one case that cannot
/// be worked around.
///
/// **Zero devices is fatal and is the worst failure shape available.** It means
/// the CPU module is missing from the install, so nothing can run: not a slower
/// turn, no turn. The message names the directories that were searched, because
/// the alternative diagnosis path is reading llama.cpp's DEBUG log for an absent
/// line.
pub fn review(
    accel: &Acceleration,
    found: Registered,
    searched: &[PathBuf],
) -> Result<Option<String>> {
    if found.total == 0 {
        let dirs = searched
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(internal(format!(
            "no compute device registered, so no model can run. The CPU backend \
             module is part of the install and appears to be missing. Searched: {dirs}"
        )));
    }
    match accel {
        // A named backend that did not turn up is a preference that could not be
        // honoured. Continue on what did register rather than refusing: the
        // module is an optional consent-gated download, and its absence is the
        // state of every machine that has not accepted one.
        Acceleration::Named(name) if found.accelerated == 0 => Ok(Some(format!(
            "no {name} device registered, continuing on CPU. Install the {name} \
             backend from the model settings to use it."
        ))),
        _ => Ok(None),
    }
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

    fn install() -> PathBuf {
        PathBuf::from("/opt/loca")
    }

    fn root() -> PathBuf {
        PathBuf::from("/home/u/.loca/stable")
    }

    #[test]
    fn the_preference_is_read_from_the_select_and_anything_else_is_a_name() {
        assert_eq!(acceleration(&record(json!({}))), Acceleration::Auto);
        assert_eq!(
            acceleration(&record(json!({ "llm.binaryBackend": "auto" }))),
            Acceleration::Auto,
        );
        assert_eq!(
            acceleration(&record(json!({ "llm.binaryBackend": "" }))),
            Acceleration::Auto,
        );
        assert_eq!(
            acceleration(&record(json!({ "llm.binaryBackend": "cpu" }))),
            Acceleration::Cpu,
        );
        for named in ["vulkan", "cuda", "rocm", "hip", "metal"] {
            assert_eq!(
                acceleration(&record(json!({ "llm.binaryBackend": named }))),
                Acceleration::Named(named.to_string()),
                "every value of the select must survive the read",
            );
        }
    }

    #[test]
    fn every_preference_loads_the_install_directory_because_the_cpu_module_is_there() {
        // The CPU backend is a dlopen'd module like any other (D-106). A
        // preference that skipped the install directory would register zero
        // devices and could not run at all, which is the failure `review` calls
        // the worst shape available.
        for accel in [
            Acceleration::Auto,
            Acceleration::Cpu,
            Acceleration::Named("cuda".to_string()),
        ] {
            let dirs = directories(&accel, &install(), &root());
            assert_eq!(
                dirs.first(),
                Some(&install()),
                "{accel:?} must load the install directory first",
            );
        }
    }

    #[test]
    fn cpu_does_not_even_look_at_the_downloaded_backends() {
        // "Stay off the GPU" has to mean the module is not loaded, not merely
        // that no layers are offloaded to it. A loaded module still registers
        // its device and still initialises the driver, which is exactly what a
        // user with a crashing driver is escaping.
        assert_eq!(
            directories(&Acceleration::Cpu, &install(), &root()),
            vec![install()],
        );
        assert_eq!(
            directories(&Acceleration::Auto, &install(), &root()),
            vec![install(), root().join("backends")],
        );
        assert_eq!(
            directories(
                &Acceleration::Named("vulkan".to_string()),
                &install(),
                &root()
            ),
            vec![install(), root().join("backends")],
        );
    }

    #[test]
    fn cpu_pins_zero_layers_and_nothing_else_overrides_the_setting() {
        // The two settings can disagree: `llm.gpuLayers` blank means every
        // layer, and this preference has to win. Letting the layer count through
        // would offload to a device the user asked not to use, on a machine
        // where that is a crash rather than a preference.
        assert_eq!(gpu_layers_override(&Acceleration::Cpu), Some(0));
        assert_eq!(gpu_layers_override(&Acceleration::Auto), None);
        assert_eq!(
            gpu_layers_override(&Acceleration::Named("metal".to_string())),
            None,
            "a named backend must not disturb the configured layer count",
        );
    }

    #[test]
    fn zero_devices_fails_loudly_and_names_where_it_looked() {
        // The one fatal case. It cannot be degraded around: with no device
        // registered llama.cpp cannot load a model at all, so a warning here
        // would produce a core that accepts turns and answers none of them.
        let dirs = directories(&Acceleration::Auto, &install(), &root());
        let err = review(&Acceleration::Auto, Registered::default(), &dirs).unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("/opt/loca") && message.contains("backends"),
            "the message must name both searched directories, or the only \
             diagnosis left is reading llama.cpp's debug log: {message}",
        );
        assert!(message.contains("CPU backend"), "{message}");
    }

    #[test]
    fn a_cpu_only_registration_is_fine_for_auto_and_for_cpu() {
        // The ordinary machine: no GPU download accepted, one CPU device. This
        // must be silent, not warned about, or every default install logs a
        // warning on every model load.
        let found = Registered {
            total: 1,
            accelerated: 0,
        };
        let dirs = directories(&Acceleration::Auto, &install(), &root());
        assert_eq!(review(&Acceleration::Auto, found, &dirs).unwrap(), None);
        assert_eq!(review(&Acceleration::Cpu, found, &dirs).unwrap(), None);
    }

    #[test]
    fn a_named_backend_that_did_not_register_warns_and_continues() {
        // A preference that could not be honoured. Refusing would turn a missing
        // OPTIONAL download into a core that cannot answer, which is a far worse
        // outcome than a slower one, and the module is consent-gated so its
        // absence is the normal state.
        let found = Registered {
            total: 1,
            accelerated: 0,
        };
        let dirs = directories(&Acceleration::Auto, &install(), &root());
        let warning = review(&Acceleration::Named("vulkan".to_string()), found, &dirs)
            .unwrap()
            .expect("a missing named backend must say so");
        assert!(warning.contains("vulkan"), "{warning}");

        // And it goes quiet once the device is there, or the warning becomes
        // noise that is logged on every load and stops being read.
        let found = Registered {
            total: 2,
            accelerated: 1,
        };
        assert_eq!(
            review(&Acceleration::Named("vulkan".to_string()), found, &dirs).unwrap(),
            None,
        );
    }
}

//! Which backend modules are loaded and how to judge what registers. Desktop
//! only; mobile links statically. Only our own directories are ever searched
//! (`ggml_backend_load_all*` is poisoned in the shim). The CPU backend is
//! itself a loadable module, so zero devices is a hard error; a missing GPU
//! module is the normal state of a machine that never accepted the download.

use std::path::{Path, PathBuf};

use crate::error::{internal, Result};
use crate::settings::access::str_setting;
use crate::settings::SettingsRecord;

/// What the user asked for in `llm.binaryBackend`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acceleration {
    /// Load everything present and use whatever registers.
    Auto,
    /// Stay off the GPU: the escape hatch for a broken driver.
    Cpu,
    /// A specific backend; a preference, not a requirement: see [`review`].
    Named(String),
}

/// Read `llm.binaryBackend` from a resolved settings record. An unrecognised
/// string is a named backend (warning + CPU) rather than an error.
pub fn acceleration(settings: &SettingsRecord) -> Acceleration {
    match str_setting(settings, "llm.binaryBackend", "auto").as_str() {
        "cpu" => Acceleration::Cpu,
        // Blank is how "never touched" reads on disk; auto is the default.
        "auto" | "" => Acceleration::Auto,
        named => Acceleration::Named(named.to_string()),
    }
}

/// The directories to load from, in order: `install` (beside the core binary;
/// the CPU baseline ships here, so it loads for every preference) and
/// `<root>/backends/` (consent-downloaded GPU modules; absent is zero modules).
/// `cpu` skips the second entirely: a loaded module still registers its device.
pub fn directories(accel: &Acceleration, install: &Path, root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![install.to_path_buf()];
    if *accel != Acceleration::Cpu {
        dirs.push(root.join("backends"));
    }
    dirs
}

/// `cpu` pins zero layers regardless of `llm.gpuLayers`; everything else
/// leaves `load_spec`'s answer alone.
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
    /// Those that are not the CPU device; llama.cpp reports a coarse kind, not
    /// the backend name.
    pub accelerated: usize,
}

/// Judge the registration outcome: the warning to log, if any. Zero devices is
/// fatal (the CPU module is missing) and names the searched directories.
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
        // A preference that could not be honoured; continue on what registered.
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
            );
        }
    }

    #[test]
    fn every_preference_loads_the_install_directory_because_the_cpu_module_is_there() {
        for accel in [
            Acceleration::Auto,
            Acceleration::Cpu,
            Acceleration::Named("cuda".to_string()),
        ] {
            let dirs = directories(&accel, &install(), &root());
            assert_eq!(dirs.first(), Some(&install()), "{accel:?}");
        }
    }

    #[test]
    fn cpu_does_not_even_look_at_the_downloaded_backends() {
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
        assert_eq!(gpu_layers_override(&Acceleration::Cpu), Some(0));
        assert_eq!(gpu_layers_override(&Acceleration::Auto), None);
        assert_eq!(
            gpu_layers_override(&Acceleration::Named("metal".to_string())),
            None
        );
    }

    #[test]
    fn zero_devices_fails_loudly_and_names_where_it_looked() {
        let dirs = directories(&Acceleration::Auto, &install(), &root());
        let err = review(&Acceleration::Auto, Registered::default(), &dirs).unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("/opt/loca") && message.contains("backends"),
            "{message}"
        );
        assert!(message.contains("CPU backend"), "{message}");
    }

    #[test]
    fn a_cpu_only_registration_is_fine_for_auto_and_for_cpu() {
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
        let found = Registered {
            total: 1,
            accelerated: 0,
        };
        let dirs = directories(&Acceleration::Auto, &install(), &root());
        let warning = review(&Acceleration::Named("vulkan".to_string()), found, &dirs)
            .unwrap()
            .unwrap();
        assert!(warning.contains("vulkan"), "{warning}");

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

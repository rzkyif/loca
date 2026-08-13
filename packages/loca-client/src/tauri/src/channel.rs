// Install channel. Keeps the client's on-disk state and OS-keychain
// entries isolated per channel, mirroring loca-core's LOCA_CHANNEL.
//
// Every channel (including stable) lives under ~/.loca/<channel>/, so a
// dev or latest build never reads or clobbers a stable install's data:
//
//   stable → ~/.loca/stable/{core,client}   keychain "loca-client"
//   dev    → ~/.loca/dev/{core,client}       keychain "loca-client-dev"
//   latest   → ~/.loca/latest/{core,client}      keychain "loca-client-latest"
//
// Resolution order: the runtime LOCA_CHANNEL env var (set by
// `deno task dev` on desktop) wins, then the value baked in at build time via
// `option_env!` (build.rs bakes it from the `channel` file each orchestrator
// writes, which is how a shipped bundle - desktop OR mobile - pins its
// channel), else "stable". Unknown values fall back to "stable" rather than
// crash the UI.
//
// Models are deliberately NOT channel-scoped on the core side; they live at
// the shared ~/.loca/models. The client never touches the models dir, so
// nothing here needs to special-case it.

use std::path::{Path, PathBuf};

/// Map a raw channel string (env / build-time) to one of the known channels,
/// defaulting unknown / empty / missing values to "stable". Pure so it can be
/// unit-tested without touching the process environment.
fn normalize(raw: Option<&str>) -> &'static str {
    match raw.map(str::trim) {
        Some("dev") => "dev",
        Some("latest") => "latest",
        _ => "stable",
    }
}

/// The active channel: one of "stable", "dev", "latest".
pub fn channel() -> &'static str {
    let from_env = std::env::var("LOCA_CHANNEL").ok();
    normalize(from_env.as_deref().or(option_env!("LOCA_CHANNEL")))
}

fn channel_root_for(home: &Path, channel: &str) -> PathBuf {
    home.join(".loca").join(channel)
}

/// `~/.loca/<channel>`: the per-channel root for core + client state.
pub fn channel_root(home: &Path) -> PathBuf {
    channel_root_for(home, channel())
}

fn keychain_service_for(channel: &str) -> String {
    if channel == "stable" {
        "loca-client".to_string()
    } else {
        format!("loca-client-{channel}")
    }
}

/// OS-keychain service name, namespaced per channel. Stable keeps the bare
/// "loca-client" so existing entries keep resolving.
pub fn keychain_service() -> String {
    keychain_service_for(channel())
}

/// Suffix that namespaces per-channel binaries + OS service labels. Stable is
/// bare; dev/latest get "-dev"/"-latest". Mirrors core paths.ts channelSuffix().
fn channel_suffix_for(channel: &str) -> &'static str {
    match channel {
        "dev" => "-dev",
        "latest" => "-latest",
        _ => "",
    }
}

fn channel_port_offset(channel: &str) -> u16 {
    match channel {
        "latest" => 10,
        "dev" => 20,
        _ => 0,
    }
}

fn core_port_for(channel: &str) -> u16 {
    7800 + channel_port_offset(channel)
}

/// Default local-core HTTP port for this channel. Stable 7800; dev/latest are
/// offset so both cores can run as services at once. Mirrors core paths.ts
/// corePort().
pub fn core_port() -> u16 {
    core_port_for(channel())
}

/// Default local llama-server / speech ports for this channel, offset
/// in lockstep with the core port. Mirror core paths.ts llmPort()/speechPort().
/// The UI uses these as fallbacks when the paired core hasn't overridden
/// llm.port / stt.port, so a latest client tokenizes against the latest llama
/// (7711) instead of stable's 7701.
pub fn llm_port() -> u16 {
    7701 + channel_port_offset(channel())
}

pub fn stt_port() -> u16 {
    7702 + channel_port_offset(channel())
}

fn core_binary_name_for(channel: &str, windows: bool) -> String {
    let suffix = channel_suffix_for(channel);
    if windows {
        format!("loca-core{suffix}.exe")
    } else {
        format!("loca-core{suffix}")
    }
}

/// On-disk filename of the locally-installed core binary for this channel
/// (loca-core / loca-core-latest, plus .exe on Windows). Mirrors core
/// paths.ts channelBinName("loca-core").
pub fn core_binary_name() -> String {
    core_binary_name_for(channel(), cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalize_maps_known_channels_and_defaults_to_stable() {
        assert_eq!(normalize(None), "stable");
        assert_eq!(normalize(Some("")), "stable");
        assert_eq!(normalize(Some("stable")), "stable");
        assert_eq!(normalize(Some("dev")), "dev");
        assert_eq!(normalize(Some("latest")), "latest");
        assert_eq!(normalize(Some("  dev  ")), "dev"); // trims whitespace
        assert_eq!(normalize(Some("nonsense")), "stable"); // unknown → stable
    }

    #[test]
    fn channel_root_nests_under_dot_loca() {
        let home = PathBuf::from("/home/u");
        assert_eq!(
            channel_root_for(&home, "stable"),
            PathBuf::from("/home/u/.loca/stable")
        );
        assert_eq!(
            channel_root_for(&home, "dev"),
            PathBuf::from("/home/u/.loca/dev")
        );
        assert_eq!(
            channel_root_for(&home, "latest"),
            PathBuf::from("/home/u/.loca/latest")
        );
    }

    #[test]
    fn keychain_service_is_bare_on_stable_and_suffixed_otherwise() {
        assert_eq!(keychain_service_for("stable"), "loca-client");
        assert_eq!(keychain_service_for("dev"), "loca-client-dev");
        assert_eq!(keychain_service_for("latest"), "loca-client-latest");
    }

    #[test]
    fn channel_suffix_is_bare_on_stable_and_suffixed_otherwise() {
        assert_eq!(channel_suffix_for("stable"), "");
        assert_eq!(channel_suffix_for("dev"), "-dev");
        assert_eq!(channel_suffix_for("latest"), "-latest");
    }

    #[test]
    fn core_port_offsets_per_channel() {
        assert_eq!(core_port_for("stable"), 7800);
        assert_eq!(core_port_for("latest"), 7810);
        assert_eq!(core_port_for("dev"), 7820);
    }

    #[test]
    fn core_binary_name_suffixes_per_channel() {
        assert_eq!(core_binary_name_for("stable", false), "loca-core");
        assert_eq!(core_binary_name_for("latest", false), "loca-core-latest");
        assert_eq!(core_binary_name_for("latest", true), "loca-core-latest.exe");
        assert_eq!(core_binary_name_for("dev", false), "loca-core-dev");
    }
}

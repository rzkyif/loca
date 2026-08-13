use std::{env, fs, path::Path};

fn main() {
    // Bake the install channel into the binary. channel.rs reads it back with
    // option_env!("LOCA_CHANNEL") as its runtime default (the runtime env still
    // wins when present, e.g. `deno task dev` on desktop).
    //
    // We can't rely on the ambient LOCA_CHANNEL env reaching this compile: a
    // mobile build runs cargo inside Xcode's script phase / a persistent Gradle
    // daemon, neither of which forwards it reliably (Xcode drops it, and the
    // daemon pins whatever channel it first started with). So each build
    // orchestrator (scripts/dev.ts, build-client/ios/android.ts) writes the
    // channel to a `channel` file next to this script, and we bake THAT - a
    // deterministic on-disk value no build tool can strip or stale out. A direct
    // LOCA_CHANNEL env is honored as a fallback for a bare `cargo build`.
    println!("cargo:rerun-if-env-changed=LOCA_CHANNEL");
    let channel_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("channel");
    println!("cargo:rerun-if-changed={}", channel_file.display());

    let channel = fs::read_to_string(&channel_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("LOCA_CHANNEL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "stable".to_string());
    println!("cargo:rustc-env=LOCA_CHANNEL={channel}");

    // tauri-build embeds a Windows app manifest whose only content is a
    // dependency on Common Controls v6, which is what makes the loader bind
    // comctl32 to the WinSxS v6 copy that exports `TaskDialogIndirect` (called
    // by tauri-runtime-wry's error dialog). It reaches the bin target ONLY:
    // embed-resource emits `cargo:rustc-link-arg-bins`. So the `cargo test
    // --lib` harness exe binds comctl32 to the v5.82 copy in System32 instead,
    // which has no such export, and Windows kills it at load with
    // STATUS_ENTRYPOINT_NOT_FOUND before a single test runs. Embed the same
    // dependency into the test binaries. link.exe merges every /MANIFESTINPUT,
    // so this adds to (never replaces) the manifest rustc supplies.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-test.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }

    tauri_build::build();
}

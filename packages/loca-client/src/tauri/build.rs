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
    // STATUS_ENTRYPOINT_NOT_FOUND before a single test runs.
    //
    // Declare the same dependency to the linker instead. `rustc-link-arg-tests`
    // does not cover the lib's unit-test harness (only `tests/*.rs` targets), so
    // this has to be the unscoped form, which also reaches the bin and the
    // cdylib. /MANIFESTDEPENDENCY is the shape that stays safe there: it lands
    // in the side-by-side `<output>.manifest` file link.exe writes by default,
    // never in the image's resources, so it cannot collide with the RT_MANIFEST
    // tauri-build already puts in the bin. Executables with no embedded manifest
    // (every rustc-linked test exe) pick that file up at load.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }

    tauri_build::build();
}

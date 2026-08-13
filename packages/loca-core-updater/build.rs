// Windows auto-elevates any executable whose filename contains "update" (the OS
// installer-detection heuristic). That blocks `cargo test` on a UAC-enabled
// machine, since the unit-test binary is named `loca_core_updater-<hash>.exe`,
// and would force a UAC prompt every time core spawns the real updater during a
// self-update. Embedding an application manifest with a `requestedExecutionLevel`
// of `asInvoker` opts this binary out of the heuristic so it runs un-elevated.
//
// The catch-all `rustc-link-arg` covers binaries and tests (the unit-test
// harness of this bin crate builds under the bin target, so there is no separate
// test target to select), so both the shipped binary and `cargo test` run
// un-elevated. MSVC only: the `/MANIFEST` flags are link.exe-specific, and the
// updater is only ever built for Windows with the MSVC toolchain.
fn main() {
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows_msvc {
        return;
    }
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("loca-core-updater.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
}

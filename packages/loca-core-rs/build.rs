//! Builds llama.cpp and the C ABI shim that `inference::shim` binds to.
//!
//! All of it is behind `local-inference`, OFF by default because `vendor/` is
//! gitignored and a fresh checkout must still build. The vendored tree is
//! pinned by SHA in `vendor/.llama-pin`, written by `scripts/vendor-llama.ts`.
//!
//! Two link modes, following the target rather than a knob. Desktop is shared
//! plus `GGML_BACKEND_DL`, so one binary serves a CUDA, ROCm, Vulkan or Metal
//! machine with the backend fetched on consent. Mobile is fully static: it
//! loads no backend module and downloads no executable code, as W^X requires.

use std::path::{Path, PathBuf};

/// The vendored llama.cpp commit. Keep in sync with `PIN` in
/// `scripts/vendor-llama.ts`; upstream release tag `b10415`. Bumping it means
/// re-vendor, rebuild, run the feature-on gate and the shim's mutation checks.
const LLAMA_CPP_PIN: &str = "1d2869c6e54d5003f3927a79efbca0fefa034a6d";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/inference/shim/shim.cpp");
    println!("cargo::rerun-if-changed=src/inference/shim/loca_llama.h");
    println!("cargo::rustc-check-cfg=cfg(loca_llama_link, values(\"static\", \"shared\"))");

    // Before the feature gate: the trust root depends on no feature.
    emit_signing_key();

    if std::env::var_os("CARGO_FEATURE_LOCAL_INFERENCE").is_none() {
        return;
    }

    let vendor = match vendor_dir() {
        Ok(v) => v,
        Err(e) => fail(&e),
    };
    println!(
        "cargo::rerun-if-changed={}",
        vendor.join("../.llama-pin").display()
    );

    let target_os = env("CARGO_CFG_TARGET_OS");
    let mode = LinkMode::for_target(&target_os);

    let dst = build_llama(&vendor, &target_os, mode);
    build_shim(&vendor);
    emit_links(&dst, &target_os, mode);

    println!("cargo::rustc-cfg=loca_llama_link=\"{}\"", mode.as_str());
    println!(
        "cargo::rustc-env=LOCA_LLAMA_VENDOR_DIR={}",
        vendor.display()
    );
    // Where the backend modules landed. Only meaningful in shared mode, where
    // nothing can run without loading one; the tests point the shim at this.
    println!(
        "cargo::rustc-env=LOCA_LLAMA_BACKEND_DIR={}",
        dst.join("bin").display()
    );
    // The shim's object file, for the no-call-path audit in `shim::tests`.
    // Auditing our own object is the check that survives desktop mode, where
    // the shared library legitimately contains an HTTP client we never reach.
    println!(
        "cargo::rustc-env=LOCA_LLAMA_SHIM_OBJ_DIR={}",
        env("OUT_DIR")
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    /// Mobile. Everything archived into the binary, no runtime loading.
    Static,
    /// Desktop. Shared ggml/llama plus dlopen-ed backend modules.
    Shared,
}

impl LinkMode {
    fn for_target(target_os: &str) -> Self {
        match target_os {
            "android" | "ios" => Self::Static,
            _ => Self::Shared,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Shared => "shared",
        }
    }

    fn is_static(self) -> bool {
        self == Self::Static
    }
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// Aborts the build with `msg`. Panicking is cargo's documented way for a build
/// script to fail; the allow is scoped here rather than file-wide.
#[allow(clippy::panic)]
fn fail(msg: &str) -> ! {
    panic!("{msg}");
}

/// Bakes the committed Ed25519 public key into the binary from
/// `data/signing-keys.json`, the one file the release pipeline writes. Reading
/// it here (with `rerun-if-changed`) is what makes that write load-bearing: a
/// hardcoded constant would silently keep trusting a rotated-out key.
fn emit_signing_key() {
    let path = PathBuf::from(env("CARGO_MANIFEST_DIR")).join("data/signing-keys.json");
    println!("cargo::rerun-if-changed={}", path.display());

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => fail(&format!(
            "cannot read the signing trust root at {}: {err}",
            path.display()
        )),
    };
    let key = match extract_public_key(&raw) {
        Ok(key) => key,
        Err(err) => fail(&format!("{} is unusable: {err}", path.display())),
    };

    let out = PathBuf::from(env("OUT_DIR")).join("signing_key.rs");
    let body = format!(
        "/// The Ed25519 verifying key every compiled core trusts, baked in from\n\
         /// `data/signing-keys.json` by `build.rs`.\n\
         pub const SIGNING_PUBLIC_KEY: &str = \"{key}\";\n"
    );
    if let Err(err) = std::fs::write(&out, body) {
        fail(&format!("cannot write {}: {err}", out.display()));
    }
}

/// Pull `publicKey` out of the trust-root file, refusing anything unexpected.
/// Hand-parsed to keep `serde_json` out of the build dependencies; every branch
/// refuses rather than falling back.
fn extract_public_key(raw: &str) -> Result<String, String> {
    const FIELD: &str = "\"publicKey\"";
    let after = raw
        .find(FIELD)
        .map(|at| &raw[at + FIELD.len()..])
        .ok_or_else(|| "no `publicKey` field".to_string())?;
    let after = after
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| "`publicKey` is not followed by `:`".to_string())?;
    let after = after
        .trim_start()
        .strip_prefix('"')
        .ok_or_else(|| "`publicKey` is not a string".to_string())?;
    let end = after
        .find('"')
        .ok_or_else(|| "`publicKey` string is unterminated".to_string())?;
    let key = &after[..end];

    // 32 raw bytes is exactly 44 characters of padded standard base64, so a
    // truncated or re-encoded key fails the build rather than every signature.
    if key.len() != 44 || !key.ends_with('=') || key[..43].contains('=') {
        return Err(format!(
            "`publicKey` is not 44 characters of padded base64 (a 32-byte Ed25519 key): {key:?}"
        ));
    }
    if !key[..43]
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return Err(format!("`publicKey` is not standard base64: {key:?}"));
    }
    Ok(key.to_string())
}

/// The vendored llama.cpp tree, verified to be at [`LLAMA_CPP_PIN`]. Reads
/// `vendor/.llama-pin` rather than running git, which may not be installed.
fn vendor_dir() -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "cannot locate the repo root from CARGO_MANIFEST_DIR".to_string())?;
    let vendor = root.join("vendor").join("llama.cpp");
    let pin_file = root.join("vendor").join(".llama-pin");

    let advice = "run: deno run -A scripts/vendor-llama.ts";
    if !vendor.join("CMakeLists.txt").is_file() {
        return Err(format!(
            "vendor/llama.cpp is missing or incomplete.\n{advice}"
        ));
    }
    let pinned = std::fs::read_to_string(&pin_file)
        .map_err(|e| format!("cannot read vendor/.llama-pin ({e}).\n{advice}"))?;
    let pinned = pinned.trim();
    if pinned != LLAMA_CPP_PIN {
        return Err(format!(
            "vendor/llama.cpp is at {pinned}, but build.rs pins {LLAMA_CPP_PIN}.\n\
             re-vendor with: deno run -A scripts/vendor-llama.ts --force",
        ));
    }
    Ok(vendor)
}

fn build_llama(vendor: &Path, target_os: &str, mode: LinkMode) -> PathBuf {
    let mut cfg = cmake::Config::new(vendor);
    let apple = matches!(target_os, "macos" | "ios");

    cfg.define("CMAKE_BUILD_TYPE", "Release")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        // Jinja chat templates, per-format tool-call extraction, GBNF
        // constraint and the <think> split all live in common/chat.cpp.
        .define("LLAMA_BUILD_COMMON", "ON")
        .define("LLAMA_BUILD_TESTS", "OFF")
        .define("LLAMA_BUILD_TOOLS", "OFF")
        // libmtmd: what makes a local model able to see an image. A standalone
        // build, so it drags in none of the rest of tools/.
        .define("LLAMA_BUILD_MTMD", "ON")
        .define("LLAMA_BUILD_EXAMPLES", "OFF")
        .define("LLAMA_BUILD_SERVER", "OFF")
        .define("LLAMA_BUILD_APP", "OFF")
        // `LLAMA_USE_PREBUILT_UI` defaults ON and fetches a web UI mid-build.
        .define("LLAMA_BUILD_UI", "OFF")
        .define("LLAMA_USE_PREBUILT_UI", "OFF")
        // Both default ON: `llama-common` carries an HTTP downloader and a
        // subprocess spawner alongside chat.cpp, and mobile W^X forbids the
        // latter outright.
        .define("LLAMA_OPENSSL", "OFF")
        .define("LLAMA_SUBPROCESS", "OFF")
        .define("LLAMA_LLGUIDANCE", "OFF")
        // -march=native makes the artifact host-specific.
        .define("GGML_NATIVE", "OFF")
        // A build that silently depends on a host tool is not reproducible.
        .define("GGML_CCACHE", "OFF")
        // No libomp in the NDK sysroot or the iOS SDK, and ggml's CPU backend
        // has its own thread pool. Off everywhere so all targets match.
        .define("GGML_OPENMP", "OFF")
        .define("GGML_CPU", "ON");

    if mode.is_static() {
        cfg.define("BUILD_SHARED_LIBS", "OFF")
            .define("GGML_BACKEND_DL", "OFF");
    } else {
        // GGML_BACKEND_DL is a hard FATAL_ERROR without BUILD_SHARED_LIBS.
        cfg.define("BUILD_SHARED_LIBS", "ON")
            .define("GGML_BACKEND_DL", "ON");
    }

    if apple {
        cfg.define("GGML_ACCELERATE", "ON")
            .define("GGML_BLAS", "OFF");
    } else {
        cfg.define("GGML_ACCELERATE", "OFF")
            .define("GGML_BLAS", "OFF");
    }

    // Metal on macOS only; iOS is CPU + Accelerate for v1.
    if target_os == "macos" {
        cfg.define("GGML_METAL", "ON")
            .define("GGML_METAL_EMBED_LIBRARY", "ON");
    } else {
        cfg.define("GGML_METAL", "OFF");
    }

    if target_os == "android" {
        if let Some(ndk) = android_ndk() {
            cfg.define(
                "CMAKE_TOOLCHAIN_FILE",
                ndk.join("build/cmake/android.toolchain.cmake"),
            )
            .define("ANDROID_ABI", "arm64-v8a")
            .define("ANDROID_PLATFORM", "android-24");
        } else {
            fail(
                "android target needs the NDK: set ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) \
                 to an NDK r26+ installation",
            );
        }
    }

    if target_os == "ios" {
        cfg.define("CMAKE_SYSTEM_NAME", "iOS")
            .define("CMAKE_OSX_SYSROOT", "iphoneos")
            .define("CMAKE_OSX_DEPLOYMENT_TARGET", "14.0");
    }

    cfg.build()
}

fn android_ndk() -> Option<PathBuf> {
    for key in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"] {
        if let Some(v) = std::env::var_os(key) {
            let p = PathBuf::from(v);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    // The default SDK layout, newest NDK first.
    let base = PathBuf::from(env("HOME")).join("Library/Android/sdk/ndk");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    versions.pop()
}

/// The NDK's toolchain `bin/` for this host. Needed because `cc` derives tool
/// names from the triple (`aarch64-linux-android-clang++`, `-ar`) and the NDK
/// ships neither: its drivers carry the API level and its archiver is `llvm-ar`.
/// The cmake half never hits this, going through `android.toolchain.cmake`.
fn android_ndk_bin() -> Option<PathBuf> {
    let prebuilt = android_ndk()?.join("toolchains/llvm/prebuilt");
    // The one host directory's name does not always match the host arch (an
    // x86_64 NDK under Rosetta), so read it rather than composing it.
    let host_dir = std::fs::read_dir(&prebuilt)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_dir())?;
    let bin = host_dir.join("bin");
    bin.is_dir().then_some(bin)
}

fn build_shim(vendor: &Path) {
    let mut build = cc::Build::new();
    if env("CARGO_CFG_TARGET_OS") == "android" {
        match android_ndk_bin() {
            Some(bin) => {
                // API 24 matches `ANDROID_PLATFORM` on the cmake side.
                build.compiler(bin.join("aarch64-linux-android24-clang++"));
                build.archiver(bin.join("llvm-ar"));
            }
            // Not a silent fallback: `cc` would fail anyway, naming a missing
            // tool rather than the missing NDK.
            None => fail(
                "android target needs the NDK toolchain: set ANDROID_NDK_HOME (or \
                 ANDROID_NDK_ROOT / NDK_HOME) to an NDK r26+ installation",
            ),
        }
    }
    build
        .cpp(true)
        .std("c++17")
        .file("src/inference/shim/shim.cpp")
        .include("src/inference/shim")
        .include(vendor.join("common"))
        .include(vendor.join("include"))
        .include(vendor.join("ggml/include"))
        // mtmd.h and mtmd-helper.h: public, just not under `include/`.
        .include(vendor.join("tools/mtmd"))
        // nlohmann/json.hpp, from llama.cpp's own vendor dir.
        .include(vendor.join("vendor"))
        .warnings(false)
        .compile("loca_llama_shim");
}

/// Link names of the libraries cmake actually installed, most-dependent first.
/// Discovered rather than hardcoded: the set differs between the two modes and
/// between llama.cpp versions.
fn installed_libs(lib_dir: &Path, mode: LinkMode) -> Vec<String> {
    let ext = if mode.is_static() {
        ".a"
    } else if cfg!(target_os = "macos") {
        ".dylib"
    } else {
        ".so"
    };

    let mut found: Vec<String> = std::fs::read_dir(lib_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.strip_prefix("lib")?.strip_suffix(ext)?;
            // Rejects the versioned aliases (`libggml.0.19.0.dylib` leaves
            // `ggml.0.19.0`), keeping only the unversioned link name.
            (!stem.contains('.')).then(|| stem.to_owned())
        })
        .collect();

    // Most dependent first; anything unrecognised sorts after.
    let rank = |n: &str| match n {
        // Above llama-common: mtmd links `ggml` and `llama`, and only our shim
        // links mtmd.
        "mtmd" => 0,
        "llama-common" => 1,
        "llama-common-base" => 2,
        "llama" => 3,
        "ggml" => 4,
        "ggml-base" => 5,
        _ => 6,
    };
    found.sort_by_key(|n| (rank(n), n.clone()));
    found
}

/// Copies the runtime libraries next to cargo's own artifacts, which is what
/// lets the rpath stay purely relative: `@loader_path` then resolves in
/// development exactly as it will in an install. Writing outside `OUT_DIR` is
/// deliberate, and failures are fatal, because a silent partial copy turns a
/// build error into a loader error nobody can read.
fn stage_runtime_libs(dst: &Path) {
    let out_dir = PathBuf::from(env("OUT_DIR"));
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        fail(&format!(
            "cannot locate the profile directory from OUT_DIR ({}); the runtime \
             libraries have nowhere to be staged and the relative rpath would not resolve",
            out_dir.display()
        ));
    };
    let targets = [profile_dir.to_path_buf(), profile_dir.join("deps")];

    let mut staged = 0usize;
    for src_dir in [dst.join("lib"), dst.join("bin")] {
        let Ok(entries) = std::fs::read_dir(&src_dir) else {
            // `bin/` legitimately does not exist in every configuration; only an
            // empty result overall is a problem, and that is checked below.
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name();
            let is_runtime = name.to_string_lossy();
            if !(is_runtime.contains(".dylib")
                || is_runtime.contains(".so")
                || is_runtime.ends_with(".dll"))
            {
                continue;
            }
            for target in &targets {
                if let Err(e) = std::fs::create_dir_all(target) {
                    fail(&format!(
                        "cannot create {} to stage llama.cpp's runtime libraries: {e}",
                        target.display()
                    ));
                }
                if let Err(e) = std::fs::copy(&path, target.join(&name)) {
                    fail(&format!(
                        "cannot stage {} into {}: {e}. The rpath is relative by design, \
                         so the build only runs when these are beside cargo's artifacts.",
                        path.display(),
                        target.display()
                    ));
                }
            }
            staged += 1;
        }
    }

    if staged == 0 {
        fail(&format!(
            "the shared build produced no runtime libraries to stage under {}; \
             nothing would resolve through the relative rpath at run time",
            dst.display()
        ));
    }
}

fn emit_links(dst: &Path, target_os: &str, mode: LinkMode) {
    let lib_dir = dst.join("lib");
    for dir in [&lib_dir, &dst.join("bin")] {
        if dir.is_dir() {
            println!("cargo::rustc-link-search=native={}", dir.display());
        }
    }

    let kind = if mode.is_static() { "static" } else { "dylib" };
    let libs = installed_libs(&lib_dir, mode);
    if libs.is_empty() {
        fail(&format!(
            "no llama.cpp libraries were installed into {}; the cmake build \
             produced nothing to link",
            lib_dir.display()
        ));
    }
    // Emitted twice: ggml and ggml-base are mutually referential and a single
    // pass leaves undefined symbols on linkers that do not re-scan.
    for _ in 0..2 {
        for lib in &libs {
            println!("cargo::rustc-link-lib={kind}={lib}");
        }
    }

    if mode.is_static() {
        // Only static mode archives the backends in; shared mode loads them at
        // runtime from an explicit directory and must not link them.
        for lib in ["ggml-cpu", "ggml-metal", "ggml-blas"] {
            let stem = if target_os == "windows" { "" } else { "lib" };
            if lib_dir.join(format!("{stem}{lib}.a")).is_file() {
                println!("cargo::rustc-link-lib=static={lib}");
            }
        }
    } else {
        // Relative rpaths only. Self-update anchors the old install by moving
        // the whole directory aside, so an absolute rpath baking in the build
        // directory would break rollback exactly when it is needed. No absolute
        // rpath is added as a dev convenience either: it would satisfy the
        // loader in every test and hide the regression.
        match target_os {
            "macos" | "ios" => {
                for rp in ["@loader_path", "@loader_path/../lib", "@executable_path"] {
                    println!("cargo::rustc-link-arg=-Wl,-rpath,{rp}");
                }
            }
            "windows" => {
                // The loader resolves DLLs beside the executable; the copy
                // below is what puts them there.
            }
            _ => {
                for rp in ["$ORIGIN", "$ORIGIN/../lib"] {
                    println!("cargo::rustc-link-arg=-Wl,-rpath,{rp}");
                }
                // Some toolchains refuse $ORIGIN expansion without this.
                println!("cargo::rustc-link-arg=-Wl,-z,origin");
            }
        }
        stage_runtime_libs(dst);
    }

    match target_os {
        "macos" | "ios" => {
            println!("cargo::rustc-link-lib=c++");
            for fw in [
                "Foundation",
                "Accelerate",
                "Metal",
                "MetalKit",
                "QuartzCore",
            ] {
                println!("cargo::rustc-link-lib=framework={fw}");
            }
        }
        "windows" => {}
        _ => println!("cargo::rustc-link-lib=stdc++"),
    }
}

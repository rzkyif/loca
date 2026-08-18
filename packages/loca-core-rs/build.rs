//! Builds llama.cpp and the C ABI shim that `inference::shim` binds to.
//!
//! Everything here is behind the `local-inference` feature, which is OFF by
//! default. Without it this script emits its `rerun-if-changed` lines and
//! returns, so a build that does not want llama.cpp pays one build-script
//! compile and nothing else. The feature is off because CI builds this crate on
//! three runners from a fresh checkout where `vendor/` (gitignored) does not
//! exist, and because a cold llama.cpp build does not fit that job's budget.
//!
//! # The pin
//!
//! llama.cpp is pinned to a SHA, not a tag: a tag can move, and the point of the
//! pin is that the tree we build is the tree the S0.2 spike was verified against
//! (D-002). `scripts/vendor-llama.ts` fetches it and records what it checked out
//! in `vendor/.llama-pin`; this script refuses to build if the two disagree.
//!
//! # Two link modes, and why they are not a preference (D-106)
//!
//! `GGML_BACKEND_DL` is a hard `FATAL_ERROR` unless `BUILD_SHARED_LIBS` is on
//! (`ggml/src/CMakeLists.txt`), so the two modes are:
//!
//! - **Desktop: shared + backend-DL.** ggml, llama and llama-common are shared
//!   libraries beside the core binary, and every backend (the CPU baseline
//!   included) is a module loaded at runtime. This is what lets one core binary
//!   serve a CUDA, ROCm, Vulkan or Metal machine, with the GPU backend fetched
//!   on consent instead of a whole per-flavour distribution.
//! - **Mobile: fully static.** Android and iOS are CPU-only for v1, load no
//!   backend module and download no executable code, which is what W^X and App
//!   Store 2.5.2 require.
//!
//! The mode follows the target and is not configurable, because the two
//! platforms need opposite things and a knob would only let them be set wrong.

use std::path::{Path, PathBuf};

/// The vendored llama.cpp commit. Keep in sync with `PIN` in
/// `scripts/vendor-llama.ts`; upstream release tag `b10415`.
///
/// This is the exact commit S0.2 was verified against (D-002), which is why it
/// is this one and not a more recent tag. Bumping it is a ritual, not a
/// default: re-vendor, rebuild, run the feature-on gate, and re-run the shim's
/// mutation checks, because a bump that quietly changes `common_chat_parse`
/// semantics is exactly what those detect.
const LLAMA_CPP_PIN: &str = "1d2869c6e54d5003f3927a79efbca0fefa034a6d";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/inference/shim/shim.cpp");
    println!("cargo::rerun-if-changed=src/inference/shim/loca_llama.h");
    println!("cargo::rustc-check-cfg=cfg(loca_llama_link, values(\"static\", \"shared\"))");

    // BEFORE the feature gate below, and that placement is the whole point: the
    // trust root is not optional and does not depend on any feature.
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
    // The shim's object file, for the no-call-path audit in `shim::tests`
    // (D-108). Auditing our own object is the check that survives the desktop
    // mode, where the shared library legitimately contains an HTTP client we
    // must never reach.
    println!(
        "cargo::rustc-env=LOCA_LLAMA_SHIM_OBJ_DIR={}",
        env("OUT_DIR")
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    /// Mobile. Everything archived into the binary, no runtime loading.
    Static,
    /// Desktop. Shared ggml/llama plus dlopen-ed backend modules (D-106).
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

/// Aborts the build with `msg`.
///
/// The crate lints `clippy::panic` because a panic in a running core takes the
/// service down. A build script is a different hazard entirely: it runs on a
/// developer's or CI machine at build time, never in the shipped binary, and
/// cargo's documented way for it to report failure IS to panic, which prints the
/// message and stops the build. There is no `Result` to return to; `main` has
/// nowhere to propagate one.
///
/// Scoped to this one function rather than allowed file-wide, so any other
/// panic-shaped code added to this script still has to justify itself.
#[allow(clippy::panic)]
fn fail(msg: &str) -> ! {
    panic!("{msg}");
}

/// Bakes the committed Ed25519 public key into the binary, from the one file
/// the release pipeline writes.
///
/// # Why this is a build step and not a constant
///
/// The trust root has ONE writer: `writeSigningKeys` in `scripts/release/lib.ts`,
/// which `ci-build.ts` calls from `LOCA_SIGNING_PUBLIC_KEY_B64` immediately
/// before the core build. Four Rust modules used to carry the key as a hardcoded
/// literal instead, so that write reached nothing the binary compiled against.
/// Both directions of the resulting failure are silent at build time and total
/// at runtime: a release signed with a rotated key fails the catalog, the binary
/// manifest, self-update and the seeded built-in extension all at once, and a
/// retired key keeps being trusted by every core already built. Nothing compared
/// the four literals to the file, or to each other.
///
/// Reading the file HERE is what makes the release step's write load-bearing.
/// `rerun-if-changed` ties the two together, so changing the key rebuilds the
/// crate rather than leaving a stale constant compiled in.
///
/// The file stays under `packages/loca-core/data/` because that is where the
/// TypeScript core imports it from and where the release script already writes
/// it; one file with one writer is the property that matters, not which package
/// holds it. It moves at cutover, and this path is how the build notices.
fn emit_signing_key() {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let path = match manifest.parent() {
        Some(packages) => packages
            .join("loca-core")
            .join("data")
            .join("signing-keys.json"),
        None => fail("cannot locate the repo packages dir from CARGO_MANIFEST_DIR"),
    };
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
         /// `packages/loca-core/data/signing-keys.json` by `build.rs`.\n\
         pub const SIGNING_PUBLIC_KEY: &str = \"{key}\";\n"
    );
    if let Err(err) = std::fs::write(&out, body) {
        fail(&format!("cannot write {}: {err}", out.display()));
    }
}

/// Pull `publicKey` out of the trust-root file, refusing anything unexpected.
///
/// Hand-parsed rather than via `serde_json`, because the file has exactly one
/// machine-written shape and a build dependency compiled for the host on every
/// build is a worse trade than twenty strict lines. Strict is the operative
/// word: every branch below refuses rather than falling back, since a trust root
/// that silently parses as something else is the failure this whole function
/// exists to prevent.
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

    // A raw Ed25519 verifying key is 32 bytes, which is exactly 44 characters of
    // padded standard base64. Checked here so a truncated or re-encoded key is a
    // BUILD failure with a name on it, rather than a signature check that fails
    // everywhere at runtime for no stated reason.
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

/// The vendored llama.cpp tree, verified to be at [`LLAMA_CPP_PIN`].
///
/// Verification reads `vendor/.llama-pin` rather than running git: a build
/// script must not depend on git being installed, and the pin file is what
/// `scripts/vendor-llama.ts` writes after checking the clone's actual HEAD.
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
        // The load-bearing one: Jinja chat templates, per-format tool-call
        // extraction, GBNF constraint and the <think> split all live in
        // common/chat.cpp, not in libllama. Building it needs none of the
        // tools, tests, examples or the server (D-002).
        .define("LLAMA_BUILD_COMMON", "ON")
        .define("LLAMA_BUILD_TESTS", "OFF")
        .define("LLAMA_BUILD_TOOLS", "OFF")
        // libmtmd, which is what makes a LOCAL model able to see an image.
        //
        // Upstream provides this option for exactly this case: "standalone
        // libmtmd build without pulling in the rest of the tools/ tree", and it
        // only fires when `LLAMA_BUILD_TOOLS` is OFF, which is how it is set
        // above. So this is one flag rather than a fork, and it does NOT drag in
        // the CLI, the server, or the rest of tools/.
        //
        // Without it the port advertised something it could not do: the model
        // catalog carries a `vision` capability and settings expose
        // `llm.supportImages`, while the shim linked `libllama` + `ggml` +
        // `common` and nothing that can encode an image. See D-177.
        .define("LLAMA_BUILD_MTMD", "ON")
        .define("LLAMA_BUILD_EXAMPLES", "OFF")
        .define("LLAMA_BUILD_SERVER", "OFF")
        .define("LLAMA_BUILD_APP", "OFF")
        // LLAMA_USE_PREBUILT_UI defaults ON and FETCHES a prebuilt web UI from
        // an HF bucket during the build. A build-time download makes the build
        // non-reproducible; both halves off.
        .define("LLAMA_BUILD_UI", "OFF")
        .define("LLAMA_USE_PREBUILT_UI", "OFF")
        // Both default ON. `llama-common` carries an HTTP downloader and a
        // subprocess spawner alongside chat.cpp; neither has any business in
        // this core, and a subprocess spawner is exactly what mobile W^X
        // forbids. See D-108 for why turning them off is necessary but not
        // sufficient in shared mode.
        .define("LLAMA_OPENSSL", "OFF")
        .define("LLAMA_SUBPROCESS", "OFF")
        .define("LLAMA_LLGUIDANCE", "OFF")
        // -march=native makes the artifact host-specific, which is fatal for
        // something we ship and for every cross build.
        .define("GGML_NATIVE", "OFF")
        // A build that silently depends on a host tool is not reproducible.
        .define("GGML_CCACHE", "OFF")
        // No libomp in the NDK sysroot or the iOS SDK, and ggml's CPU backend
        // has its own thread pool regardless. Off everywhere so all four
        // targets take the same path.
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

    // Metal on macOS only. iOS is CPU + Accelerate for v1; when Metal is picked
    // up there it needs no bundled .metallib, because
    // GGML_METAL_EMBED_LIBRARY defaults to the value of GGML_METAL and
    // compiles the shader library into the binary.
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

/// The NDK's toolchain `bin/` for this host.
///
/// `cc` derives its tool names from the target triple and looks for
/// `aarch64-linux-android-clang++` and `aarch64-linux-android-ar`. **The NDK
/// ships neither name**: its compiler drivers carry the API level
/// (`aarch64-linux-android24-clang++`) and its archiver is the unprefixed
/// `llvm-ar`. So `cc` fails with `ToolNotFound` and the build dies at the shim.
///
/// The cmake half never hit this because it goes through the NDK's own
/// `android.toolchain.cmake`, which resolves both itself. The two halves of this
/// build script find their tools by different routes, and only one was wired.
///
/// Found rather than assumed: what this replaces was a build that worked on a
/// machine with `CC_aarch64_linux_android` exported by hand and failed on every
/// CI runner, which sets only `ANDROID_NDK_HOME`.
fn android_ndk_bin() -> Option<PathBuf> {
    let prebuilt = android_ndk()?.join("toolchains/llvm/prebuilt");
    // One host directory per NDK (`darwin-x86_64`, `linux-x86_64`, ...), and its
    // name does not always match the host arch: the NDK ships an x86_64 build
    // that Apple silicon runs under Rosetta, so this reads the directory rather
    // than composing the name.
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
                // API 24 matches `ANDROID_PLATFORM` on the cmake side. Keep the
                // two together: a shim compiled against a different API level
                // than the libraries it calls is a link error at best.
                build.compiler(bin.join("aarch64-linux-android24-clang++"));
                build.archiver(bin.join("llvm-ar"));
            }
            // Not a silent fallback. Without these `cc` fails anyway, and its
            // message names a tool that does not exist rather than the missing
            // NDK, which is what made this hard to read the first time.
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
        // mtmd.h and mtmd-helper.h, which live in the tools tree rather than in
        // `include/`. Public headers by intent (the CMake target exports this
        // directory), just not co-located with llama's.
        .include(vendor.join("tools/mtmd"))
        // nlohmann/json.hpp: llama.cpp's own vendor dir, which common/
        // exports as a public include path.
        .include(vendor.join("vendor"))
        .warnings(false)
        .compile("loca_llama_shim");
}

/// Link names of the libraries cmake actually installed, ordered most-dependent
/// first.
///
/// Discovered rather than hardcoded, because the set genuinely differs between
/// the two modes and between llama.cpp versions: `llama-common-base` is declared
/// STATIC upstream, so in shared mode it is absorbed into `libllama-common` and
/// never installed at all. A fixed list is a build break waiting for the next
/// bump, which is exactly what this package is trying not to be.
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

    // Most dependent first; anything unrecognised sorts after, which is the
    // right default because a new library upstream adds is more likely to sit
    // above ggml than below it.
    let rank = |n: &str| match n {
        // Above llama-common, because mtmd links `ggml` and `llama` and nothing
        // links mtmd except our own shim.
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

/// Copies the runtime libraries next to cargo's own artifacts.
///
/// This is what lets the rpath be purely relative (D-116). Cargo puts binaries
/// in `target/<profile>/` and test binaries in `target/<profile>/deps/`; with
/// the dylibs and backend modules beside them, `@loader_path` resolves in
/// development exactly the way it will resolve in an install, so every test run
/// exercises the same mechanism a rollback depends on.
///
/// Writing outside `OUT_DIR` is unusual for a build script and is deliberate:
/// the alternative is an absolute rpath that would mask the failure this is
/// preventing.
///
/// Failures here are FATAL rather than best-effort. The previous version ignored
/// every error on the grounds that a missing library "surfaces at run time", but
/// D-116 makes correct staging load-bearing rather than convenient: the relative
/// rpath only resolves because these files are where this function puts them, so
/// a silent partial copy turns a build error into a confusing loader error, or
/// into a relocation test that passes for the wrong reason.
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
                        "cannot stage {} into {}: {e}. The rpath is relative by design \
                         (D-116), so the build only runs when these are beside cargo's \
                         artifacts.",
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
        // Backends are archived in only in static mode. In shared mode they are
        // modules loaded at runtime from an explicit directory and must NOT be
        // linked, which is the whole point of D-106.
        for lib in ["ggml-cpu", "ggml-metal", "ggml-blas"] {
            let stem = if target_os == "windows" { "" } else { "lib" };
            if lib_dir.join(format!("{stem}{lib}.a")).is_file() {
                println!("cargo::rustc-link-lib=static={lib}");
            }
        }
    } else {
        // RELATIVE rpaths only, and this is load-bearing rather than tidy
        // (D-116). Self-update anchors the old install by MOVING it aside, and
        // under D-106 that install is a directory containing these dylibs. An
        // absolute rpath bakes in the build directory, so the anchored tree
        // would not load: rollback would be broken precisely when it is needed,
        // and nothing would reveal it until a real rollback on a real machine,
        // because a build that runs in place never exercises the move.
        //
        // The dylibs already carry `@rpath/...` install names from cmake, so
        // making the consumer's rpath relative is the whole fix.
        //
        // No absolute rpath is emitted as a dev convenience either. That would
        // silently satisfy the loader during every test and hide a regression
        // here forever. Instead the libraries are copied next to cargo's
        // artifacts below, so `@loader_path` is the path every test run
        // actually resolves through.
        match target_os {
            "macos" | "ios" => {
                for rp in ["@loader_path", "@loader_path/../lib", "@executable_path"] {
                    println!("cargo::rustc-link-arg=-Wl,-rpath,{rp}");
                }
            }
            "windows" => {
                // The loader resolves DLLs from the executable's own directory,
                // so there is no rpath to set; the copy below is what puts them
                // there.
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

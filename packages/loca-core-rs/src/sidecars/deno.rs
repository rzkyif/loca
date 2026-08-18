//! The pinned `deno` runtime the tool workers and the npm-based extension
//! installer run under.

use std::path::PathBuf;

use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;

/// The release the bundled `deno` is pinned to, on every channel.
///
/// **This is the only copy.** Tool-worker permission prompts are parsed out of
/// this version's prompt wording, and D12's live probe is the tripwire that
/// catches a wording change before it ships. A version constant in two places
/// is how a tripwire like that stops tripping, so consumers import this rather
/// than declaring their own.
///
/// Bumping it is deliberate: re-run the live probe against the new release
/// first, and change `BINARY_MANIFEST.deno.pinnedTag` in
/// `@loca/shared/domain/model.ts` to match, since that is what the release
/// build resolves the download from.
pub const PINNED_DENO_TAG: &str = "v2.8.2";

/// The bundled `deno` binary's on-disk name. Not channel-suffixed: downloaded
/// binaries are isolated by the per-channel `bin` directory and keep their
/// original names.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "deno.exe"
    } else {
        "deno"
    }
}

/// Resolve the bundled `deno`, checking it is actually there.
///
/// `deno` is a downloadable requirement, so until the user installs it a spawn
/// would fail with a bare `NotFound` that says nothing about how to fix it.
/// This turns that into the `binary_not_found` the client already has a path
/// for.
pub fn resolve_worker_deno(paths: &Paths) -> Result<PathBuf> {
    let path = paths.bin_path(binary_name());
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(path),
        _ => Err(AppError::new(
            ErrorCode::BinaryNotFound,
            "the 'deno' worker runtime is not installed yet; download required files from Settings",
        )),
    }
}

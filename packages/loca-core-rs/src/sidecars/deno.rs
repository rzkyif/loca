//! The pinned `deno` runtime the tool workers and the npm-based extension
//! installer run under.

use std::path::PathBuf;

use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;

/// The release the bundled `deno` is pinned to, on every channel.
///
/// **The only copy.** Tool-worker permission prompts are parsed out of this
/// version's wording, and the live probe that catches a wording change stops
/// tripping if a second constant exists. Bumping means re-running that probe and
/// changing `BINARY_MANIFEST.deno.pinnedTag` in `@loca/shared` to match.
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
/// `deno` is a downloadable requirement, so until it is installed a spawn would
/// fail with a bare `NotFound`. This is the `binary_not_found` the client
/// already has a path for.
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

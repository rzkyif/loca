//! Downloading, verifying and installing the binaries this core needs.
//!
//! **One kind: `deno`** (plan section 4.2). The GPU variant matrix, the
//! tar/gzip path, the companion archives and the `bin/lib/<kind>/` convention
//! are all dropped: every one of them is reachable only from the sidecar kinds
//! D14 already absorbed, and keeping them would mean downloading gigabytes
//! nothing in this core can execute.
//!
//! `BinaryKind` stays 3-valued on the wire, because `@loca/shared` is the
//! contract and it is not narrowing until cutover. This core simply installs one
//! of the three. That difference from the Deno oracle at `/api/v1/binaries` is a
//! tracked difference, recorded like D-039, never a placeholder.

pub mod extract;
pub mod installer;
pub mod manifest;
pub mod routes;
pub mod upstream;

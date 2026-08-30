//! Downloading, verifying and installing the binaries this core needs.
//!
//! One installable kind (`deno`); `BinaryKind` stays 3-valued on the wire
//! because `@loca/shared` is the contract.

pub mod extract;
pub mod installer;
pub mod manifest;
pub mod routes;
pub mod upstream;

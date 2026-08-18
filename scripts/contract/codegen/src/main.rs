//! JSON Schema bundle -> Rust wire types, via typify.
//!
//! Usage: `loca-contract-codegen <bundle.json> <header-comment>`, printing the
//! module to stdout. The caller (`scripts/contract/generate.ts`) owns file
//! placement and the staleness check.
//!
//! typify emits one Rust type per `$defs` entry, which is why the emitters hand
//! it a bare `$defs` map keyed by the name each Rust type should carry.

use std::io::Write;

use typify::{TypeSpace, TypeSpaceSettings};

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(header)) = (args.next(), args.next()) else {
        eprintln!("usage: loca-contract-codegen <bundle.json> <header-comment>");
        return std::process::ExitCode::from(2);
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let schema: schemars::schema::RootSchema = match serde_json::from_str(&content) {
        Ok(schema) => schema,
        Err(e) => {
            eprintln!("{path} is not a schemars RootSchema: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut settings = TypeSpaceSettings::default();
    settings.with_struct_builder(false);
    let mut space = TypeSpace::new(&settings);
    if let Err(e) = space.add_root_schema(schema) {
        eprintln!("typify failed on {path}: {e}");
        return std::process::ExitCode::FAILURE;
    }

    let file: syn::File = match syn::parse2(space.to_stream()) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("typify emitted unparseable Rust: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{header}");
    let _ = write!(out, "{}", prettyplease::unparse(&file));
    std::process::ExitCode::SUCCESS
}

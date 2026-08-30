# loca-core-rs

Loca Core in Rust: one library that runs as the `loca-core` service on desktop
and links into the Tauri app on mobile (through `loca-core-ffi`), so the phone
embeds the real Core rather than a reimplementation of it.

## Why Rust, why one binary

Desktop runs local inference by downloading upstream `llama-server` and spawning
it. On mobile that is impossible: Android `targetSdk 36` enforces W^X so an app
cannot exec a binary written into app-private storage, and iOS forbids executing
downloaded code outright. The inference engine has to be compiled into the app.
Model weights are data and stay downloadable.

Compiling the engine into the app only helps if the app can also _drive_ it, so
the whole Core comes along: sessions, settings, memories, tools, speech. Only
`loca-core-updater` stays a separate binary, because a self-update swap has to
outlive the process it is replacing.

## Layout

```
src/
  lib.rs        module tree
  error.rs      AppError + the closed ErrorCode set and its status mapping
  log.rs        two scrubbed sinks: stderr for humans, core.log for machines
  config.rs     channel, ports, boot config, every hardcoded URL
  boot.rs       wires every module together for both frontends
  contract/     wire types generated from @loca/shared (deno task contract)
  bin/          the desktop service frontend
  <domain>/     one module per subsystem, each owning its own routes.rs
workers/        the TypeScript tool worker the core spawns under deno
data/           the committed signing trust root and espeak-ng data
```

## Build and test

```
deno task build:core-rs    # cargo build --release
deno task lint:core-rs     # cargo clippy --all-targets -D warnings
deno task test:core-rs     # cargo test
deno task fmt              # cargo fmt, repo-wide
```

Or `cargo` directly with `-p loca-core-rs`.

## Logging

Modules log through `tracing` with a `loca::<scope>` target:

```rust
tracing::info!(target: "loca::chat", "stream {id} started");
```

stderr carries everything from DEBUG up, formatted to match the client so the
multiplexed `deno task dev` console reads consistently. `core.log` carries INFO
and up in a stable machine shape, size-rotated at 10 MB with 5 backups.
Dependency crates are gated to WARN so their chatter stays out of the console,
and every line passes through `scrub_secrets` before it is written.

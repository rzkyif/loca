# Parity fixture generators

Two scripts that emit test vectors `packages/loca-core-rs` checks itself
against. They are run by hand and their output is committed.

| script               | writes                                        | rerun safe?                            |
| -------------------- | --------------------------------------------- | -------------------------------------- |
| `tls-pin-vectors.ts` | `src/transport/testdata/tls_pin_vectors.json` | yes, but pointless: new keys, new pins |
| `pake-vectors.ts`    | `src/auth/testdata/pake-vectors.json`         | yes, byte-identical (seeded PRNG)      |

Paths are relative to `packages/loca-core-rs/`. The other testdata files there
(`password-vectors.json`, `deno-sessions.json`, `relevance_vectors.json`) were
produced by the retired TypeScript core and are frozen.

TLS SPKI pin derivation and CPace are byte-exact parity surfaces: values already
pinned by paired clients, so a drift breaks pairing on upgrade. Prefer
regenerating over hand-editing.

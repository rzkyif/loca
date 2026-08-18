# Parity fixture generators

Five scripts that emit the test vectors `packages/loca-core-rs` checks itself
against. Each one drives the **real TypeScript implementation** and records what
it produced, so the Rust side is compared with the code that ships rather than
with a second reading of a spec.

They are run by hand, not by a task, and their output is committed. That is
deliberate: a fixture regenerated automatically on every gate run would agree
with whatever the code currently does, which is the one thing it must not do.

| script                 | writes                                         | rerun safe?                                            |
| ---------------------- | ---------------------------------------------- | ------------------------------------------------------ |
| `tls-pin-vectors.ts`   | `src/transport/testdata/tls_pin_vectors.json`  | yes, but pointless: new keys, new pins                 |
| `pake-vectors.ts`      | `src/auth/testdata/pake-vectors.json`          | yes, byte-identical (seeded PRNG)                      |
| `password-vectors.ts`  | `src/auth/testdata/password-vectors.json`      | yes, but not reproducible: the argon2id salt is CSPRNG |
| `session-fixture.ts`   | `src/sessions/testdata/deno-sessions.json`     | yes                                                    |
| `relevance-vectors.ts` | `src/memories/testdata/relevance_vectors.json` | yes, byte-identical                                    |

Paths in that table are relative to `packages/loca-core-rs/`. Every generator
takes its output path as an argument except `session-fixture.ts` and
`relevance-vectors.ts`, which write in place; each file's header carries the exact
command.

## Why three of these are load-bearing beyond testing

TLS SPKI pin derivation, CPace, and the admin-password hash are **byte-exact
parity surfaces**. They are not "the Rust answer should look like the TypeScript
answer"; they are values already written to disk or already pinned by a paired
client. A one-field drift in any of them breaks pairing on upgrade, and the
symptom is a connection that fails with no diagnosis beyond "wrong password" or a
certificate warning. That is why the fixtures are produced by the shipping
implementation and why `tls_pin_vectors.json` says, in the file, not to hand-edit
it.

## Regenerating

Prefer regenerating over editing, always. If a fixture and the Rust core
disagree, the question is which one is wrong, and hand-editing the fixture
answers it by fiat. `pake-vectors.ts` and `relevance-vectors.ts` reproduce their
files byte for byte, so a diff after running one of those is a real change in the
oracle, not noise.

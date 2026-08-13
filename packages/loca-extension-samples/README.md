# loca-extension-samples

The dev-only capability showcase for loca. It is never released to production;
it is only installed from the codebase in the dev environment so the showcase
tools stay out of a real user's tool list. It replaces the demo tools that used
to live in the built-in extension.

It doubles as the worked example for extension authors: every tool is a small,
single-purpose demo of exactly one thing an extension can do, so a new author
can read one file and copy the shape they need. The `loca.json` manifest shows
the full surface (per-tool permissions, `alwaysAvailable`, `platforms`,
`database`, and bundled `memories`).

## Tools

Each tool demonstrates one capability:

| Tool              | Capability                                                                 |
| ----------------- | -------------------------------------------------------------------------- |
| `sample_choice`   | `askUser` choice questions (text, single-select, multiselect + freeform)   |
| `sample_diff`     | `askUser` diff question (accept/reject a before/after)                     |
| `sample_files`    | `askUser` files picker (multiselect)                                       |
| `sample_image`    | `askUser` image question (custom actions)                                  |
| `sample_table`    | `askUser` table question, persisting accepted rows via `ctx.db`            |
| `sample_display`  | one-way `ctx.display` pushes: markdown, image, table, diff                 |
| `sample_database` | the private SQLite (`ctx.db`) the `database` flag provisions               |
| `sample_llm`      | a single completion via `ctx.llm` (gated by the `llm` permission)          |
| `sample_tts`      | speech synthesis via `ctx.tts` (gated by the `tts` permission)             |
| `sample_stt`      | transcription via `ctx.stt` (gated by the `stt` permission)                |
| `sample_schedule` | proposing a scheduled prompt via `ctx.schedulePrompt`                      |
| `sample_memory`   | reading and writing memories via `ctx.memories` (read + write permissions) |

`sample_choice` and `sample_display` set `alwaysAvailable: true`; `sample_files`
declares `platforms` to show OS gating. The bundled `memories/` (a knowledge
file and a skill) are the worked examples for shipping memories.

## Develop

```sh
deno task check   # type-check the entry module
deno task test    # run the co-located *.test.ts suites
```

Tests run against a hand-rolled mock `ToolContext` (scripted `askUser`,
recording `ctx.db`, in-memory `ctx.memories`), so they need no running Core.

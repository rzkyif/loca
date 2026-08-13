# loca-extension-builtin

Reference Loca extension, installed by default on fresh setups and doubling as
the baseline example for third-party extension authors. It bundles a spread of
everyday tools (web search and page reading, a calculator, date and time,
downloads, memories, and scheduled prompts) chosen to cover the common
`loca.json` cases.

For the full capability showcase (every askUser kind, every display kind, the
`llm` / `tts` / `stt` / `db` / `schedulePrompt` modules, a sample knowledge
memory and a sample skill), see the dev-only samples extension at
`packages/loca-extension-samples`.

The `loca.json` format is an open standard: any host that understands
`loca.json` can load extensions. The core discovers extensions by searching npm
for the `loca-extension` keyword. An extension is a provider: it can ship
tools, memories (knowledge and skills), or both. Each tool declares the OS-level
permissions it needs, and the user grants them per tool (see Permissions below).

| Tool                | Function        | What it does                                                                        |
| ------------------- | --------------- | ----------------------------------------------------------------------------------- |
| `web_search`        | webSearch       | Search DuckDuckGo and return the top results.                                       |
| `fetch_webpage`     | fetchWebpage    | Fetch a URL and return its readable text content.                                   |
| `calculator`        | calculator      | Evaluate a math expression (arithmetic, comparisons, functions like `sqrt`).        |
| `get_datetime`      | getDatetime     | Report the current local date, time, weekday, and UTC offset.                       |
| `download_url`      | download        | Download a file from an http(s) URL into the user's Downloads folder.               |
| `open_website`      | open            | Open a URL in the default browser (macOS `open`, Linux `xdg-open`, Win `rundll32`). |
| `open_app`          | openApp         | Launch one or more apps by name (macOS `open -a`, Win `start`, Linux `gtk-launch`). |
| `open_file`         | openFile        | Open a file, in a chosen app or the default one.                                    |
| `get_window_layout` | getWindowLayout | Read open windows' positions and sizes (macOS/Windows/Linux X11; best-effort).      |
| `set_window_layout` | setWindowLayout | Move and resize windows to a remembered layout (macOS/Windows/Linux X11).           |
| `list_memories`     | listMemories    | List the user's memories by title, kind, and short summary.                         |
| `read_memory`       | readMemory      | Read a memory's full content by title.                                              |
| `show_memory`       | showMemory      | Render a memory as markdown in the chat (one-way display).                          |
| `write_memory`      | writeMemory     | Create a memory, or replace an existing memory's content.                           |
| `edit_memory`       | editMemory      | Replace one exact text occurrence in a memory.                                      |
| `schedule_prompt`   | schedulePrompt  | Propose a scheduled prompt the user reviews and edits in chat before saving.        |

## Layout

```
.
├── loca.json     # Loca manifest: names, parameters, triggers, permissions
├── deno.json      # exports (entry) + imports (mime-types, expr-eval, @mozilla/readability, linkedom)
├── index.ts       # entry point: re-exports every tool function
└── src/
    ├── download.ts    # download_url
    ├── open.ts        # open_website
    ├── app.ts         # open_app, open_file
    ├── window.ts      # get_window_layout, set_window_layout
    ├── memories.ts    # list / read / show / write / edit_memory
    ├── schedule.ts    # schedule_prompt
    ├── datetime.ts    # get_datetime
    ├── calculator.ts  # calculator
    ├── web.ts         # shared size-capped fetch helper (not a tool)
    ├── webpage.ts     # fetch_webpage
    ├── search.ts      # web_search
    └── types.ts       # local copy of the ToolContext shape the worker injects
```

## Permissions

Each tool declares the minimum set of permissions it needs in `loca.json`
(network hosts, filesystem paths, executables, env vars, plus the Loca module
kinds `memories`, `llm`, `tts`, `stt`). The worker pool reads the user's
per-tool grants on spawn and gives the worker subprocess exactly the matching
`--allow-*` flags; module access (memories, ...) is brokered by the core rather
than handed to the worker. Specifically:

- `download_url` needs **read** and **write** on `$downloads` (read to pick a
  non-clobbering filename, write to save the file), plus **env** for
  `XDG_DOWNLOAD_DIR` / `HOME` / `USERPROFILE` (all optional; the tool resolves
  the folder from whichever is granted). The web-facing tools refuse loopback
  and private-range hosts and re-check every redirect hop.
- `fetch_webpage` needs **net** to any http(s) host; `web_search` needs **net**
  to `html.duckduckgo.com` only.
- `open_website` needs **run** access for `open`, `xdg-open`, and `rundll32`
  (one per host OS).
- `open_app` needs **run** for `open`, `cmd`, `gtk-launch`, and `xdg-open` (one
  set per host OS; only the matching one is ever spawned).
- `open_file` needs the same **run** binaries minus `gtk-launch`, plus **read**
  on `/` and `$home`: opening a remembered file means reading from anywhere on
  disk. `/` covers the whole tree on macOS/Linux; `$home` is the entry that
  reaches files on Windows, where a `/` grant does not span drive letters (so on
  Windows, files outside the home folder are not reached). The read defaults to
  ask, so each open prompts until the user grants it; granting it as always-allow
  shows a risky-permission warning. The handler also refuses any non-absolute
  path.
- `get_window_layout` / `set_window_layout` need **run** for `osascript`
  (macOS), `powershell` (Windows), and `wmctrl` (Linux X11). They declare
  `platforms: ["darwin", "windows", "linux_x11"]`, so they never appear on a
  Linux Wayland session (which has no way to position other apps' windows).
- `list_memories`, `read_memory`, and `show_memory` need **memories:read**;
  `write_memory` and `edit_memory` need **memories:write** (which also covers
  reads).
- `get_datetime`, `calculator`, and `schedule_prompt` need nothing.
  `schedule_prompt`'s in-chat confirmation form is its consent gate.

## Author guide

Use this extension as a template for your own. The shape every extension must
respect:

1. A `loca.json` at the package root, validated against the Loca `tools-v1`
   schema (`https://get.loca.rzkyif.com/schemas/loca-v1.json`). It needs a `name`, a
   user-facing `displayName`, and a `description`. `tools` is optional: an
   extension may ship only memories and declare no tools at all. Set
   `"database": true` at the top level if any tool uses `ctx.db`. A tool may
   declare `platforms` (`darwin` / `windows` / `linux` / `linux_x11` /
   `linux_wayland`) to limit which operating systems it runs on; Loca hides it
   everywhere on a host its list doesn't cover, so OS-specific tools never reach
   the model. The display server (`linux_x11` vs `linux_wayland`) is detected by
   Loca, so authors never inspect environment variables for it.
2. Dependencies declared in `deno.json` `imports` (use `npm:` / `jsr:`
   specifiers). The host installs them with `deno install` and never edits your
   `deno.json`. A deno-native extension like this built-in needs no
   `package.json`.
3. Named async exports matching each tool's `"function"` field in `loca.json`.
   They're called as `fn(args, ctx)`.

The `ctx` the worker injects gives a tool more than `setProgress` / `log`:
`ctx.askUser` drives the in-chat forms (plain text, single-select, multiselect,
and the richer `diff` / `files` / `image` / `table` kinds), `ctx.display.*`
pushes one-way markdown / image / table / diff bubbles, `ctx.memories` reads and
writes the user's memories, `ctx.schedulePrompt` proposes a scheduled prompt,
and `ctx.db` reaches the extension's private SQLite database (only when
`"database": true` is declared). `src/types.ts` is the full shape. The samples
extension (`packages/loca-extension-samples`) is the worked example of every
one of these capabilities.

### Shipping memories

Beyond tools, an extension can bundle memories the user gets read-only on
install. Declare them in a top-level `memories` array in `loca.json`, each
entry naming its `kind` and `path` (relative to the extension root). The samples
extension (`packages/loca-extension-samples`) ships one of each as the worked
example:

```jsonc
"memories": [
  { "kind": "knowledge", "path": "memories/loca-overview.md" },
  { "kind": "skill", "path": "memories/web-research" }
]
```

The path must stay inside the extension (no absolute paths, no `..` segments);
the manifest schema and the store both reject anything that would escape the
install dir.

The two kinds differ in shape and in how the agent uses them:

- **Knowledge** is reference data. The `path` points at a single `<slug>.md`
  file, and its content is injected as data the agent must not treat as
  instructions.
- **Skill** is procedural instructions the agent follows when relevant. The
  `path` points at a `<slug>/` folder holding a `SKILL.md` plus any optional
  bundled reference files. `SKILL.md` may open with simple frontmatter giving a
  `description` (used for relevance and the listing) and `suggested-tools`. The
  folder layout matches Anthropic's Agent Skills format, so a community
  `SKILL.md` drops in unchanged: its `name` key is tolerated and ignored (the
  slug names the skill here) and its `description` seeds the summary. A skill is
  read on demand; the agent reaches its bundled files through the
  `read_skill_file` tool.

Memories an extension ships are read-only to the user, alongside the knowledge
and skills they author themselves.

To distribute via npm, add a `package.json` with a name starting
`loca-extension-` (convention, not enforced) and
`"keywords": ["loca-extension"]` so users can find it in the in-app marketplace
and `Download` it from Settings → Extensions. npm packages declare deps in
`package.json`; the host installs deps from either `package.json` or
`deno.json`.

## Testing your extension

This package ships co-located tests as the worked example for testing a
extension. They mock the `ToolContext` directly (no real worker bridge) so they
run wherever `deno test` does:

- `src/calculator.test.ts` / `src/datetime.test.ts`: pure-logic tools, asserted
  without touching ctx beyond the stub.
- `src/organize.test.ts`: scripted `askUser` answers run against a tempdir.
- `src/open.test.ts`: URL validation; skips the actual subprocess spawn (which
  would open a real browser).
- `src/app.test.ts` / `src/window.test.ts`: assert the pure per-OS command
  builders and output parsing across every platform, plus handler validation,
  without spawning (which would launch real apps or move windows).

The pattern, reduced to the minimum (stub the ctx fields your tool actually
touches; the rest reject so an accidental call is loud):

```ts
import { assertEquals } from "jsr:@std/assert@^1";
import { myTool } from "./myTool.ts";
import type { ToolContext } from "./types.ts";

function mockCtx(): ToolContext & { progress: number[] } {
  const progress: number[] = [];
  return {
    progress,
    setProgress: (p) => progress.push(p),
    askUser: () => Promise.resolve([]),
    log() {},
    display: { markdown() {}, image() {}, table() {}, diff() {} },
    memories: {
      list: () => Promise.resolve([]),
      get: () => Promise.reject(new Error("not stubbed")),
      write: () => Promise.reject(new Error("not stubbed")),
      edit: () => Promise.reject(new Error("not stubbed")),
    },
    db: {
      query: () => Promise.reject(new Error("not stubbed")),
      execute: () => Promise.reject(new Error("not stubbed")),
    },
    llm: { complete: () => Promise.reject(new Error("not stubbed")) },
    tts: { speak: () => Promise.reject(new Error("not stubbed")) },
    stt: { transcribe: () => Promise.reject(new Error("not stubbed")) },
    schedulePrompt: () => Promise.reject(new Error("not stubbed")),
    signal: new AbortController().signal,
    getChatContext: () => ({ userMessage: "", sessionId: null }),
  };
}

Deno.test("myTool: progress runs 0 -> 1", async () => {
  const ctx = mockCtx();
  await myTool({ input: "x" }, ctx);
  assertEquals(ctx.progress.at(-1), 1);
});
```

What to assert:

- **Progress order**: `setProgress` is called in non-decreasing order and ends
  at 1.
- **Abort honored**: when `ctx.signal.aborted` flips mid-call, the tool exits
  with a thrown error or returns early.
- **Permission boundary**: the tool only touches the resources its `loca.json`
  declares. If it tries to fetch a host that's not in `net`, the worker pool
  blocks it; your test can assert the tool propagates the rejection cleanly.

Tests are co-located as `*.test.ts` with no tier suffix; `*.tmp.test.ts` is the
gitignored scratch variant for quick experimentation.

## Run, build, test

This package's tests run with the repo suite: `deno task test`, or
`deno task test:extension` for just this package (`deno task check:extension`
for its type-check). `deno task release` (or `release:stable`) publishes the
extension manifest + tarball when its content changed, after a version bump in
this package's `deno.json`; see
[`../loca-website/README.md`](../loca-website/README.md) for the release
pipeline.

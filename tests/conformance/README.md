# Conformance suite

A language-agnostic HTTP/WS suite that drives a running Core over the wire and
asserts its observable behaviour. It runs against the **Deno core** (the oracle,
which proves the suite is right) and the **Rust core** (the acceptance gate,
which proves the port is right).

A work package is not done because its own tests pass. It is done when its slice
of this suite passes.

## Run it

```sh
deno task conformance                      # every case, against the Deno core
deno task conformance --core=rust          # against the Rust core
deno task conformance --core=diff          # BOTH, compared to each other
deno task conformance --filter=D2          # one package's slice
deno task conformance --filter=f6.ws       # one case, or a prefix of several
deno task conformance --list               # enumerate without running
deno task conformance --json=out.json      # machine-readable results
```

## Differential mode

`--core=diff` boots both cores, runs each case against both, and compares them
to the expectation **and to each other**. It is the direct answer to "does the
Rust do what the Deno does"; the expectations only answer it for what somebody
thought to write down.

Four verdicts:

| verdict  | meaning                                                          |
| -------- | ---------------------------------------------------------------- |
| `agree`  | both satisfied the expectation and produced the same thing       |
| `DIFFER` | both passed and still disagree: the expectation missed something |
| `FAIL`   | one core failed its expectation. Usually a port bug              |
| `BOTH`   | both failed. Usually the CASE is wrong                           |

`DIFFER` is the interesting one. The comparison masks every position where the
case used a placeholder or a `${captured}` reference, so declared-unstable
fields never appear. **Everything else is compared exactly, including fields no
case mentions** - that is the point. When a `DIFFER` names a legitimately
unstable field, the fix is to assert it with a placeholder, which makes the run
quieter and the case stronger at the same time.

Cutover cases (`handoff: "swap"`) are the one exception: they run **once**, in
every mode, and are reported in their own block rather than compared. See
[Cutover steps](#cutover-steps).

Error `message` text is reported as a `prose` line but does **not** fail. The
`code` is the contract and is compared exactly; the message has no machine
consumer, and forcing two implementations to emit byte-identical English would
be busywork. Pin the wording in the expectation if a case genuinely needs it.

## Real-install parity

```sh
deno task conformance --core=diff --real-home=~/.loca/stable/core --yes-copy
```

Boots both cores on a **copy** of a populated real install and checks that it
still works: both boot, both derive the same SPKI pin from the same sealed key,
both report the same health, and both leave an identical tree on disk.

Nothing else in the port proves an existing user's install survives the cutover,
and the on-disk tree is one of the byte-exact parity surfaces. It complements
F4's Deno-written `core.sqlite` fixture, which proves migration parity from the
inside at the schema; this proves the observable half from the wire.

The original is **copied and never written to**. Pointing at a live `~/.loca`
still requires `--yes-copy`, because a suite that can damage a real install is
one nobody will run, and losing that trust once is enough.

Useful extras: `--keep-home` leaves each core's tempdir behind for a post-mortem,
and `LOCA_CONFORMANCE_CORE_LOGS=1` streams the core's stderr as it runs.

Nothing needs building first for the Deno target. `--core=rust` needs
`target/{release,debug}/loca-core`, and **fails loudly if it is absent** rather
than skipping: a run that passes because nothing ran is the exact failure this
suite exists to prevent. So does any run that selects a cutover case, `--core=deno`
included. The run header always prints which binary was used.

The suite is deliberately **not** part of `deno task test`. It spawns real
processes and is slow by that standard; `check`, `lint`, and `fmt` do cover it.

## Add a case

Each package owns its own file: `cases/<id>-<area>.ts`, e.g. `cases/d2-sessions.ts`.
Copy a neighbour and change the fields. You do not need to read the runner.

```ts
import type { Case } from "../runner/types.ts";

export const cases: Case[] = [
  {
    id: "d2.sessions.create-patch-get-delete",
    package: "D2",
    routes: ["POST /api/v1/sessions", "PATCH /api/v1/sessions/:id", "DELETE /api/v1/sessions/:id"],
    auth: "paired",
    steps: [
      {
        request: { method: "POST", path: "/api/v1/sessions", body: { title: "first" } },
        expect: {
          status: 200,
          body: { id: "<id>", title: "first", createdAtMs: "<ms>", messageCount: 0 },
        },
        capture: { sid: "$.id" },
      },
      {
        request: { method: "PATCH", path: "/api/v1/sessions/${sid}", body: { title: "second" } },
        expect: { status: 200, bodyContains: { title: "second" } },
      },
      {
        request: { method: "DELETE", path: "/api/v1/sessions/${sid}" },
        expect: { status: 204 },
      },
    ],
  },
];
```

Then add one line to `cases/index.ts` importing it. Ids must be unique; the
registry throws at load if they are not.

**Write cases against the contract, not against the output.** The fastest way to
build one is to run it, read the mismatch, and paste in what came back. Resist
that for anything load-bearing: check `@loca/shared` and the route handler for
what the field is _supposed_ to be. A case that encodes a bug is a case that
locks the bug in.

Every foundation case here was written this way, and seven of the first eighteen
failed on their first run. That is the result you want. A case written from the
contract disagrees with reality exactly where somebody's mental model is wrong,
and each disagreement is either a bug in the case or a fact nobody knew: that is
how the bare array from `GET /pairing/clients` and the three distinct auth error
codes surfaced. A case written from observed output agrees with reality by
construction and proves nothing.

Assert the error `code`, not just the status. The codes are a published contract
(`ERROR_CODES` in `@loca/shared`), so collapsing several into one is an
observable wire change whether or not anything reads them today, and three of
the auth codes share a single 401 status: a status-only assertion cannot tell
them apart.

### Matchers

Any expected string of the form `<name>` matches instead of comparing. The set
is closed; `<<literal>>` escapes to a literal `<literal>`.

| placeholder   | matches                                                    |
| ------------- | ---------------------------------------------------------- |
| `<id>`        | non-empty id-shaped string                                 |
| `<uuid>`      | a UUID                                                     |
| `<ms>`        | epoch millis between the run's start and a minute from now |
| `<ms-future>` | epoch millis in the future, within a year                  |
| `<duration>`  | non-negative integer                                       |
| `<port>`      | a port number                                              |
| `<pin>`       | base64 of 32 bytes                                         |
| `<version>`   | a semver string                                            |
| `<path>`      | an absolute path                                           |
| `<sha256>`    | 64 lowercase hex chars                                     |
| `<string>`    | any string                                                 |
| `<number>`    | any number                                                 |
| `<bool>`      | any boolean                                                |
| `<any>`       | present, any type                                          |
| `<absent>`    | the key must NOT be present                                |

`<ms>` and `<ms-future>` are bounded on purpose. "Any integer" would pass on a
core returning `0` or a seconds-based timestamp, which is the easy mistake in
Rust where `SystemTime` and `chrono` both make seconds the path of least
resistance.

### Exact by default

`expect.body` is an **exact** match, key set included: an unexpected key is a
failure. That is the point. A port drops fields silently, and loose matching is
how that ships.

The deliberate opt-outs, in ascending order of how much they give up:

- `bodyContains` instead of `body`, when other contents are not your business
  (list endpoints whose contents depend on which cases shared the boot).
- `unordered: { "$.memories": "id" }` sorts before comparing, for arrays whose
  order is genuinely not contractual. If order IS contractual, do not use it.
- `ignorePaths: ["$.core.uptimeMs"]` drops a subtree. Leave a comment saying why.

Two values are folded out of the actual body before comparison, so you can
assert around them: the core's port becomes `<port>` and its home directory
becomes `<home>`. Write `"<home>/sessions"` and it works.

### Auth

`auth` on the case, optionally overridden per step:

| value          | what it sends                                            |
| -------------- | -------------------------------------------------------- |
| `none`         | nothing                                                  |
| `admin`        | `X-Admin-Token`                                          |
| `paired`       | the scenario's paired client (the usual choice)          |
| `paired:other` | a second paired client, for cross-client isolation cases |

Pairing is real: the harness mints a code with the admin token it seeded, then
runs the full CPace handshake using the same implementation the Client uses,
folding the TLS pin it derived from the core's own certificate. Nothing is
stubbed and no check is bypassed.

### Broadcasts triggered by an HTTP request

Most of what this core does on the WS is broadcast-on-mutation: `PATCH` the
settings and every client gets `settings.updated`; the same shape carries
`downloads.snapshot`, `requirements.snapshot`, and `session.updated`. A `ws`
step cannot see any of it, because the trigger is a different request on a
different connection.

Declare `watch` on the case and assert `frames` on the step that triggers them:

```ts
{
  id: "d1.settings.patch-broadcasts",
  package: "D1",
  routes: ["PATCH /api/v1/settings"],
  auth: "paired",
  watch: ["settings.updated"],
  steps: [
    {
      request: { method: "PATCH", path: "/api/v1/settings", body: { "llm.idleUnloadSeconds": 123 } },
      expect: {
        status: 200,
        frames: [{ kind: "settings.updated", values: { "llm.idleUnloadSeconds": 123 }, deleted: [] }],
      },
    },
  ],
}
```

The runner opens one authenticated connection **before the first step** and
buffers the watched kinds continuously. That is what keeps this from being a
race: a broadcast that lands while the request is still in flight is already
buffered when the assertion runs, so the usual wait is zero. The bound
(`frameTimeoutMs`, default 5s) only covers the tail.

Two failures are reported differently on purpose, because they have different
causes: `no settings.updated frame arrived within 5000ms` means the broadcast
never happened, while an ordinary mismatch list means it happened and was wrong.

A wait longer than a second still passes but prints a `note`, on success as well
as failure. A case that is quietly racing should be visible while it is still
green, not after it starts failing in CI.

`noFrames: ["settings.updated"]` asserts the opposite, and costs the full bound
by construction: proving a negative means waiting it out. Keep its
`frameTimeoutMs` short (it defaults to 1s).

### Scripting the model

Every seeded core points its LLM at the loopback mock, which by default **echoes
the last message's content back**, so an unscripted chat turn is deterministic:
the assistant says what the user said. `llm` on the case changes that, and is
reinstalled before every case, so a script cannot leak into the neighbour it
shares a boot with.

| kind         | answers with                                    |
| ------------ | ----------------------------------------------- |
| `echo`       | the last message's content (the default)        |
| `text`       | a fixed assistant message                       |
| `error`      | an HTTP failure, for the provider-error mapping |
| `tool_calls` | tool calls and `finish_reason: "tool_calls"`    |
| `sequence`   | one entry per upstream request, in order        |

`sequence` is what makes a multi-hop turn expressible, because one chat turn can
be several upstream requests:

```ts
{
  id: "d4.chat.a-tool-result-comes-back-to-the-model",
  package: "D4",
  routes: ["POST /api/v1/sessions/:id/messages"],
  auth: "paired",
  isolate: true,
  llm: {
    kind: "sequence",
    scripts: [
      { kind: "tool_calls", calls: [{ id: "call_1", name: "probe_tool", arguments: '{"q":"weather"}' }] },
      // With a tool result as the last message, `echo` answers with the TOOL's
      // own output. That text can only appear if the core really dispatched the
      // call and fed the result back in the right place, which is what makes
      // the hop assertable end to end rather than by proxy.
      { kind: "echo" },
    ],
  },
  steps: [/* ... a chat.start ws step ... */],
}
```

Three things this mock will not do quietly, on purpose:

- **It never falls back to a default answer.** A `sequence` asked for a turn it
  was not scripted for answers HTTP 500 with a message naming the overrun, which
  the core surfaces as `chat.error` / `provider_error`. A mock that echoed
  instead would let every tool case pass without dispatching anything, which is
  D-050 case 1 in a new place.
- **A malformed script is rejected when the case installs it**, not when the
  turn runs, so it reads as a broken case rather than as a port defect.
- **Tool arguments are streamed in two pieces.** Real providers fragment
  `tool_calls[].function.arguments` across chunks and both cores accumulate them
  keyed on `index`; sending them whole would leave that accumulation untested.

`loop: true` repeats the last entry forever, which is how the hop limit is
reached: a script of one `tool_calls` entry drives exactly `tools.maxHops`
upstream requests and ends the turn with `chat.done`, `reason: "hop_limit"`.

### File uploads

`request.form` sends multipart/form-data. Parts are either a field (`value`) or
a file (`file`); the boundary is generated, so do not set `content-type`.

```ts
{
  request: {
    method: "POST",
    path: "/api/v1/sessions/${sid}/attachments",
    form: [
      { name: "messageId", value: "msg-1" },
      { name: "file", file: { filename: "hello.txt", contentType: "text/plain", content: "hello" } },
    ],
  },
  expect: { status: 200, bodyContains: { id: "<id>" } },
}
```

`file.content` is text unless `encoding: "base64"`. For the download half,
`expect.bodyBytes` and `expect.bodySha256` assert on the raw response bytes
(`res.text()` would mangle binary), which is what makes an upload/download round
trip a real content-equality check rather than a status check.

### Parity steps

A step with `handoff: "restart"` stops the core and brings it back on the same
home before evaluating its expectation. Everything is expected to carry over:
the bearer, the client rows, the persisted tree, and the TLS pin.

```ts
{
  handoff: "restart",
  request: { method: "GET", path: "/api/v1/pairing/clients" },
  expect: { status: 200, pinUnchanged: true, bodyContains: [{ id: "${clientId}" }] },
}
```

`pinUnchanged` re-derives the SPKI pin and requires it byte-identical. The cert
is regenerated every boot but the key is sealed in the secrets vault, so a moved
pin means the key was not persisted or the derivation drifted, and either one
re-pairs every paired client with no obvious diagnosis.

### Cutover steps

`handoff: "swap"` brings the **other** core up on the same home instead. The
port is a big-bang cutover with no dual-shipping period, so the property the
whole migration rests on is that an install the Deno core wrote still works when
the Rust core boots on it, and this is the only thing that tests it.

```ts
{
  handoff: "swap",
  request: { method: "GET", path: "/api/v1/sessions/${sid}" },
  expect: { status: 200, pinUnchanged: true, body: { /* what Deno wrote */ } },
}
```

Four things about a cutover case are not like an ordinary one:

- **It is not parameterised by `--core`.** It exercises both binaries by
  construction, so it runs the same way in every mode and needs **both** present.
  A missing Rust binary fails the run even under `--core=deno`, rather than
  skipping.
- **It always starts on Deno.** The direction is not symmetric: a home seeded for
  the Rust core carries one helper binary and the Deno core refuses to boot
  without four, so only deno to rust is bootable, and it is also the only
  direction the cutover claim is about.
- **It requires `isolate: true`**, checked at load. It replaces the core
  underneath itself, and a neighbour sharing that boot would find a different
  core halfway through its own steps.
- **In diff mode it is run once, not compared.** Replaying a swap case against
  both cores would produce two identical transcripts and an "agree" that means
  nothing, so cutover cases are reported in their own block with their own count
  and never fold into the `N agree` line.

Write one for your own package's persistence. Anything a user would lose if the
port could not read it back belongs here: a case that only restarts proves the
core survives its own reboot, which is a different and much weaker claim.

**Read this before trusting a green cutover case.** Every assertion one makes is
also satisfied by the first core simply restarting: "the pin has not moved, the
bearer still works, the session is still there" describes both. The only thing
that separates a real hand-off from one that quietly did nothing is which binary
is answering, so the harness asks the OS which executable is serving after every
swap and fails loudly if it is the wrong one (`assertServedBy` in
`harness/core.ts`). That check is what your assertions rest on. It is watched
failing, not assumed: making `swap()` re-spawn the Deno core turns every cutover
case red, and the first version of the check, which read the expected binary off
the spawn instead of off the target asked for, left them all green.

### Scenarios and isolation

Cases sharing a `scenario` share one core boot, in declaration order, which is
why they must be self-contained: create what you read. Anything that mutates
global state (rotating a token, setting the admin password, revoking a client)
sets `isolate: true` and pays for its own boot.

### WebSocket steps

A step with `expect.ws` runs the upgrade instead of an HTTP request:

```ts
{ expect: { ws: { ticket: "valid", send: [{ kind: "ping" }], expect: [{ kind: "pong" }] } } }
```

`ticket` may be `valid`, `none`, `invalid`, or `spent`; `status` asserts the
upgrade's HTTP status (101 unless stated). The suite performs the handshake over
a raw TLS socket precisely so that status is assertable: the browser WebSocket
API hides it, which is why the headless E2E lane documents the unauthorized
state as uncoverable there.

Frames in `expect` are matched as an **ordered subsequence filtered to the kinds
you named**, not a full transcript. The core interleaves heartbeats and snapshot
broadcasts whose timing is not deterministic, so a full-transcript assertion
would be flaky by construction.

## How it is put together

```
main.ts                 CLI: flags, scenario grouping, the run loop
runner/types.ts         the case format. The whole vocabulary a case needs
runner/match.ts         placeholders, normalisation, exact comparison
runner/execute.ts       one case against one core: HTTP, SSE, WS steps
runner/watch.ts         buffered broadcast observation and its bounded waits
runner/scope.ts         which routes each TARGET serves
runner/report.ts        console output and the per-package route coverage
harness/core.ts         spawn, address, swap, tear down a core. One interface, 3 targets
harness/home.ts         throwaway LOCA_CORE_HOME seeding, target-aware
harness/tls.ts          SPKI pin derivation from the core's own tls-cert.pem
harness/pair.ts         the real CPace pairing, via @loca/shared
harness/ws.ts           raw WebSocket upgrade + frame codec
harness/upstream.ts     loopback mock for LLM/STT/TTS and artifact downloads
cases/                  the cases. One file per package
```

**No outbound network.** Every upstream a core could reach (LLM, STT, TTS, model
weights, release manifests, HuggingFace) is redirected to a loopback mock. That
is not just for speed: the repo's absolute rule is no non-consented network, and
a core that reached the real internet here would fail rather than quietly pass.

**TLS is real.** The runner accepts loopback self-signed certificates
(`--unsafely-ignore-certificate-errors=127.0.0.1`, the same flag four root dev
tasks use), but the core still serves its real cert with its real key, and the
pin is derived independently from that certificate and folded into the handshake
for real. Certificate identity is checked; only chain validation is relaxed.

## Design notes

The full design, the target-keyed scope table, the differential mode, the route
inventory grouped by package, and the phasing are in
[docs/rust-port/packages/F8/plan.md](../../docs/rust-port/packages/F8/plan.md).

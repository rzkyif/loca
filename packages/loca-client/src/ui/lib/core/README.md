# Core API layer

The client's only doorway to a core. Everything above imports from the barrel
([`index.ts`](index.ts)): `import { cores, ApiError } from "$lib/core"`. Nothing
else in the UI talks HTTP or WS to a core directly.

## Transport ([`client.ts`](client.ts))

`CoreClient` is the single HTTP+WS client for the currently-selected core. All
HTTP requests are bearer-authed (except the public health check and
pairing-claim) and every connection is pinned to the cert SPKI captured at
pairing (`CoreEndpoint.tlsPin`). One multiplexed WebSocket per paired core:
every frame carries its own discriminator (`streamId`, `callId`, or `jobId`),
and subscribers register callbacks the client dispatches incoming frames to.
Reconnects use exponential backoff with a connect watchdog (tighter timeout for
a loopback core than a remote one), and a coarse `ConnectionState` (`connecting`
/ `connected` / `disconnected`) feeds UI banners.

## Paired cores ([`cores.ts`](cores.ts))

`cores` is the paired-cores registry: the `{id, name, baseUrl, tlsPin}` entries
plus the current-core pointer live in their own `cores.json` (this module is the
file's single owner; settings live separately), the bearer tokens in the OS
keychain. It owns the currently-selected core and rebuilds the `CoreClient` plus
all per-domain APIs on switch. WS and connection-state listeners are persistent:
they survive core switches because `select()` re-binds every registered listener
onto the freshly-built client. The settings store hooks these notifications to
load the selected core's settings baseline and follow `settings.updated`
broadcasts, so selection, pairing, and reconnects all converge without
per-call-site fetches.

The client is connected to one core at a time. The CoreBar
([`components/chat/CoreBar.svelte`](../../components/chat/CoreBar.svelte)) shows
which core that is, its merged status, and a quick switcher; full management
(pair / unpair / rename) stays in Settings. The displayed status merges the
backend `CoreStatus` (Starting Up / Downloading / Idle / Busy / Updating /
Error, carried on
`core.status` frames and surfaced by the `coreStatusState` store) with the
client transport states (Connecting / Reconnecting / Disconnected / Re-pair
needed, from `connectionState`); transport wins whenever the socket is not
connected, since a backend status can never ride a dropped wire.

`core.status` is the SINGLE status frame: alongside the aggregate it carries a
per-subsystem breakdown (`subsystems`) and, while busy, queue counts (`queues`).
There is no separate per-sidecar frame. Every sidecar failure folds into the one
`error` status; clicking the CoreBar status pill expands a card listing the
broken subsystems and their errors (or, while busy, what is working and waiting).
The per-sidecar facade `serversState` (read by the Services field and the chat
input) is rebuilt from `subsystems`, so it keeps working without a per-sidecar
frame.

## Connection resume and tool-pause survival

Swapping cores closes the current socket but the turn keeps running on the core
you left. On reconnect (or reopening the session), the client sends a
`chat.subscribe` frame from [`sessions.svelte`](../../state/sessions.svelte.ts)'
`load()`; the core re-emits the in-flight messages so far (a catch-up snapshot)
and any open tool prompt, then live deltas resume. The client adopts the
server's stream via the existing foreign-stream path in
[`streaming.svelte`](../../state/streaming.svelte.ts), which on disconnect now
`softReset`s the local turn (no abandon, no interrupt) instead of tearing it
down. A tool call awaiting user input therefore survives a client
disconnect / reconnect / restart transparently to the tool (the worker stays
paused on the core); only a core restart drops it.

## Pairing ([`pairing.ts`](pairing.ts))

Pairing is a CPace PAKE keyed by the 6-digit code. The client first
TOFU-captures the core's cert pin, then folds that pin into both the CPace
channel identifier (the derived key is cert-bound) and the key confirmation, so
a MITM re-terminating TLS is detected twice over. Success returns the bearer
token and the pin to store. `PairingApi` (instance) covers operations on an
already-paired core (mint a code, list/revoke/rotate clients) through the pinned
`CoreClient`; the free functions (`probeCore`, `mintCodeWithAdminToken`,
`pairWithCode`) cover the unpaired flow where no pin exists yet.

## Per-domain APIs

| Module                               | Covers                                                                             |
| ------------------------------------ | ---------------------------------------------------------------------------------- |
| [`sessions.ts`](sessions.ts)         | Session CRUD, message append/patch.                                                |
| [`chat.ts`](chat.ts)                 | Stream control: start, interrupt, subscribe (resume), tool response.               |
| [`models.ts`](models.ts)             | Model catalog, downloads, presets.                                                 |
| [`binaries.ts`](binaries.ts)         | Binary manifest, check/install/update.                                             |
| [`requirements.ts`](requirements.ts) | Required files, download missing.                                                  |
| [`extensions.ts`](extensions.ts)     | Extension search/download/grants, filtering.                                       |
| [`settings.ts`](settings.ts)         | Sparse core settings load/patch (live-synced via the `settings.updated` WS frame). |
| [`sidecars.ts`](sidecars.ts)         | Sidecar snapshots plus CPU/RSS sampling.                                           |
| [`llm.ts`](llm.ts)                   | Autocorrect, transcription merge.                                                  |
| [`stt.ts`](stt.ts)                   | Speech-to-Text transcription and status.                                           |
| [`tts.ts`](tts.ts)                   | TTS load/unload, synthesize, voices.                                               |
| [`update.ts`](update.ts)             | Core self-update check and apply.                                                  |
| [`storage.ts`](storage.ts)           | Core on-disk storage tree, clear/delete.                                           |

## Client-side storage ([`client-storage.ts`](client-storage.ts))

Mirrors the core `StorageApi` shape (get / deletePaths / clearCategory) so the
StorageField component can drive either side. The tree comes from a Tauri
command; deletes go through the platform layer. The settings "clear" only
empties `settings.json`: the paired-cores registry (`cores.json`) and snippets
(`snippets/`) are separate files, so a settings reset can never cost a pairing
or a snippet. The active `client.log` is truncated (it is held open by the
logger) while rotated backups are removed outright.

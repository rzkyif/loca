# Security Policy

Loca is a local-first desktop app: a Rust service (`loca-core`) that owns all
state and compute, paired with a deliberately thin Tauri + Svelte client
(`loca-client`), plus helper binaries, a bundled extension, and a Cloudflare/R2
distribution site. The interesting attack surface is binary and model
provenance, secret handling, the local HTTPS/WSS API, multi-client pairing auth,
and the third-party tool sandbox. This document describes the security posture,
the trust boundaries we rely on, and the limitations we knowingly accept.

## Reporting a vulnerability

Please **do not** open a public issue for security-sensitive reports. Instead,
email the maintainer at [rizky@rzkyif.com](mailto:rizky@rzkyif.com).
Include:

- A description of the issue and the impact you believe it has.
- Steps to reproduce (or a proof-of-concept).
- The commit hash or release version you tested against.
- Any relevant platform details (OS, architecture).

You should expect an initial acknowledgement within seven days. We'll keep you
updated as a fix is developed, and credit you in the release notes if you'd
like.

## Threat model and trust boundaries

**Trusted.** We assume the following are not the attacker:

- The local user account, its filesystem, and the OS keychain.
- Our Ed25519 release signing key (and the Tauri/minisign client-update key).
- npm, GitHub, and the R2 origin reached over TLS.
- **Paired clients.** Pairing is the trust grant. A paired client is treated as
  an administrator of the core it paired with: it can read and write sessions,
  change any setting, run tools, and trigger updates. An action that requires
  pairing is therefore in-policy, not a vulnerability. Onboarding a _new_ device
  (minting a pairing code) or removing another paired device is a privileged
  action gated by either the admin token (device access) or the admin password
  (set at install); ordinary use never needs either again.

**Defended against.** We do protect against:

1. **Unpaired network attackers.** Anyone who can reach the core but has not
   paired, especially when the core is bound beyond loopback.
2. **MITM on the local network.** All client-core traffic is TLS (HTTPS/WSS).
   Clients pin the core's key at pair time, and the pairing handshake itself is
   channel-bound to the certificate, so an active MITM cannot intercept traffic
   or sneak into the pairing.
3. **Honest mistakes** by trusted users (footguns, accidental destructive
   actions, secrets ending up where they need not).
4. **Third-party code and supply chain.** Malicious extension npm packages,
   tampered models or helper binaries, a compromised CDN, and prompt-injected
   model output that drives tool calls.

## Architecture trust map

**Transport security (TLS).** The core serves its API exclusively over
HTTPS/WSS; there is no plaintext fallback. It uses a self-signed certificate (no
public CA exists for `127.0.0.1` or LAN IPs): an ECDSA P-256 keypair is
generated once and sealed in the secrets vault, and the certificate is
regenerated from it on every boot. Clients pin the key, not the certificate: the
pin is `base64(SHA-256(SubjectPublicKeyInfo))` (the RFC 7469 convention), so it
stays stable across reboots and `server.bindHost` changes and no re-pair is
needed when the core's address changes. The Tauri webview cannot pin a
self-signed certificate, so all core traffic terminates in the Rust host: a
custom rustls verifier enforces the stored pin (constant-time comparison) for
both HTTP (reqwest) and WebSocket (tungstenite) connections, and a mismatch
fails the connection.

**Pairing and authentication.** Pairing is a PAKE (CPace) keyed by a short
6-digit code minted by the admin-token-guarded `POST /pairing/codes`. The
unpaired client first captures the core's certificate pin over an
unauthenticated TLS connection (trust-on-first-use), then runs the PAKE with
that pin folded into both the key derivation and the key-confirmation tags. The
core folds in its own pin, so a MITM that terminates TLS with a different
certificate diverges the keys and fails the confirmation even if it relays the
PAKE messages verbatim; the wrong code fails the same way. The confirmation is
mutual: the client verifies the core's tag before accepting the pairing, so a
fake core cannot complete it either. The handshake is hardened against an
unpaired attacker: the per-IP rate limiter keys on the real socket peer address
(threaded from `Deno.serve`, not the spoofable `X-Forwarded-For`), and every
failed confirmation burns the outstanding code's attempt budget and poisons it
after a small number of tries. A successful pairing mints a 32-byte bearer
token, stored as a sha256 hash; the admin token is 128-bit, mode `0600`, and
compared in constant time. All HTTP routes require the bearer token except
`/health` and the pairing handshake; the WebSocket authenticates the bearer at
upgrade. Revoking or rotating a client force-closes its live WebSockets so a
removed device loses access immediately rather than at its next reconnect.

**Minting a code remotely (the admin password).** Reading the admin token off
disk needs access to the core's machine, which a remote operator does not have.
So `POST /pairing/codes` and cross-client revoke accept a second authorization:
a valid bearer (an already-paired, trusted device) **plus** an admin password
set at install. The password is never a standalone gate on the open network. It
is a second factor behind an existing pairing, it is stored only as an argon2id
hash (`.admin-password`, mode `0600`, per-install salt), and verification is
rate-limited per client id, per peer IP, and globally with the cheap counter
checked _before_ the hash runs so wrong guesses are throttled and argon2id
cannot be turned into a CPU DoS. Setting or replacing the password itself
requires the admin token (device access), so a network peer on a `0.0.0.0`-bound
core cannot overwrite it. The password travels only in the request body over the
pinned TLS channel and is redacted from logs.

**HTTP and WebSocket surface.** CORS is origin-allowlisted (loopback plus the
Tauri origins); auth is header-based, so there is no CSRF vector. A global
request-body size limit bounds memory use. Error responses are run through the
same secret-scrubber as the logs before they cross the wire. On the WebSocket
control plane, `interrupt` / `tool.cancel` / `tool.askuser_response` are scoped
to the owning client, so one paired client cannot drive another's in-flight chat
or tool call.

**Secret handling.** External API keys and the TLS private key live in an
AES-GCM-256 vault (`secrets.enc`) sealed with a master key in the OS keychain
(or a `0600` file fallback on headless Linux); keychain writes are verified by
reading the value back, so a silently-failing keychain falls through to the file
path instead of losing the key. The vault is **write-only over the API**: secret
values are never returned. `GET /settings` redacts secret-typed fields, and
`PATCH /settings` rejects them (they must go through the secrets endpoint). The
client shows a "saved" placeholder for configured keys and only transmits a new
value when the user edits the field.

**Settings.** `PATCH /settings` is validated strictly against the shared schema:
unknown keys and wrong types are rejected (so a malformed value cannot break
core or flow into a sidecar argument), and external base-URL fields must be
HTTPS or loopback HTTP. `server.bindHost` is deliberately **not** a schema
setting: a paired client must never be able to widen the core's network exposure
over the API, so the bind interface can only be changed by editing
`settings.json` on disk or via the `LOCA_CORE_HOST` env var.

**Binary, model, and self-update provenance.** The helper-binary manifest
(`binaries.json`) and the core self-update manifest (`core.json`) are
Ed25519-signed and verified at runtime against a public key compiled into the
core. The self-update verifier authenticates the **entire** manifest (version,
binaries, workers, and helpers), so a tampered manifest cannot inject attacker
worker scripts or helper binaries even though their hashes are self-referential.
The staged core binary is sha256-verified before the swap, and a downgrade guard
unconditionally refuses manifests older than the running version, so a replayed
old (signed) manifest cannot re-introduce a fixed vulnerability. Model weights
downloaded from HuggingFace are verified against HF's published sha256 (the LFS
`x-linked-etag`) when available.

**Tool sandbox.** Tools run in Deno subprocesses with `--no-prompt` and an
explicit `--allow-*` set. Each tool runs in its **own** worker keyed per
(extension, tool), so its permissions are exactly the grants for that one tool,
not the union of every enabled tool's grants. Every worker additionally gets
`--deny-read` / `--deny-write` on the core's secret material (`secrets.enc`,
`.master-key`, `.admin-token`, `.admin-password`, the SQLite DB), which Deno
enforces over any
`--allow`, so even a tool granted a broad path like `$home` cannot reach the
vault. npm extension tarballs are integrity-checked (SRI sha512, falling back to
sha1) before extraction; extraction rejects path-traversal (zip-slip) and
symlink/hardlink entries; `deno install` runs with `--allow-scripts=false` so
npm lifecycle scripts never execute; tools install default-disabled and
ungranted; and worker stdout/stderr are size-capped.

**Subprocesses.** Inference and speech run in-process. MCP stdio servers and
the `deno` tool workers are spawned without a shell and inherit only an
allowlist of OS-essential environment variables (not the core's full
environment, which can carry operator secrets like `GITHUB_TOKEN`).

**Tauri client.** The webview loads only the bundled frontend (no remote-domain
IPC), and all core traffic leaves through the Rust host's pinned-TLS net layer
rather than the webview's own fetch/WebSocket. The CSP forbids inline scripts.
The single raw-HTML render path (assistant markdown) is sanitized with DOMPurify
against a tag/attribute allowlist, and remote images are blocked by the CSP, so
there is no XSS-to-exfiltration chain. Links in rendered model output open in
the system browser rather than navigating the app webview, preventing in-frame
phishing. Per-core bearer tokens live in the OS keychain.

**Distribution.** The landing page ships HSTS, `X-Content-Type-Options`,
`X-Frame-Options`, `Referrer-Policy`, and a Content-Security-Policy. Release
artifacts are served from an R2 origin over TLS. Private signing keys stay in a
gitignored `.env`; only public keys are committed.

## Known limitations and accepted risks

- **The web client cannot pin.** Browser fetch/WebSocket expose no
  certificate-pinning API, so the web build relies on ordinary browser CA
  verification and cannot pair with a self-signed core. Full MITM protection
  (pinning plus the cert-bound PAKE) is a desktop-client property.
- **No TLS key rotation.** The core's TLS keypair is generated once and kept for
  the life of the install; there is no rotation mechanism. Rotating it would
  invalidate every paired client's pin and force re-pairing. If the vault (and
  so the key) is compromised, the attacker can already read the stored secrets,
  so the marginal exposure is limited, but a rotation story is a tracked
  hardening item.
- **Paired clients are equal administrators.** There is no per-client role
  separation. Any paired client can reconfigure the core, trigger a self-update,
  or revoke another client (which cascade-deletes that client's data). This is
  by design under the trust model above; the destructive client actions in the
  UI are gated behind a confirmation to guard against honest mistakes.
- **Install-time provenance.** The native installers carry the Core payload
  inside the signed installer artifact itself; any install-time fetch (the
  `install-service` bootstrap fetching what the payload did not carry, e.g. the
  built-in extension) runs through the Core binary's own fetch-verify path,
  which verifies the manifest's Ed25519 signature against the committed signing
  key BEFORE trusting any artifact URL or sha256, then checks each download's
  sha256 against the now-authenticated manifest. Once installed, the running
  core re-enforces the signature on every subsequent update. The client bundle
  is minisign-verified by the Tauri updater for in-app updates.
- **Latest channel sidecar provenance.** On the latest channel, sidecar binaries
  resolve to the latest upstream GitHub release at runtime and are verified
  against GitHub's published sha256 over TLS, which is outside our Ed25519
  signature. Trust for latest-channel sidecars shifts partly to GitHub and TLS.
  Stable pins URLs and hashes at release time.
- **Network egress after install (consent-by-configuration).** Once installed,
  Core and Client make no outbound request without an explicit user action:
  update checks are button-gated and downloads happen behind a confirmation modal
  (the requirements size-probe to HuggingFace/GitHub was moved behind that modal,
  so it no longer fires on client connect or a settings change). Three paths
  instead run from boot or in the background based on STANDING configuration
  rather than a fresh per-action click, and their consent is the act of
  configuring them: (1) an external LLM/STT/TTS/embedding provider you set is
  contacted when you send a message, and background memory summarization +
  embedding, greetings, and scheduled prompts send that content to it from idle;
  (2) remote MCP servers you enable are auto-connected at boot with a background
  reconnect loop; (3) the local llama/speech sidecars and the built-in extension
  only ever talk to loopback. To keep Core fully local, leave the provider on the
  local model and add no remote MCP servers.
- **Extensions run third-party code.** A extension you install runs with the
  Deno permissions you grant it. Grant narrowly: a broad
  `read`/`write`/`run`/`ffi` grant is real capability (the vault is always
  denied, but a granted `$home` read still exposes the rest of your home
  directory). Tools are sandboxed per-tool, but a tool you grant network and run
  access can still act with those capabilities.
- **Defense-in-depth items not yet tightened.** The Tauri `connect-src` CSP
  still allows a broad `https:` (no live XSS chain makes this exploitable today,
  and core traffic bypasses the webview via the Rust net layer anyway), the
  `convert_file_to_markdown` command is not path-restricted, the fs capability
  spans `~/.loca/**`, and the signed binary manifest does not yet bind the
  release channel or a monotonic timestamp (a replay would require a compromised
  CDN or TLS MITM, and the core's own downgrade guard rejects older versions).
  These are tracked hardening items.

## Operator hardening

- Keep `server.bindHost` at `127.0.0.1` unless you specifically need LAN
  pairing. Traffic is TLS-encrypted and pinned either way, but widening the bind
  still exposes the pairing and health endpoints to the network. Changing it
  requires editing `settings.json` on disk (or `LOCA_CORE_HOST`); it cannot be
  changed over the API.
- Grant extension permissions as narrowly as possible. Be especially cautious
  with `run`, `ffi`, and broad `read`/`write` paths.
- Back up `~/.loca/<channel>/core/.master-key` if it exists (the
  keychain-sealed path needs no backup); losing it loses all stored secrets.
- Prefer the secrets vault over plaintext values in `settings.json` for API
  keys.
- Only pair devices you trust: pairing makes them administrators of the core.

## Scope

In-scope:

- The Tauri host (Rust) and its commands, capabilities, and CSP.
- The `loca-core` Rust service and its HTTP and WebSocket surface.
- The binary and model download and verification flow (Ed25519-signed
  manifests).
- Secret handling (API keys in the vault / OS keychain).
- Pairing, authentication, and the TLS / certificate-pinning layer.
- The third-party tool sandbox.

Out of scope:

- Vulnerabilities in upstream `llama.cpp`, `sherpa-onnx`, or `deno` binaries
  themselves. Report those upstream. (Their provenance and verification are
  in-scope.)
- Attacks that require an already-compromised local user account or OS keychain
  (we assume both are trusted).
- Actions available only to an already-paired client (paired clients are trusted
  administrators by design).

## Supported versions

The project has not yet made a stable release. All security fixes land on
`main`. Once a `1.0` is cut, we'll document a supported-versions table here.

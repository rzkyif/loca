// Single HTTP+WS client used by the rest of the client app to reach the
// currently-selected loca-core. All requests are bearer-authed (except the
// public health check + pairing-claim).
//
// Multiplexed WS: one socket per paired core, every frame carries its own
// discriminator (streamId / callId / jobId). Subscribers register callbacks
// keyed by frame kind; the client dispatches incoming frames to them.

import type { ApiErrorBody, ClientToServerFrame, ServerToClientFrame } from "@loca/shared";
import { serverToClientFrameSchema } from "@loca/shared";
import type { NetResponse, NetSocket } from "../platform/index.ts";
import { getLogger } from "../util/log.ts";
import { Subscribers } from "../util/subscribers.ts";
import { type CoreTransport, netTransport } from "./transport.ts";

const log = getLogger("ws");

/** How the Client establishes trust in a paired core's TLS. `pin` (the default)
 *  enforces the SPKI pin captured at pairing (a self-signed core); `webpki` does
 *  standard public-CA validation for a core served over HTTPS behind a
 *  terminating reverse proxy. Chosen per core at pairing, then persisted. */
export type TrustMode = "pin" | "webpki";

export interface CoreEndpoint {
  baseUrl: string; // e.g. "https://127.0.0.1:7800"
  token: string; // bearer (from OS keychain)
  trustMode: TrustMode;
  tlsPin?: string; // pinned cert SPKI (base64 SHA-256); required when trustMode === "pin"
}

export type WsListener = (frame: ServerToClientFrame) => void;

// Coarse connection state for UI banners. "connecting" is the initial state
// before the first open, plus during reconnect attempts after a close;
// "disconnected" is the transient gap between those attempts.
//
// The last three PARK the client: no reconnect is scheduled and only an
// explicit retryNow() (or a fresh CoreClient) resumes it.
//  - "unauthorized": the core rejected our bearer at the ticket mint (its DB was
//    reset, this client was revoked), so the same token will never connect.
//    Recovery is a re-pair.
//  - "gaveUp": MAX_ATTEMPTS consecutive attempts failed. The core is unreachable
//    and hammering it further buys nothing; the user retries when they're ready.
//  - "stopped": this device's core process isn't running, so there is nothing to
//    reach. The user starts it, rather than waiting on a reconnect that cannot
//    succeed.
export type ConnectionState =
  | "connecting"
  | "connected"
  | "disconnected"
  | "unauthorized"
  | "gaveUp"
  | "stopped";

export type ConnectionListener = (state: ConnectionState, reason?: string) => void;

// Consecutive failed connect attempts in one disconnect gap before we park as
// "gaveUp". At the remote backoff (500ms, doubling to a 30s cap) six attempts
// span roughly half a minute of trying.
const MAX_ATTEMPTS = 6;

// How much later than its scheduled deadline a reconnect timer has to fire
// before we read it as "this app was suspended" rather than "the event loop was
// busy". Comfortably above the 30s remote backoff cap, so ordinary scheduling
// jitter can never look like a suspend.
const SUSPEND_GAP_MS = 60_000;

// The attempt from which we start asking whether the local core is even running.
// Not the first close: a local core restarts in well under a second (dev
// hot-reload, on-demand respawn) and would otherwise flash "Stopped" every time.
const STOPPED_PROBE_FROM_ATTEMPT = 2;

export class ApiError extends Error {
  readonly code: string;
  readonly status: number;
  readonly details?: Record<string, unknown>;
  constructor(status: number, body: ApiErrorBody["error"]) {
    super(body.message);
    this.code = body.code;
    this.status = status;
    this.details = body.details;
  }
}

export class CoreClient {
  private ws: NetSocket | null = null;
  private wsBackoffMs: number;
  private readonly wsBackoffInitialMs: number;
  private readonly wsBackoffCapMs: number;
  // How long to wait for `onOpen` after a connect starts before giving up on it
  // (see armConnectWatchdog). Tighter for a loopback core, looser for remote.
  private readonly wsConnectTimeoutMs: number;
  private readonly isLocalCore: boolean;
  private listeners = new Subscribers<WsListener>();
  private wsConnected = false;
  private wsClosing = false;
  // A connect is in flight (openSocket running) or a reconnect timer is armed.
  // Together with `ws` these gate connectWs() so subscribe() during the backoff
  // window can't open a second socket racing the scheduled reconnect.
  private wsConnecting = false;
  private wsReconnectTimer: ReturnType<typeof setTimeout> | null = null;
  // Deadline timer for the in-flight connect; cleared once the socket opens.
  private wsConnectTimer: ReturnType<typeof setTimeout> | null = null;
  // Bumped on every connect attempt (and on watchdog/close) so a late-resolving
  // connectWebSocket() from a superseded attempt is discarded, not adopted.
  private wsConnectGen = 0;
  private connState: ConnectionState = "disconnected";
  private connListeners = new Subscribers<ConnectionListener>();
  // Last WS connect-failure reason (from the Tauri error event or an IPC
  // failure), surfaced with the "disconnected" transition so the UI banner can
  // show why. Cleared on a successful open.
  private lastWsError: string | null = null;
  // When the current disconnect gap began (first close after a connected/initial
  // state), or null while connected. Drives the "connected after <ms>ms down"
  // log and gates the one-per-gap "disconnected" line. Reset on a successful open.
  private disconnectedAtMs: number | null = null;
  // Connect attempts in the current gap, for the "connecting (attempt N)" log.
  // Reset on a successful open.
  private connectAttempt = 0;
  // Parked in one of the terminal states: no reconnect is scheduled and none
  // will be until retryNow(). Guards connectWs the same way "unauthorized" does.
  private parked = false;

  /** @param localStoppedProbe resolves true when this endpoint is this device's
   *  own core AND that core's process is not running, so a reconnect cannot
   *  succeed until someone starts it. Injected (rather than reached for here)
   *  to keep this class transport-only: which core is ours, and whether its
   *  process is up, are the registry's concerns. Absent for a remote core.
   *  @param transport how requests cross to the core. Defaults to the pinned
   *  network transport; the registry passes the in-process one for the Core
   *  inside this app. Resolved lazily (see `transport` below) so a client may
   *  still be constructed before the platform is installed. */
  constructor(
    public readonly endpoint: CoreEndpoint,
    private readonly localStoppedProbe?: () => Promise<boolean>,
    private readonly injectedTransport?: CoreTransport,
  ) {
    // A loopback core (dev hot-reload, on-demand local spawn) restarts in well
    // under a second, so reconnect briskly with a low cap; a remote core can be
    // down a while, so back off further to avoid hammering the link. An
    // in-process core is the same case taken further: there is no link at all.
    this.isLocalCore =
      injectedTransport?.inProcess === true ||
      /\/\/(?:127\.0\.0\.1|localhost|\[::1\])(?:[:/]|$)/.test(endpoint.baseUrl);
    this.wsBackoffInitialMs = this.isLocalCore ? 250 : 500;
    this.wsBackoffCapMs = this.isLocalCore ? 2_000 : 30_000;
    // The connect deadline does NOT follow the backoff profile, because it is
    // measuring something different on each transport. Over a socket it bounds a
    // connect that resolved but never opened, and legitimate connect latency is
    // a round trip, so seconds are generous. In process the connect does not
    // resolve until the Core has BOOTED: on a first-ever start that is root
    // dirs, SQLite migrations, and the offline install of the built-in extension
    // behind an Ed25519 + sha256 verify, none of which a phone does in four
    // seconds. Retrying buys nothing there (the boot is start-once, so every
    // attempt awaits the same one), so a tight deadline only converts a slow
    // start into a parked client. What the deadline still catches is a boot that
    // HANGS; a boot that FAILS rejects instead and takes the backoff path
    // immediately, without waiting for this at all.
    this.wsConnectTimeoutMs =
      injectedTransport?.inProcess === true ? 60_000 : this.isLocalCore ? 4_000 : 12_000;
    this.wsBackoffMs = this.wsBackoffInitialMs;
  }

  /** Leave a parked state and connect again immediately. The UI calls this when
   *  the user clicks the Disconnected / Stopped pill (after starting the core,
   *  in the Stopped case). A client parked as "unauthorized" is NOT revived:
   *  its token is dead, and recovery there is a re-pair that builds a new
   *  client. Safe to call on a healthy client (connectWs no-ops). */
  retryNow(): void {
    if (this.connState === "unauthorized") return;
    this.parked = false;
    this.connectAttempt = 0;
    this.wsBackoffMs = this.wsBackoffInitialMs;
    this.disconnectedAtMs = null;
    this.connectWs();
  }

  // Subscribe to connection-state transitions. The listener is invoked
  // synchronously on register with the current state, and then on every
  // change. Returns an unsubscribe function. UI consumers use this to
  // drive the "Reconnecting to <core>…" banner after a 5s disconnect.
  onConnectionState(listener: ConnectionListener): () => void {
    const off = this.connListeners.add(listener);
    queueMicrotask(() => listener(this.connState, this.lastWsError ?? undefined));
    return off;
  }

  get connectionState(): ConnectionState {
    return this.connState;
  }

  private setConnState(state: ConnectionState, reason?: string): void {
    if (this.connState === state) return;
    this.connState = state;
    this.connListeners.emit(state, reason);
  }

  // --- REST ---------------------------------------------------------------

  async health(): Promise<{ status: string; version: string; uptimeMs: number }> {
    return await this.fetchJson("GET", "/api/v1/health", undefined, {
      auth: false,
    });
  }

  async get<T>(path: string): Promise<T> {
    return await this.fetchJson<T>("GET", path);
  }

  async post<T>(path: string, body?: unknown): Promise<T> {
    return await this.fetchJson<T>("POST", path, body);
  }

  async patch<T>(path: string, body?: unknown): Promise<T> {
    return await this.fetchJson<T>("PATCH", path, body);
  }

  async put<T>(path: string, body?: unknown): Promise<T> {
    return await this.fetchJson<T>("PUT", path, body);
  }

  // `body` is rarely used on DELETE, but the cross-client revoke carries the
  // admin password in the request body (over the pinned TLS channel).
  async del<T>(path: string, body?: unknown): Promise<T | void> {
    return await this.fetchJson<T>("DELETE", path, body);
  }

  /** Alias for `del`. Matches `fetch`'s method naming so call sites
   *  reading like `client.delete("/foo")` are natural. */
  async delete<T>(path: string, body?: unknown): Promise<T | void> {
    return await this.del<T>(path, body);
  }

  // multipart for attachments + STT audio uploads. FormData is encoded to raw
  // multipart bytes here (with its boundary content-type) so the pinned Rust
  // net layer can send it as an opaque body.
  async postForm<T>(path: string, form: FormData): Promise<T> {
    const encoded = new Response(form);
    const body = new Uint8Array(await encoded.arrayBuffer());
    const contentType = encoded.headers.get("content-type") ?? "multipart/form-data";
    const res = await this.transport.fetch({
      url: this.endpoint.baseUrl + path,
      method: "POST",
      headers: {
        Authorization: `Bearer ${this.endpoint.token}`,
        "Content-Type": contentType,
      },
      body,
      ...this.netTrust(),
    });
    return await this.parseResponse<T>(res);
  }

  // POST a JSON body and get a binary blob back (TTS synth WAV).
  async postBlob(path: string, body: unknown): Promise<Blob> {
    const res = await this.transport.fetch({
      url: this.endpoint.baseUrl + path,
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.endpoint.token}`,
      },
      body: JSON.stringify(body),
      ...this.netTrust(),
    });
    if (!isOk(res)) this.throwApiError(res);
    return new Blob([res.body as BlobPart]);
  }

  /** One request through this client's transport, with headers of the caller's
   *  choosing and no response parsing. For the few call sites that need a
   *  header this class does not send (the admin token) or already hold an
   *  absolute URL (a stored attachment path).
   *
   *  It exists so those sites do not reach for `platform().net` themselves: a
   *  raw network fetch is the wrong answer for a Core that is in process, and
   *  the base URL it would target is a sentinel with nothing behind it. The
   *  trust mode is this client's and is not the caller's to choose. */
  async fetchRaw(req: {
    url: string;
    method?: string;
    headers?: Record<string, string>;
    body?: string | Uint8Array;
  }): Promise<NetResponse> {
    return await this.transport.fetch({ ...req, ...this.netTrust() });
  }

  // For binary endpoints (TTS WAV blob, attachment downloads).
  async fetchBlob(path: string): Promise<Blob> {
    const res = await this.transport.fetch({
      url: this.endpoint.baseUrl + path,
      headers: { Authorization: `Bearer ${this.endpoint.token}` },
      ...this.netTrust(),
    });
    if (!isOk(res)) this.throwApiError(res);
    return new Blob([res.body as BlobPart]);
  }

  // --- WebSocket ---------------------------------------------------------

  subscribe(listener: WsListener): () => void {
    const off = this.listeners.add(listener);
    if (!this.ws) this.connectWs();
    return off;
  }

  sendWs(frame: ClientToServerFrame): void {
    if (this.ws && this.wsConnected) {
      this.ws.send(JSON.stringify(frame));
    }
  }

  close(): void {
    this.wsClosing = true;
    this.wsConnecting = false;
    this.wsConnectGen++; // discard any in-flight connect
    this.clearConnectWatchdog();
    if (this.wsReconnectTimer !== null) {
      clearTimeout(this.wsReconnectTimer);
      this.wsReconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
  }

  isWsConnected(): boolean {
    return this.wsConnected;
  }

  // --- internals ---------------------------------------------------------

  // How this client reaches its core. A getter rather than a constructor
  // default so `platform()` is still read per call: a default parameter would
  // resolve it at construction and break any caller that builds a client before
  // setPlatform().
  private get transport(): CoreTransport {
    return this.injectedTransport ?? netTransport();
  }

  // The TLS trust arguments for a paired request. Pin mode enforces the stored
  // SPKI pin; webpki mode does standard public-CA validation with no pin. Every
  // net.fetch / connectWebSocket call spreads this, so the mode is never absent.
  // Public so the few call sites that build a raw request outside this class
  // (pairing's admin-token fetch, attachment fetch) stay in the same trust mode.
  netTrust(): { mode: TrustMode; pin?: string } {
    return this.endpoint.trustMode === "pin"
      ? { mode: "pin", pin: this.endpoint.tlsPin }
      : { mode: "webpki" };
  }

  private async fetchJson<T>(
    method: string,
    path: string,
    body?: unknown,
    options: { auth?: boolean } = {},
  ): Promise<T> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    if (options.auth !== false) {
      headers.Authorization = `Bearer ${this.endpoint.token}`;
    }
    const res = await this.transport.fetch({
      url: this.endpoint.baseUrl + path,
      method,
      headers,
      body: body !== undefined ? JSON.stringify(body) : undefined,
      ...this.netTrust(),
    });
    return this.parseResponse<T>(res);
  }

  private parseResponse<T>(res: NetResponse): T {
    if (res.status === 204) return undefined as T;
    if (!isOk(res)) this.throwApiError(res);
    const ct = res.headers["content-type"] ?? "";
    if (!ct.includes("application/json")) {
      // No JSON consumer in the workspace requests anything else through
      // this code path. Binary endpoints (TTS WAV, attachments) use
      // `fetchBlob`, SSE goes direct. A non-JSON content-type here is a
      // server-side bug; fail loud rather than cast a string to `T`.
      throw new ApiError(res.status, {
        code: "internal_error",
        message: `expected JSON response, got content-type "${ct}"`,
      });
    }
    return JSON.parse(decodeBody(res)) as T;
  }

  private throwApiError(res: NetResponse): never {
    let body: ApiErrorBody | undefined;
    try {
      body = JSON.parse(decodeBody(res)) as ApiErrorBody;
    } catch {
      /* */
    }
    if (body?.error) throw new ApiError(res.status, body.error);
    throw new ApiError(res.status, {
      code: "internal_error",
      message: `HTTP ${res.status}`,
    });
  }

  private connectWs(): void {
    if (this.wsClosing) return;
    // Parked: the credentials are dead, the core is unreachable, or its process
    // is down. Retrying buys nothing, so a stray subscribe() must not revive the
    // loop; only retryNow() (or, for "unauthorized", a fresh CoreClient) does.
    if (this.parked) return;
    // Already connected, a connect is in flight, or a reconnect is scheduled:
    // don't open a competing socket.
    if (this.ws || this.wsConnecting || this.wsReconnectTimer !== null) return;
    this.wsConnecting = true;
    const gen = ++this.wsConnectGen;
    log.debug(`connecting to ${this.endpoint.baseUrl} (attempt ${++this.connectAttempt})`);
    this.setConnState("connecting");
    this.armConnectWatchdog();
    void this.mintTicketAndOpen(gen);
  }

  // Mint a short-lived WS ticket over authed HTTP (bearer in the Authorization
  // header, kept out of the ws:// URL), then open the socket with only the
  // ticket in the URL. The mint is the sole credential check now: a 401/403
  // here is the dead-bearer signal (revoked client / core reset its DB) that
  // never recovers, so it is terminal. Every other outcome is transient - an
  // offline/5xx mint, or a stale/spent ticket that the upgrade later rejects -
  // and the backoff loop re-mints a fresh ticket rather than parking.
  private async mintTicketAndOpen(gen: number): Promise<void> {
    // An in-process core has no wire URL, and the URL is the only thing the
    // ticket exists to travel in: the identity is injected below the boundary
    // instead. Minting one anyway would send a credential nobody reads and make
    // the connection depend on a route with no part in it.
    if (this.transport.inProcess) {
      await this.openSocket("", gen);
      return;
    }
    let ticket: string;
    try {
      ticket = (await this.post<{ ticket: string }>("/api/v1/ws/ticket")).ticket;
    } catch (err) {
      if (gen !== this.wsConnectGen) return; // superseded while minting
      if (err instanceof ApiError && (err.status === 401 || err.status === 403)) {
        this.enterUnauthorized(`HTTP ${err.status}`);
        return;
      }
      this.lastWsError = err instanceof ApiError ? `HTTP ${err.status}` : String(err);
      this.handleWsClosed();
      return;
    }
    if (gen !== this.wsConnectGen || this.wsClosing) return;
    const wsUrl =
      this.endpoint.baseUrl.replace(/^http/, "ws") + `/ws/v1?ticket=${encodeURIComponent(ticket)}`;
    await this.openSocket(wsUrl, gen);
  }

  // Open a pinned WebSocket through the platform net layer (Rust on desktop,
  // browser WebSocket on web). The socket primitive differs, but the
  // backoff / ping-pong / dispatch logic is identical to the old browser path.
  private async openSocket(wsUrl: string, gen: number): Promise<void> {
    let sock: NetSocket;
    try {
      sock = await this.transport.connectWebSocket(wsUrl, this.netTrust());
    } catch (err) {
      // A newer attempt (watchdog/close) superseded this one while it was in
      // flight: drop it silently so we don't double-schedule a reconnect.
      if (gen !== this.wsConnectGen) return;
      // IPC-level failure (rare). The common connect failure (DNS / TLS pin
      // mismatch / refused) arrives later via the WS error event (onError
      // below). Record the reason and treat like a close so the backoff
      // reconnect loop kicks in.
      this.lastWsError = String(err);
      this.handleWsClosed();
      return;
    }
    // Superseded by a newer attempt, or close() raced in while we were
    // connecting: discard this socket.
    if (gen !== this.wsConnectGen || this.wsClosing) {
      try {
        sock.close();
      } catch {
        /* */
      }
      return;
    }
    this.ws = sock;
    // Connected: the `ws` guard in connectWs() now applies.
    this.wsConnecting = false;
    // Every callback is generation-guarded: once the watchdog (or a close)
    // abandons this attempt by bumping wsConnectGen, a late event from this now-
    // stale socket must NOT drive state, or it could tear down the healthy
    // socket a newer attempt has since opened.
    sock.onOpen(() => {
      if (gen !== this.wsConnectGen) {
        try {
          sock.close();
        } catch {
          /* */
        }
        return;
      }
      this.clearConnectWatchdog();
      this.wsConnected = true;
      this.wsBackoffMs = this.wsBackoffInitialMs;
      this.lastWsError = null;
      const downMs = this.disconnectedAtMs !== null ? Date.now() - this.disconnectedAtMs : null;
      log.info(downMs !== null ? `connected after ${downMs}ms down` : "connected");
      this.disconnectedAtMs = null;
      this.connectAttempt = 0;
      this.setConnState("connected");
    });
    sock.onMessage((data) => {
      if (gen !== this.wsConnectGen) return;
      this.handleWsMessage(data);
    });
    sock.onClose(() => {
      if (gen !== this.wsConnectGen) return;
      this.handleWsClosed();
    });
    sock.onError((reason) => {
      if (gen !== this.wsConnectGen) return;
      if (reason) this.lastWsError = reason;
      try {
        sock.close();
      } catch {
        /* */
      }
      // Drive recovery from the error itself rather than waiting for a following
      // onClose: an error always means this socket is done, so schedule the
      // reconnect now. A later onClose for the same socket re-enters
      // handleWsClosed but no-ops on the reconnect-timer / state guards (and the
      // generation is unchanged), so this can't double-schedule.
      this.handleWsClosed();
    });
  }

  // Some transports resolve connectWebSocket() before the socket is truly open
  // (or never deliver a close for a half-open socket), which would wedge us in
  // "connecting" forever with no reconnect and a stuck "Reconnecting…" banner.
  // Arm a deadline when a connect starts; if `onOpen` hasn't cleared it in time,
  // tear the attempt down and fall into the backoff loop. Bumping the generation
  // discards a late resolve from the abandoned attempt.
  private armConnectWatchdog(): void {
    this.clearConnectWatchdog();
    this.wsConnectTimer = setTimeout(() => {
      this.wsConnectTimer = null;
      if (this.wsConnected || this.wsClosing) return;
      log.warn(`ws connect timed out after ${this.wsConnectTimeoutMs}ms; retrying`);
      this.wsConnectGen++;
      const sock = this.ws;
      this.ws = null;
      this.wsConnecting = false;
      if (sock) {
        try {
          sock.close();
        } catch {
          /* */
        }
      }
      this.handleWsClosed();
    }, this.wsConnectTimeoutMs);
  }

  private clearConnectWatchdog(): void {
    if (this.wsConnectTimer !== null) {
      clearTimeout(this.wsConnectTimer);
      this.wsConnectTimer = null;
    }
  }

  private handleWsMessage(data: string): void {
    // Two-stage parse: JSON decode, then validate the full discriminated
    // union via Zod. Per-variant schemas use `.passthrough()` so unknown
    // fields survive, which lets a newer server send extras without
    // breaking an older client. Frames that fail the kind discriminant
    // are dropped with a warn.
    let raw: unknown;
    try {
      raw = JSON.parse(data);
    } catch (err) {
      log.warn("rejected frame: invalid JSON", err);
      return;
    }
    const parsed = serverToClientFrameSchema.safeParse(raw);
    if (!parsed.success) {
      // Name the offending kind: an unknown/unhandled frame kind (the exact
      // class of bug that silently dropped requirements.snapshot) is the most
      // useful thing to see here.
      const kind = (raw as { kind?: unknown } | null)?.kind;
      log.warn(`rejected frame (kind=${String(kind)}): schema mismatch`, parsed.error.message);
      return;
    }
    // Cast bridges the Zod-parsed shape (which uses `string` for the
    // open-ended `code` field on chat.error) to the narrower TS union
    // (which uses `ErrorCode`). The schemas validate at runtime; the
    // TS narrowing is for downstream-handler ergonomics.
    const frame = parsed.data as unknown as ServerToClientFrame;
    if (frame.kind === "ping") {
      // Reply to the server heartbeat so the hub doesn't drop us after its pong
      // timeout (see core ws/hub.ts armHeartbeat).
      try {
        this.ws?.send(JSON.stringify({ kind: "pong" }));
      } catch {
        /* socket closing */
      }
      return;
    }
    if (frame.kind === "pong") {
      // The core is the only heartbeat initiator (server pings, we pong above)
      // and never sends `pong`, so there is no client-side pong-deadline timer.
      // Still drop a stray protocol pong here so it never reaches app listeners.
      return;
    }
    this.listeners.emit(frame);
  }

  // Park this client permanently: the core rejected our bearer at the ticket
  // mint, which never starts working again, so no reconnect is scheduled. The UI
  // surfaces "unauthorized" as a re-pair prompt; recovery builds a fresh
  // CoreClient. A bad ticket at the upgrade layer does NOT reach here - that is
  // transient and flows through handleWsClosed's normal backoff instead.
  private enterUnauthorized(reason: string): void {
    this.park("unauthorized", reason);
    log.warn(`core rejected credentials (${reason}); halting reconnect, re-pair required`);
  }

  // Stop the reconnect loop and settle on a state the user has to act on. Tears
  // down the in-flight connect and any armed retry, and bumps the generation so
  // a late callback from the abandoned socket can't drive state afterwards.
  // The socket is closed, not merely dropped: parking can race a reconnect the
  // backoff already started (the stopped probe resolves asynchronously), and an
  // orphaned half-open socket would keep the transport busy behind our back.
  private park(state: "unauthorized" | "gaveUp" | "stopped", reason?: string): void {
    this.clearConnectWatchdog();
    if (this.wsReconnectTimer !== null) {
      clearTimeout(this.wsReconnectTimer);
      this.wsReconnectTimer = null;
    }
    this.wsConnectGen++;
    this.wsConnected = false;
    this.wsConnecting = false;
    const sock = this.ws;
    this.ws = null;
    if (sock) {
      try {
        sock.close();
      } catch {
        /* already closing */
      }
    }
    this.disconnectedAtMs = null;
    this.parked = true;
    this.setConnState(state, reason);
  }

  // Is this endpoint this device's own core, with its process down? Asked from
  // STOPPED_PROBE_FROM_ATTEMPT onwards, off the close path so the backoff timer
  // is armed either way; if the answer arrives while we're still down we park as
  // "stopped" instead. Scoped to the disconnect GAP (not the connect generation,
  // which the 250ms local backoff would roll over before a probe resolves): a
  // successful open clears disconnectedAtMs, so a late "it's down" is discarded.
  private async maybeEnterStopped(gapStartedAtMs: number, probe: () => Promise<boolean>) {
    let stopped: boolean;
    try {
      stopped = await probe();
    } catch (err) {
      log.warn("local-core liveness probe failed; staying in reconnect", err);
      return;
    }
    if (!stopped || this.parked || this.disconnectedAtMs !== gapStartedAtMs) return;
    log.info("this device's core is not running; halting reconnect until it is started");
    this.park("stopped", "Core is not running");
  }

  private handleWsClosed(): void {
    this.clearConnectWatchdog();
    this.wsConnected = false;
    this.wsConnecting = false;
    this.ws = null;
    // Log "disconnected" once per gap (the first close); the per-attempt
    // reconnect churn shows at debug below, so repeating it on every retry's
    // close would just be noise.
    const firstCloseOfGap = this.disconnectedAtMs === null;
    if (firstCloseOfGap) {
      this.disconnectedAtMs = Date.now();
      log.info(this.lastWsError ? `disconnected: ${this.lastWsError}` : "disconnected");
    }
    if (this.wsClosing || this.listeners.size === 0) {
      this.setConnState("disconnected", this.lastWsError ?? undefined);
      return;
    }
    // Enough attempts have failed that the core is not coming back on its own.
    // Park rather than retry every 30s forever; the user resumes via retryNow().
    if (this.connectAttempt >= MAX_ATTEMPTS) {
      log.warn(`giving up after ${this.connectAttempt} attempts; retry is now manual`);
      this.park("gaveUp", this.lastWsError ?? undefined);
      return;
    }
    this.setConnState("disconnected", this.lastWsError ?? undefined);
    const probe = this.localStoppedProbe;
    if (probe && this.connectAttempt >= STOPPED_PROBE_FROM_ATTEMPT && this.disconnectedAtMs) {
      void this.maybeEnterStopped(this.disconnectedAtMs, probe);
    }
    if (this.wsReconnectTimer !== null) return; // a reconnect is already scheduled
    const delay = this.wsBackoffMs;
    this.wsBackoffMs = Math.min(this.wsBackoffMs * 2, this.wsBackoffCapMs);
    log.debug(`reconnect in ${delay}ms`);
    const scheduledAtMs = Date.now();
    this.wsReconnectTimer = setTimeout(() => {
      this.wsReconnectTimer = null;
      this.resumeIfSuspended(scheduledAtMs, delay);
      this.connectWs();
    }, delay);
  }

  // A backgrounded WebView has its timers throttled or frozen, so a reconnect
  // scheduled before the app was suspended fires whenever the user comes back.
  // The attempts counted before that gap describe a network the user has since
  // left, and nothing else resets the counter (only a successful open, or an
  // explicit retryNow), so a background gap that began with two or three
  // failures would spend the rest of the budget in the first seconds of the
  // foreground and park as "gaveUp" on a link that is probably fine. Measured
  // rather than subscribed to: a visibility subscription would make this class
  // reach for the platform, which is exactly what keeps it transport-only.
  private resumeIfSuspended(scheduledAtMs: number, delay: number): void {
    if (Date.now() - scheduledAtMs < delay + SUSPEND_GAP_MS) return;
    log.info("reconnect resumed after a long gap; restarting the attempt budget");
    this.connectAttempt = 0;
    this.wsBackoffMs = this.wsBackoffInitialMs;
  }
}

// True for a 2xx NetResponse.
function isOk(res: NetResponse): boolean {
  return res.status >= 200 && res.status < 300;
}

// Decode a NetResponse body (UTF-8 bytes) to text for JSON parsing.
function decodeBody(res: NetResponse): string {
  return new TextDecoder().decode(res.body);
}

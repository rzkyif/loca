// Per-client WebSocket registry. One socket per paired client (multiplexed);
// every frame carries the discriminator it needs (streamId / callId / jobId)
// for the client to route. Chat-related client frames are dispatched to
// the chat service.
//
// Frame routing on the server side: services that want to push to a specific
// client (e.g. tool.askuser_request -> the client whose chat triggered it)
// call hub.broadcastToClient(clientId, frame). Broadcast to all
// (e.g. downloads.snapshot, core.status, extension.snapshot) uses
// hub.broadcastAll(frame).
//
// Heartbeat: server pings every 25 s and expects a pong within 10 s.
// Auth: /ws/v1?ticket=<t>, where <t> is a short-lived single-use ticket the
// client mints over HTTP (POST /api/v1/ws/ticket, bearer in the Authorization
// header). The long-lived bearer never travels in the ws:// URL, so it can't be
// captured from a reverse-proxy access log; the ticket is consumed here.

import type { ServerToClientFrame } from "@loca/shared";
import { type EngineConnection, frameBus } from "@loca/core-engine";
import {
  chatInterruptWsSchema,
  chatStartWsSchema,
  chatSubscribeWsSchema,
  errMessage,
  isSecretSettingKey,
  isValidSettingKey,
  scheduleConfirmResponseSchema,
  toolAskUserResponseSchema,
  toolCancelSchema,
  toolPermissionResponseSchema,
  wsFrameEnvelopeSchema,
} from "@loca/shared";
import { authService } from "../services/auth.ts";
import { isOriginAllowed } from "@loca/core-engine/http/middleware/cors";
import { chatService } from "@loca/core-engine/services/chat";
import { coreStatus } from "../services/core-status.ts";
import { downloadManager } from "../downloads/manager.ts";
import { subscribeUpdate } from "../update/self-updater.ts";
import { notifyRequirementsChanged, onRequirementsChanged } from "../services/requirements.ts";
import {
  subscribeClientSettings,
  subscribeCoreSettings,
} from "@loca/core-engine/services/core-settings";
import { subscribeSecretsChanged } from "@loca/core-engine/services/secrets";
import { onBinaryInstalled, onBinaryInstallFailed } from "../binaries/manager.ts";
import { getLogger } from "../shared/log.ts";

const log = getLogger("ws");

/** Whether a download row's status transition should trigger a requirements
 *  recompute. MODEL rows recompute on entering a terminal state (Completed = now
 *  present; Error = surface the failure). BINARY rows never recompute here: their
 *  Completed is pre-extract, so binary readiness comes only from the install-
 *  lifecycle listeners (onBinaryInstalled / onBinaryInstallFailed); recomputing on
 *  a binary row would race a stale "still missing" snapshot in. Exported for test. */
export function isRequirementsRecomputeEdge(
  destination: string,
  prevStatus: string | undefined,
  nextStatus: string,
): boolean {
  if (destination === "binaries") return false;
  const terminal = (s: string): boolean => s === "Completed" || s === "Error";
  return !terminal(prevStatus ?? "") && terminal(nextStatus);
}

const HEARTBEAT_MS = 25_000;
const PONG_TIMEOUT_MS = 10_000;
// Client frames are small control messages (chat.start, tool responses); reject
// anything wildly larger before parsing so a hostile/buggy client can't make the
// core JSON.parse an unbounded payload.
const MAX_INBOUND_FRAME_CHARS = 1_000_000;

interface Connection {
  ws: WebSocket;
  clientId: string;
  alive: boolean;
  // The engine FrameBus connection this socket is delivered through. Engine
  // services broadcast via frameBus(); the subscribe() below pipes each frame to
  // this socket. Closed when the socket goes away.
  fb: EngineConnection;
  pingTimer?: ReturnType<typeof setTimeout>;
  pongTimer?: ReturnType<typeof setTimeout>;
}

class WsHub {
  private byClient = new Map<string, Set<Connection>>();
  private listenersWired = false;

  // Called on the Deno.serve hook when /ws/v1 receives an upgrade request.
  async handleUpgrade(req: Request): Promise<Response> {
    const url = new URL(req.url);
    // Mirror the HTTP CORS allowlist on the WS upgrade. The upgrade already
    // requires a valid ticket in ?ticket=, but rejecting a cross-origin browser
    // upgrade here is defense in depth. Native clients (Tauri rust, curl) send no
    // Origin and pass.
    const origin = req.headers.get("origin");
    if (origin && !isOriginAllowed(origin)) {
      return new Response("forbidden origin", { status: 403 });
    }
    // Consume the single-use ticket the client minted over HTTP. The long-lived
    // bearer is never in this URL, so a reverse-proxy access log can capture at
    // most a spent, short-lived ticket.
    const ticket = url.searchParams.get("ticket");
    if (!ticket) {
      return new Response("missing ticket", { status: 401 });
    }
    const clientId = authService().consumeWsTicket(ticket);
    if (!clientId) {
      return new Response("invalid or expired ticket", { status: 401 });
    }

    const upgrade = Deno.upgradeWebSocket(req);
    this.registerConnection(upgrade.socket, clientId);
    this.wireListenersOnce();
    return upgrade.response;
  }

  // Delivery is the engine FrameBus's job now: it fans a frame out to every
  // registered connection's sink (each socket's ws.send, wired in
  // registerConnection). Engine services broadcast through frameBus() directly;
  // these delegating methods keep the hub's call shape for shell callers.
  broadcastToClient(clientId: string, frame: ServerToClientFrame): void {
    frameBus().broadcastToClient(clientId, frame);
  }

  broadcastAll(frame: ServerToClientFrame): void {
    frameBus().broadcastAll(frame);
  }

  // Force-close every live socket for a client. Called when a client is
  // revoked or its token is rotated, so a now-untrusted/compromised device's
  // long-lived WS (which only authenticates once, at upgrade) is actually cut
  // off instead of remaining functional until it happens to disconnect.
  closeClient(clientId: string): void {
    const set = this.byClient.get(clientId);
    if (!set) return;
    for (const conn of set) {
      conn.fb.close();
      if (conn.pingTimer !== undefined) clearTimeout(conn.pingTimer);
      if (conn.pongTimer !== undefined) clearTimeout(conn.pongTimer);
      try {
        conn.ws.close(4001, "client revoked");
      } catch {
        /* already closing */
      }
    }
    this.byClient.delete(clientId);
  }

  shutdown(): void {
    for (const set of this.byClient.values()) {
      for (const conn of set) {
        conn.fb.close();
        // Clear heartbeat timers first so a scheduled tick can't fire on a
        // half-closed socket and trigger a noisy ws.send() failure.
        if (conn.pingTimer !== undefined) clearTimeout(conn.pingTimer);
        if (conn.pongTimer !== undefined) clearTimeout(conn.pongTimer);
        try {
          conn.ws.close();
        } catch {
          /* */
        }
      }
    }
    this.byClient.clear();
  }

  // --- internals ---------------------------------------------------------

  private registerConnection(ws: WebSocket, clientId: string): void {
    // Register a FrameBus connection and pipe every delivered frame to this
    // socket. Engine services broadcast via frameBus(); this is the sink.
    const fb = frameBus().registerConnection(clientId);
    fb.subscribe((payload) => {
      try {
        ws.send(payload);
      } catch {
        /* socket closing */
      }
    });
    const conn: Connection = { ws, clientId, alive: true, fb };
    let set = this.byClient.get(clientId);
    if (!set) {
      set = new Set();
      this.byClient.set(clientId, set);
    }
    set.add(conn);

    ws.addEventListener("open", () => {
      this.armHeartbeat(conn);
      // Seed the just-connected client with the current core status. The
      // change-broadcast only fires on edges, so without this a client joining
      // a steady (e.g. idle) core would never learn its status. Also reseeds on
      // reconnect, including after a core swap.
      try {
        conn.ws.send(JSON.stringify({ kind: "core.status", snapshot: coreStatus().snapshot() }));
      } catch {
        /* socket closing */
      }
    });
    ws.addEventListener("message", (ev) => {
      if (typeof ev.data !== "string") {
        log.warn(`rejected ws frame: non-string payload`);
        return;
      }
      if (ev.data.length > MAX_INBOUND_FRAME_CHARS) {
        log.warn(`rejected ws frame: ${ev.data.length} chars exceeds ${MAX_INBOUND_FRAME_CHARS}`);
        return;
      }
      let raw: unknown;
      try {
        raw = JSON.parse(ev.data);
      } catch (err) {
        log.warn(`rejected ws frame: invalid JSON (${errMessage(err)})`);
        return;
      }
      const parsed = wsFrameEnvelopeSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`rejected ws frame: envelope mismatch: ${parsed.error.message}`);
        return;
      }
      this.dispatchClientFrame(conn, parsed.data as Record<string, unknown>);
    });
    ws.addEventListener("close", () => this.removeConnection(conn));
    ws.addEventListener("error", () => this.removeConnection(conn));
  }

  private dispatchClientFrame(conn: Connection, raw: Record<string, unknown>): void {
    const kind = raw.kind;
    if (kind === "ping") {
      try {
        conn.ws.send(JSON.stringify({ kind: "pong" }));
      } catch {
        /* */
      }
      return;
    }
    if (kind === "pong") {
      // Reply to our heartbeat ping (armHeartbeat): the client is alive, so
      // cancel the pending pong-timeout that would otherwise close the socket.
      if (conn.pongTimer !== undefined) {
        clearTimeout(conn.pongTimer);
        conn.pongTimer = undefined;
      }
      return;
    }
    if (kind === "chat.start") {
      const parsed = chatStartWsSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad chat.start: ${parsed.error.message}`);
        return;
      }
      // Fire-and-forget: start() validates + registers the stream then runs the
      // turn in the background. A rejection here is a rejected validation (e.g.
      // unknown session); log it rather than leaving an unhandled rejection.
      void chatService()
        .start(conn.clientId, parsed.data)
        .catch((err) => {
          log.warn(`chat.start failed: ${errMessage(err)}`);
        });
      return;
    }
    if (kind === "chat.interrupt") {
      const parsed = chatInterruptWsSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad chat.interrupt: ${parsed.error.message}`);
        return;
      }
      // Pass the connection's clientId so the chat service only acts on
      // streams/calls owned by this client (cross-client control is rejected).
      chatService().interrupt(parsed.data.streamId, conn.clientId);
      return;
    }
    if (kind === "chat.subscribe") {
      const parsed = chatSubscribeWsSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad chat.subscribe: ${parsed.error.message}`);
        return;
      }
      chatService().resubscribe(conn.clientId, parsed.data.sessionId);
      return;
    }
    if (kind === "tool.askuser_response") {
      const parsed = toolAskUserResponseSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad tool.askuser_response: ${parsed.error.message}`);
        return;
      }
      chatService().forwardAskUserResponse(
        parsed.data.callId,
        parsed.data.requestId,
        parsed.data.answers,
        conn.clientId,
      );
      return;
    }
    if (kind === "tool.permission_response") {
      const parsed = toolPermissionResponseSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad tool.permission_response: ${parsed.error.message}`);
        return;
      }
      chatService().forwardPermissionResponse(
        parsed.data.callId,
        parsed.data.requestId,
        parsed.data.allow,
        conn.clientId,
      );
      return;
    }
    if (kind === "schedule.confirm_response") {
      const parsed = scheduleConfirmResponseSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad schedule.confirm_response: ${parsed.error.message}`);
        return;
      }
      chatService().forwardScheduleResponse(
        parsed.data.callId,
        parsed.data.requestId,
        parsed.data.accepted,
        parsed.data.draft,
        conn.clientId,
      );
      return;
    }
    if (kind === "tool.cancel") {
      const parsed = toolCancelSchema.safeParse(raw);
      if (!parsed.success) {
        log.warn(`bad tool.cancel: ${parsed.error.message}`);
        return;
      }
      chatService().forwardCancel(parsed.data.callId, conn.clientId);
      return;
    }
    log.warn(`unknown ws frame kind: ${String(kind)}`);
  }

  private removeConnection(conn: Connection): void {
    conn.fb.close(); // detach this socket's sink from the FrameBus
    const set = this.byClient.get(conn.clientId);
    if (set) {
      set.delete(conn);
      if (set.size === 0) this.byClient.delete(conn.clientId);
    }
    if (conn.pingTimer !== undefined) clearTimeout(conn.pingTimer);
    if (conn.pongTimer !== undefined) clearTimeout(conn.pongTimer);
  }

  private armHeartbeat(conn: Connection): void {
    const tick = () => {
      if (conn.ws.readyState !== WebSocket.OPEN) return;
      try {
        conn.ws.send(JSON.stringify({ kind: "ping" }));
      } catch {
        /* */
      }
      conn.pongTimer = setTimeout(() => {
        log.warn(`pong timeout for client ${conn.clientId}; closing`);
        try {
          conn.ws.close(4002, "pong timeout");
        } catch {
          /* */
        }
      }, PONG_TIMEOUT_MS);
      conn.pingTimer = setTimeout(tick, HEARTBEAT_MS);
    };
    conn.pingTimer = setTimeout(tick, HEARTBEAT_MS);
  }

  // One-shot wiring of broadcast sources to the hub.
  private wireListenersOnce(): void {
    if (this.listenersWired) return;
    this.listenersWired = true;

    // Rebroadcast the authoritative requirements snapshot whenever it's
    // recomputed (settings change / download completion / binary install).
    onRequirementsChanged((snap) => {
      this.broadcastAll({
        kind: "requirements.snapshot",
        required: snap.required,
        missing: snap.missing,
      });
    });
    // Settings sync: every store change (client PATCH, preset apply, factory
    // reset) is broadcast as a sparse delta so all connected clients converge
    // without polling. Defense in depth: never let secret values or
    // non-schema keys cross the wire, whatever ended up in the store.
    subscribeCoreSettings((settings, changed) => {
      void notifyRequirementsChanged();
      const values: Record<string, unknown> = {};
      const deleted: string[] = [];
      for (const key of changed) {
        if (!isValidSettingKey(key) || isSecretSettingKey(key)) continue;
        if (key in settings) values[key] = settings[key];
        else deleted.push(key);
      }
      if (Object.keys(values).length > 0 || deleted.length > 0) {
        this.broadcastAll({ kind: "settings.updated", values, deleted });
      }
    });
    // Per-client overlay changes go ONLY to the owning client, never broadcast:
    // another client must not see (or converge onto) someone else's preferences.
    // These keys are all client-on-core inference knobs, so no requirements
    // recompute is needed (model files etc. are core-global).
    subscribeClientSettings((clientId, rawValues, rawDeleted) => {
      const values: Record<string, unknown> = {};
      const deleted: string[] = [];
      for (const [key, v] of Object.entries(rawValues)) {
        if (!isValidSettingKey(key) || isSecretSettingKey(key)) continue;
        values[key] = v;
      }
      for (const key of rawDeleted) {
        if (!isValidSettingKey(key) || isSecretSettingKey(key)) continue;
        deleted.push(key);
      }
      if (Object.keys(values).length > 0 || deleted.length > 0) {
        this.broadcastToClient(clientId, { kind: "settings.updated", values, deleted });
      }
    });
    // Vault changes ride the same frame as a names-only field so the
    // configured-secret placeholders stay fresh on every client.
    subscribeSecretsChanged((names) => {
      this.broadcastAll({
        kind: "settings.updated",
        values: {},
        deleted: [],
        secretNames: names,
      });
    });
    // Binary readiness is driven by the install lifecycle, NOT the download
    // queue: a binary's archive download completing is not the same event as the
    // binary being present (extraction runs after, and can fail). Recompute only
    // on a real install success or failure.
    onBinaryInstalled(() => void notifyRequirementsChanged());
    onBinaryInstallFailed(() => void notifyRequirementsChanged());

    // Track per-download status so a recompute fires only on edges that can
    // change `missing`, not on every progress tick. For a MODEL row, "Completed"
    // means the file is now present and "Error" means computeRequirements should
    // surface its failure (see the model-download-error map there), so both are
    // recompute edges.
    //
    // BINARY rows are deliberately excluded here (the `destination !== "binaries"`
    // guard). A binary's Completed is PRE-EXTRACT: recomputing on it would
    // broadcast a stale "still missing" snapshot that races the real, post-extract
    // onBinaryInstalled recompute; binary present/failed state comes ONLY from the
    // install-lifecycle listeners above (onBinaryInstalled / onBinaryInstallFailed).
    // Do NOT drop this guard without moving binary readiness off the extract-after-
    // download path, or the download<->present race returns. (Tested in hub.test.ts.)
    //
    // A tracked row disappearing (the user cleared a completed download) can
    // re-expose a now-missing model file, so that still recomputes.
    const lastDownloadStatus = new Map<string, string>();
    downloadManager().subscribe((snap) => {
      this.broadcastAll({ kind: "downloads.snapshot", items: snap });
      const currentIds = new Set(snap.map((e) => e.id));
      let changed = false;
      for (const e of snap) {
        const prev = lastDownloadStatus.get(e.id);
        if (isRequirementsRecomputeEdge(e.destination, prev, e.status)) changed = true;
        lastDownloadStatus.set(e.id, e.status);
      }
      const removed = [...lastDownloadStatus.keys()].filter((id) => !currentIds.has(id));
      for (const id of removed) lastDownloadStatus.delete(id);
      if (changed || removed.length > 0) void notifyRequirementsChanged();
    });
    subscribeUpdate((e) => {
      if (e.kind === "staged") {
        this.broadcastAll({ kind: "update.staged", version: e.version });
      } else {
        this.broadcastAll({
          kind: "update.error",
          code: e.code,
          message: e.message,
        });
      }
    });
    // Aggregate core lifecycle status. The one status frame broadcast to every
    // client (core readiness/load is the same for all); the client merges it
    // with its own transport state before painting the CoreBar.
    coreStatus().subscribe((snapshot) => {
      this.broadcastAll({ kind: "core.status", snapshot });
    });
  }
}

let _instance: WsHub | null = null;
export function wsHub(): WsHub {
  if (!_instance) _instance = new WsHub();
  return _instance;
}

// Test-only: closes all sockets, drops the cached instance.
export function __resetForTesting(): void {
  if (_instance) _instance.shutdown();
  _instance = null;
}

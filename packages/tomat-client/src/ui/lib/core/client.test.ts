// CoreClient surface that can be exercised without a real network. HTTP goes
// through platform().net.fetch and WebSocket through platform().net
// .connectWebSocket, so we install a fake Platform and drive those.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, CoreClient } from "./client";
import {
  type NetRequest,
  type NetResponse,
  type NetSocket,
  type Platform,
  setPlatform,
} from "../platform/index";
import type { ServerToClientFrame } from "@tomat/shared";

const ENDPOINT = { baseUrl: "https://core", token: "T", trustMode: "pin", tlsPin: "PIN" } as const;

function jsonRes(status: number, obj: unknown): NetResponse {
  return {
    status,
    headers: { "content-type": "application/json" },
    body: new TextEncoder().encode(JSON.stringify(obj)),
  };
}

// A controllable NetSocket; tests fire its lifecycle callbacks by hand.
class FakeNetSocket implements NetSocket {
  opened?: () => void;
  messaged?: (d: string) => void;
  closed?: () => void;
  errored?: (reason?: string) => void;
  sent: string[] = [];
  send(d: string): void {
    this.sent.push(d);
  }
  close(): void {}
  onOpen(cb: () => void): void {
    this.opened = cb;
  }
  onMessage(cb: (d: string) => void): void {
    this.messaged = cb;
  }
  onClose(cb: () => void): void {
    this.closed = cb;
  }
  onError(cb: (reason?: string) => void): void {
    this.errored = cb;
  }
}

const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

describe("ApiError", () => {
  it("captures status + code + message from the error body", () => {
    const err = new ApiError(404, {
      code: "session_not_found",
      message: "no such session",
    });
    expect(err.status).toBe(404);
    expect(err.code).toBe("session_not_found");
    expect(err.message).toBe("no such session");
    expect(err.details).toBeUndefined();
  });

  it("preserves details when present", () => {
    const err = new ApiError(412, {
      code: "permissions_required",
      message: "missing",
      details: { missing: ["net://x"] },
    });
    expect(err.details).toEqual({ missing: ["net://x"] });
  });
});

describe("CoreClient HTTP", () => {
  let netFetch: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    netFetch = vi.fn();
    setPlatform({
      net: { fetch: netFetch, connectWebSocket: vi.fn() },
    } as unknown as Platform);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("sends bearer header + pin on non-public calls", async () => {
    netFetch.mockResolvedValueOnce(jsonRes(200, { ok: 1 }));
    const c = new CoreClient(ENDPOINT);
    const out = await c.get<{ ok: number }>("/api/v1/x");
    expect(out).toEqual({ ok: 1 });

    const req = netFetch.mock.calls[0][0] as NetRequest;
    expect(req.url).toBe("https://core/api/v1/x");
    expect(req.headers).toMatchObject({ Authorization: "Bearer T" });
    expect(req.pin).toBe("PIN");
  });

  it("translates non-2xx JSON error body into ApiError", async () => {
    netFetch.mockResolvedValueOnce(
      jsonRes(400, { error: { code: "validation_error", message: "bad" } }),
    );
    const c = new CoreClient(ENDPOINT);
    try {
      await c.get("/api/v1/x");
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).code).toBe("validation_error");
      expect((e as ApiError).status).toBe(400);
    }
  });

  it("returns undefined for 204 responses", async () => {
    netFetch.mockResolvedValueOnce({
      status: 204,
      headers: {},
      body: new Uint8Array(),
    });
    const c = new CoreClient(ENDPOINT);
    const out = await c.del("/api/v1/x");
    expect(out).toBeUndefined();
  });
});

// Serve the WS-ticket mint the client now calls before opening a socket; any
// other request 404s (these tests only drive the WS path).
function ticketFetch(status = 200): ReturnType<typeof vi.fn> {
  return vi.fn((req: NetRequest): Promise<NetResponse> => {
    if (req.url.endsWith("/api/v1/ws/ticket")) {
      return Promise.resolve(
        status === 200
          ? jsonRes(200, { ticket: "TICKET" })
          : jsonRes(status, { error: { code: "invalid_token", message: "nope" } }),
      );
    }
    return Promise.resolve(jsonRes(404, { error: { code: "not_found", message: "unmocked" } }));
  });
}

describe("CoreClient WebSocket dispatch", () => {
  let last: FakeNetSocket | null = null;
  beforeEach(() => {
    last = null;
    setPlatform({
      net: {
        fetch: ticketFetch(),
        connectWebSocket: (_url: string) => {
          last = new FakeNetSocket();
          return Promise.resolve(last);
        },
      },
    } as unknown as Platform);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  function getLast(): FakeNetSocket {
    if (!last) throw new Error("no socket constructed yet");
    return last;
  }

  it("subscribe() lazily opens a socket and dispatches received frames", async () => {
    const c = new CoreClient(ENDPOINT);
    const received: ServerToClientFrame[] = [];
    c.subscribe((f) => received.push(f));
    await flush();
    const ws = getLast();
    ws.opened?.();
    const frame = {
      kind: "chat.delta",
      streamId: "s1",
      messageId: "m1",
      delta: "hi",
    };
    ws.messaged?.(JSON.stringify(frame));
    expect(received).toEqual([frame]);
  });

  it("subscribe() returns an unsubscribe that stops dispatch", async () => {
    const c = new CoreClient(ENDPOINT);
    const received: ServerToClientFrame[] = [];
    const unsubscribe = c.subscribe((f) => received.push(f));
    await flush();
    const ws = getLast();
    ws.opened?.();
    unsubscribe();
    ws.messaged?.(JSON.stringify({ kind: "chat.chunk", streamId: "s1", contentDelta: "x" }));
    expect(received).toEqual([]);
  });

  it("onConnectionState fires 'connected' once the socket opens", async () => {
    const c = new CoreClient(ENDPOINT);
    const states: string[] = [];
    c.onConnectionState((s) => states.push(s));
    c.subscribe(() => {});
    await flush();
    getLast().opened?.();
    expect(states[states.length - 1]).toBe("connected");
  });

  it("pong frames are dropped and not forwarded to listeners", async () => {
    const c = new CoreClient(ENDPOINT);
    const received: ServerToClientFrame[] = [];
    c.subscribe((f) => received.push(f));
    await flush();
    const ws = getLast();
    ws.opened?.();
    ws.messaged?.(JSON.stringify({ kind: "pong" }));
    expect(received).toEqual([]);
  });
});

describe("CoreClient reconnect", () => {
  let sockets: FakeNetSocket[] = [];
  let netFetch: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    sockets = [];
    netFetch = ticketFetch();
    setPlatform({
      net: {
        fetch: netFetch,
        connectWebSocket: (_url: string) => {
          const s = new FakeNetSocket();
          sockets.push(s);
          return Promise.resolve(s);
        },
      },
    } as unknown as Platform);
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  // Recovery must not depend on the transport delivering an onClose after an
  // onError: an error alone has to schedule the backoff reconnect (see the
  // onError handler in openSocket).
  it("onError alone (no following onClose) still schedules a reconnect", async () => {
    vi.useFakeTimers();
    const c = new CoreClient(ENDPOINT);
    c.subscribe(() => {});
    await vi.advanceTimersByTimeAsync(0); // resolve the connectWebSocket microtask
    expect(sockets.length).toBe(1);
    sockets[0].opened?.();

    // Error with NO onClose follow-up. The remote-core backoff starts at 500ms.
    sockets[0].errored?.();
    await vi.advanceTimersByTimeAsync(600);
    expect(sockets.length).toBe(2);
  });

  // Post-M4 the bearer is checked only at the ticket MINT. A 401 at the UPGRADE
  // layer therefore means a stale/spent ticket, not a dead bearer, so it must be
  // transient: re-mint a fresh ticket and reconnect, never park in "unauthorized".
  // (Regression guard: the old code funnelled any "HTTP 401" reason - including
  // this upgrade-layer one - into the terminal path.)
  it("treats an upgrade-layer 401 as transient (re-mints and reconnects)", async () => {
    vi.useFakeTimers();
    const c = new CoreClient(ENDPOINT);
    const states: string[] = [];
    c.onConnectionState((s) => states.push(s));
    c.subscribe(() => {});
    await vi.advanceTimersByTimeAsync(0); // resolve the mint + connectWebSocket
    expect(sockets.length).toBe(1);
    sockets[0].opened?.();

    // The upgrade is rejected (ticket spent/expired); net.rs surfaces this as an
    // "HTTP 401" onError reason. It must schedule a reconnect, not go terminal.
    sockets[0].errored?.("server rejected connection: HTTP 401");
    await vi.advanceTimersByTimeAsync(600);
    expect(sockets.length).toBe(2);
    expect(states).not.toContain("unauthorized");
  });

  // A 401/403 (core reset its DB, revoked this client, ...) is terminal: stop the
  // reconnect loop and report "unauthorized" instead of hammering the core with a
  // token that will never be accepted again. With the ticket flow the dead bearer
  // is rejected at the ticket MINT (an authed HTTP call), so no socket ever opens.
  it("halts the reconnect loop when the ticket mint is auth-rejected", async () => {
    vi.useFakeTimers();
    netFetch.mockImplementation(
      (req: NetRequest): Promise<NetResponse> =>
        Promise.resolve(
          req.url.endsWith("/api/v1/ws/ticket")
            ? jsonRes(401, { error: { code: "invalid_token", message: "nope" } })
            : jsonRes(404, { error: { code: "not_found", message: "x" } }),
        ),
    );
    const c = new CoreClient(ENDPOINT);
    const states: string[] = [];
    c.onConnectionState((s) => states.push(s));
    c.subscribe(() => {});
    // The mint 401 makes the connect terminal: no socket, no reconnect, ever.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets.length).toBe(0);
    expect(states.at(-1)).toBe("unauthorized");

    // A fresh subscriber must not revive the dead token either.
    c.subscribe(() => {});
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets.length).toBe(0);
    expect(states.at(-1)).toBe("unauthorized");
  });

  // Six consecutive failures mean the core is not coming back on its own, so the
  // client parks: no more sockets, and the UI can offer a manual retry instead of
  // spinning forever behind a 30s backoff.
  it("parks as 'gaveUp' after MAX_ATTEMPTS failed attempts, then resumes on retryNow()", async () => {
    vi.useFakeTimers();
    const c = new CoreClient(ENDPOINT);
    const states: string[] = [];
    c.onConnectionState((s) => states.push(s));
    c.subscribe(() => {});

    // Advance exactly the remote backoff (500ms, doubling) so each step opens
    // one socket, then fail it at once - staying well inside the 12s connect
    // watchdog, which would otherwise abandon attempts on its own schedule.
    for (const delay of [0, 500, 1_000, 2_000, 4_000, 8_000]) {
      await vi.advanceTimersByTimeAsync(delay);
      sockets.at(-1)?.errored?.("Connection refused");
    }
    expect(sockets.length).toBe(6);
    expect(states.at(-1)).toBe("gaveUp");

    // A stray subscriber must not revive the parked loop.
    c.subscribe(() => {});
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets.length).toBe(6);

    // The user clicks the pill: attempts reset and a fresh socket opens.
    c.retryNow();
    await vi.advanceTimersByTimeAsync(0);
    expect(sockets.length).toBe(7);
    expect(states.at(-1)).toBe("connecting");
  });

  // A local core that isn't running can't be reconnected to, only started, so the
  // client parks as "stopped" well before it would have exhausted its attempts.
  it("parks as 'stopped' when the local-stopped probe says this device's core is down", async () => {
    vi.useFakeTimers();
    const c = new CoreClient(ENDPOINT, () => Promise.resolve(true));
    const states: string[] = [];
    c.onConnectionState((s) => states.push(s));
    c.subscribe(() => {});

    // The probe only runs from the SECOND failed attempt, so the first close
    // still schedules an ordinary reconnect (a restarting core must not flash
    // "Stopped").
    await vi.advanceTimersByTimeAsync(0);
    sockets[0].errored?.("Connection refused");
    expect(states).not.toContain("stopped");

    await vi.advanceTimersByTimeAsync(600);
    expect(sockets.length).toBe(2);
    sockets[1].errored?.("Connection refused");
    await vi.advanceTimersByTimeAsync(0); // let the probe resolve

    expect(states.at(-1)).toBe("stopped");
    // Parked: the backoff timer that was armed before the probe landed is torn
    // down, so no seventh socket ever appears.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets.length).toBe(2);
  });

  // The probe is async, so the backoff can open the next socket before it
  // answers. Parking must CLOSE that socket, not just drop the reference, or a
  // half-open connection outlives the parked client.
  it("closes a socket the backoff opened while the stopped probe was in flight", async () => {
    vi.useFakeTimers();
    let resolveProbe!: (down: boolean) => void;
    const c = new CoreClient(ENDPOINT, () => new Promise<boolean>((r) => (resolveProbe = r)));
    c.subscribe(() => {});

    await vi.advanceTimersByTimeAsync(0);
    sockets[0].errored?.(); // attempt 1: no probe yet
    await vi.advanceTimersByTimeAsync(600);
    sockets[1].errored?.(); // attempt 2: probe fires, stays pending

    // The 1s backoff elapses and opens socket 3 while the probe is unanswered.
    await vi.advanceTimersByTimeAsync(1_100);
    expect(sockets.length).toBe(3);
    const closed = vi.spyOn(sockets[2], "close");

    resolveProbe(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(closed).toHaveBeenCalled();
  });

  // A local core that IS running (paired over a tunnel, say) is unreachable for
  // some other reason: keep the ordinary backoff rather than offering a start.
  it("keeps reconnecting when the local-stopped probe says the core is up", async () => {
    vi.useFakeTimers();
    const c = new CoreClient(ENDPOINT, () => Promise.resolve(false));
    const states: string[] = [];
    c.onConnectionState((s) => states.push(s));
    c.subscribe(() => {});

    await vi.advanceTimersByTimeAsync(0);
    sockets[0].errored?.();
    await vi.advanceTimersByTimeAsync(600);
    sockets[1].errored?.();
    await vi.advanceTimersByTimeAsync(1_600);

    expect(states).not.toContain("stopped");
    expect(sockets.length).toBe(3);
  });
});

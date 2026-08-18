// An in-memory stand-in for the Core inside this app, for tests that need the
// embedded transport without a Tauri host.
//
// It fakes the platform BOUNDARY (the six commands and their event stream), not
// the code under test: `localTransport()`, `CoreClient` and `CoresRegistry` are
// the shipped ones. A fake that reimplemented any of those would be testing the
// copy rather than the subject.

import type { LocalCore, LocalCoreReply, LocalCoreRequest } from "../lib/platform/index.ts";

export interface FakeLocalCore extends LocalCore {
  /** Every request that reached the Core, in order. */
  readonly requests: LocalCoreRequest[];
  /** Every frame the client sent, as `[wsId, data]`. */
  readonly sent: Array<[string, string]>;
  /** Ids passed to `wsOpen`, in order. A reconnect must produce a new one. */
  readonly openedIds: string[];
  /** Ids passed to `wsClose`, in order. */
  readonly closedIds: string[];
  /** How many times `start` was called. */
  readonly starts: number;
  /** Live event subscriptions. A connection that failed to open must leave none. */
  readonly subscriberCount: number;
  /** Make the next `wsOpen` reject, standing in for a Core that will not boot. */
  failNextOpen(): void;
  /** Make every `wsOpen` HANG, standing in for a Core that is still booting
   *  (`core_ws_open` awaits the start-once cell). Returns the release. */
  holdOpens(): () => void;
  /** Queue one reply for the next request. Unqueued requests get a 200 `{}`. */
  reply(res: Partial<LocalCoreReply>): void;
  /** Deliver a lifecycle event to one connection's subscriber. */
  emitOpen(wsId?: string): void;
  emitMessage(data: string, wsId?: string): void;
  emitClose(wsId?: string): void;
  /** Run `fn` from inside the next `wsOpen`, for the listeners-before-open race. */
  onNextOpen(fn: (wsId: string) => void): void;
}

interface Handlers {
  onOpen(): void;
  onMessage(data: string): void;
  onClose(): void;
}

function b64(text: string): string {
  return btoa(text);
}

export function fakeLocalCore(): FakeLocalCore {
  const requests: LocalCoreRequest[] = [];
  const sent: Array<[string, string]> = [];
  const openedIds: string[] = [];
  const closedIds: string[] = [];
  const subscribers = new Map<string, Handlers>();
  const replies: Array<Partial<LocalCoreReply>> = [];
  let starts = 0;
  let duringOpen: ((wsId: string) => void) | null = null;
  let failOpen = false;
  let booting: Promise<void> | null = null;

  const latest = (wsId?: string): string => wsId ?? openedIds[openedIds.length - 1] ?? "";

  return {
    get requests() {
      return requests;
    },
    get sent() {
      return sent;
    },
    get openedIds() {
      return openedIds;
    },
    get closedIds() {
      return closedIds;
    },
    get starts() {
      return starts;
    },
    get subscriberCount() {
      return subscribers.size;
    },
    failNextOpen() {
      failOpen = true;
    },
    holdOpens() {
      let release = (): void => {};
      booting = new Promise<void>((r) => {
        release = r;
      });
      return () => {
        booting = null;
        release();
      };
    },
    reply(res) {
      replies.push(res);
    },
    emitOpen(wsId) {
      subscribers.get(latest(wsId))?.onOpen();
    },
    emitMessage(data, wsId) {
      subscribers.get(latest(wsId))?.onMessage(data);
    },
    emitClose(wsId) {
      subscribers.get(latest(wsId))?.onClose();
    },
    onNextOpen(fn) {
      duringOpen = fn;
    },

    start() {
      starts++;
      return Promise.resolve({ version: "0.0.0-test", channel: "dev", root: "/tmp/core" });
    },
    running: () => Promise.resolve(starts > 0),
    http(req) {
      requests.push(req);
      const queued = replies.shift() ?? {};
      return Promise.resolve({
        status: queued.status ?? 200,
        headers: queued.headers ?? { "content-type": "application/json" },
        bodyB64: queued.bodyB64 ?? b64("{}"),
      });
    },
    wsOpen(wsId) {
      if (failOpen) {
        failOpen = false;
        return Promise.reject(new Error("the Core would not start"));
      }
      const attach = (): void => {
        openedIds.push(wsId);
        // Fired from inside wsOpen deliberately: on the real boundary `open` is
        // emitted before any seed frame, so a transport that registered its
        // listeners afterwards would lose it. A test can only see that if the
        // fake is allowed to be this eager.
        const fn = duringOpen;
        duringOpen = null;
        fn?.(wsId);
      };
      // A boot in flight: the real command awaits the start-once cell, so the
      // whole call stays pending until the Core is up.
      if (booting) return booting.then(attach);
      attach();
      return Promise.resolve();
    },
    wsSend(wsId, data) {
      sent.push([wsId, data]);
      return Promise.resolve();
    },
    wsClose(wsId) {
      closedIds.push(wsId);
      return Promise.resolve();
    },
    subscribeWs(wsId, handlers) {
      subscribers.set(wsId, handlers);
      return Promise.resolve(() => subscribers.delete(wsId));
    },
  };
}

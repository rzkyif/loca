// The CoreTransport for the Core inside this app.
//
// Shaped as a near-copy of the remote one (lib/platform/shared.ts) with the
// network taken out, so CoreClient's request handling, frame dispatch and
// reconnect loop all work unchanged. What is dropped is dropped rather than
// ignored: there is no TLS in process, so `mode` and `pin` mean nothing here,
// and there is no `capturedPin` to report.
//
// Every `invoke` / `listen` this needs lives behind `platform().localCore`,
// which is why this file imports no `@tauri-apps/*` and does not have to.

import {
  type LocalCore,
  type NetRequest,
  type NetResponse,
  type NetSocket,
  platform,
} from "../platform/index.ts";
import { getLogger } from "../util/log.ts";
import type { CoreTransport } from "./transport.ts";

const log = getLogger("local-core");

/** The reserved entry's base URL is a sentinel with no host behind it, and
 *  every consumer builds request URLs by concatenating it with a path. Take the
 *  path (and query) back off before handing the request to the Core, which is
 *  what makes those consumers work untouched, attachment URLs included.
 *
 *  A string that does not parse is passed through as-is: the Core accepts a
 *  bare path+query, so a caller that already built one is not an error. */
function toPath(url: string): string {
  try {
    const u = new URL(url);
    return u.pathname + u.search;
  } catch {
    return url;
  }
}

function bytesToBase64(bytes: Uint8Array): string {
  // Bounded chunks, appending per byte: spreading a subarray into
  // String.fromCharCode(...) re-introduces the arg-count overflow the chunking
  // exists to avoid on large bodies (audio, attachments).
  let s = "";
  const chunk = 0x2000;
  for (let i = 0; i < bytes.length; i += chunk) {
    const end = Math.min(i + chunk, bytes.length);
    for (let j = i; j < end; j++) s += String.fromCharCode(bytes[j]);
  }
  return btoa(s);
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function requireLocalCore(): LocalCore {
  const core = platform().localCore;
  if (!core) {
    // Only reachable if a reserved core entry were selected on a build that
    // embeds no Core, which the registry refuses. Loud rather than silent.
    throw new Error("this build has no Core inside it");
  }
  return core;
}

/** Transport for the embedded Core. */
export function localTransport(): CoreTransport {
  return {
    async fetch(req: NetRequest): Promise<NetResponse> {
      const bodyB64 =
        req.body === undefined
          ? null
          : typeof req.body === "string"
            ? bytesToBase64(new TextEncoder().encode(req.body))
            : bytesToBase64(req.body);
      const res = await requireLocalCore().http({
        url: toPath(req.url),
        method: req.method ?? "GET",
        headers: req.headers ?? {},
        bodyB64,
      });
      // No `capturedPin`: an in-process reply presents no certificate, and
      // reporting one would be a claim about a TLS session that never existed.
      return {
        status: res.status,
        headers: res.headers,
        body: base64ToBytes(res.bodyB64),
      };
    },

    async connectWebSocket(): Promise<NetSocket> {
      const core = requireLocalCore();
      // A FRESH id per connection, never reused. Re-using a live id silently
      // replaces the previous connection on the Core side, so recycling one
      // across a reconnect would work by luck rather than by design.
      const wsId = crypto.randomUUID();

      let onOpenCb: (() => void) | null = null;
      let onMessageCb: ((d: string) => void) | null = null;
      let onCloseCb: (() => void) | null = null;
      let openedEarly = false;
      let closedEarly = false;
      const pending: string[] = [];

      let detached = false;
      let unlisten: (() => void) | null = null;
      const detach = (): void => {
        if (detached) return;
        detached = true;
        unlisten?.();
      };

      // Registered BEFORE wsOpen, and awaited. `open` is emitted before any
      // seed frame, so a listener attached afterwards loses both.
      unlisten = await core.subscribeWs(wsId, {
        onOpen: () => {
          if (onOpenCb) onOpenCb();
          else openedEarly = true;
        },
        onMessage: (data) => {
          if (onMessageCb) onMessageCb(data);
          else pending.push(data);
        },
        onClose: () => {
          if (onCloseCb) onCloseCb();
          else closedEarly = true;
          // A local close is always real, so the connection is gone: drop the
          // listeners rather than leaking a set per reconnect.
          detach();
        },
      });

      try {
        await core.wsOpen(wsId);
      } catch (err) {
        // Registering with the frame bus cannot fail, so this is a Core that
        // would not boot. Undo the subscription rather than leaving it live.
        detach();
        throw err;
      }

      return {
        send: (data) => {
          void core.wsSend(wsId, data).catch((e) => log.debug("local ws send failed", e));
        },
        close: () => {
          void core.wsClose(wsId).catch((e) => log.debug("local ws close failed", e));
          detach();
        },
        onOpen: (cb) => {
          onOpenCb = cb;
          if (openedEarly) {
            openedEarly = false;
            cb();
          }
        },
        onMessage: (cb) => {
          onMessageCb = cb;
          if (pending.length) {
            const buf = pending.splice(0);
            for (const m of buf) cb(m);
          }
        },
        onClose: (cb) => {
          onCloseCb = cb;
          if (closedEarly) {
            closedEarly = false;
            cb();
          }
        },
        // Never fires, and is not faked. There is no `error` event on this
        // boundary because registering with the frame bus cannot fail; a
        // synthesized one would be a failure mode that does not exist.
        onError: () => {},
      };
    },

    inProcess: true,
  };
}

// Platform helpers shared by the desktop (tauri.ts) and android (mobile.ts)
// Platform impls. Both run inside a Tauri webview and reach the SAME Rust
// commands for the pieces that are identical across form factors (pinned
// networking, byte<->base64 shaping), so that logic lives here once instead of
// being duplicated in each impl. Like the two impls, this file is under
// lib/platform/ and may import `@tauri-apps/*` (the no-tauri-import rule only
// restricts code outside lib/platform/).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Platform } from "./index";

// Wire shape of the Rust `net_fetch` reply (body is base64 to cross IPC).
interface NetFetchReply {
  status: number;
  headers: Record<string, string>;
  bodyB64: string;
  capturedPin: string | null;
}

export function bytesToBase64(bytes: Uint8Array): string {
  // Build the binary string char-by-char in bounded chunks. Spreading a
  // subarray into String.fromCharCode(...) re-introduces the very arg-count /
  // call-stack overflow the chunking is meant to avoid on large bodies (audio,
  // attachments), so append per byte within each chunk instead.
  let s = "";
  const chunk = 0x2000;
  for (let i = 0; i < bytes.length; i += chunk) {
    const end = Math.min(i + chunk, bytes.length);
    for (let j = i; j < end; j++) s += String.fromCharCode(bytes[j]);
  }
  return btoa(s);
}

export function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// The `net` namespace: HTTP + pinned WebSocket against a paired core, with TLS
// certificate pinning enforced in Rust below the webview. Identical on every
// platform because the Rust commands (net_fetch, net_ws_*) are registered on
// both desktop and mobile, so this single implementation backs both impls.
export const net: Platform["net"] = {
  async fetch(req) {
    const bodyB64 =
      req.body === undefined
        ? null
        : typeof req.body === "string"
          ? bytesToBase64(new TextEncoder().encode(req.body))
          : bytesToBase64(req.body);
    const res = await invoke<NetFetchReply>("net_fetch", {
      url: req.url,
      method: req.method ?? "GET",
      headers: req.headers ?? {},
      bodyB64,
      mode: req.mode,
      pin: req.pin ?? null,
    });
    return {
      status: res.status,
      headers: res.headers,
      body: base64ToBytes(res.bodyB64),
      capturedPin: res.capturedPin ?? undefined,
    };
  },
  discoverCores() {
    // The Rust command is desktop-only; the mobile impl overrides this with a
    // no-op (see mobile.ts), so this invoke only ever runs on desktop.
    return invoke<Array<{ baseUrl: string; version: string; pin: string }>>("discover_lan_cores");
  },
  async connectWebSocket(url, opts) {
    // The Rust side connects + forwards frames as per-id Tauri events. We
    // register the listeners BEFORE invoking net_ws_open and buffer anything
    // that arrives before CoreClient attaches its callbacks, so no open /
    // message is lost in the async gap.
    const wsId = crypto.randomUUID();
    let onOpenCb: (() => void) | null = null;
    let onMessageCb: ((d: string) => void) | null = null;
    let onCloseCb: (() => void) | null = null;
    let onErrorCb: ((reason?: string) => void) | null = null;
    let openedEarly = false;
    let closedEarly = false;
    let earlyError: string | undefined;
    const pending: string[] = [];

    const unlisteners: UnlistenFn[] = await Promise.all([
      listen(`net://ws/${wsId}/open`, () => {
        if (onOpenCb) onOpenCb();
        else openedEarly = true;
      }),
      listen<string>(`net://ws/${wsId}/message`, (e) => {
        if (onMessageCb) onMessageCb(e.payload);
        else pending.push(e.payload);
      }),
      listen(`net://ws/${wsId}/close`, () => {
        if (onCloseCb) onCloseCb();
        else closedEarly = true;
        // The socket is gone: detach the JS-side listeners so a server- or
        // network-initiated close (not just an explicit close()) can't leak
        // 4 Tauri listeners per reconnect over a long-lived session.
        detach();
      }),
      listen<string>(`net://ws/${wsId}/error`, (e) => {
        if (onErrorCb) onErrorCb(e.payload);
        else earlyError = e.payload;
      }),
    ]);
    let detached = false;
    const detach = (): void => {
      if (detached) return;
      detached = true;
      for (const u of unlisteners) u();
    };

    await invoke("net_ws_open", { wsId, url, mode: opts.mode, pin: opts.pin ?? null });

    return {
      send: (data) => {
        void invoke("net_ws_send", { wsId, data });
      },
      close: () => {
        void invoke("net_ws_close", { wsId });
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
      onError: (cb) => {
        onErrorCb = cb;
        if (earlyError !== undefined) {
          const r = earlyError;
          earlyError = undefined;
          cb(r);
        }
      },
    };
  },
};

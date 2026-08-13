// In-process replacement for ws/hub's per-client registry + delivery. Services
// push frames via broadcastToClient / broadcastAll (the same call shape they use
// against wsHub today); the transport registers one connection per client and
// receives serialized frames. Inbound client frames are handed to the registered
// inbound handler (the chat service's WS frame router).
//
// The Deno shell wraps each connection in a real WebSocket (Deno.upgradeWebSocket);
// a future in-process client wraps it directly. Either way the payloads are the
// JSON-encoded @loca/shared frame unions, so the wire contract is identical.

import type { ServerToClientFrame } from "@loca/shared";
import type { EngineConnection } from "./engine.ts";

type FrameSink = (frameJson: string) => void;
type InboundHandler = (clientId: string, frameJson: string) => void;

interface Conn {
  clientId: string;
  sinks: Set<FrameSink>;
}

export class FrameBus {
  private byClient = new Map<string, Set<Conn>>();
  private inbound: InboundHandler | null = null;

  // Wire the handler inbound client frames dispatch to. Set once during engine
  // init; a frame that arrives before it is wired is dropped (no client can send
  // before the engine is up).
  onInbound(handler: InboundHandler): void {
    this.inbound = handler;
  }

  registerConnection(clientId: string): EngineConnection {
    const conn: Conn = { clientId, sinks: new Set() };
    let set = this.byClient.get(clientId);
    if (!set) {
      set = new Set();
      this.byClient.set(clientId, set);
    }
    set.add(conn);
    // Arrow methods so `this` stays the FrameBus without aliasing it.
    return {
      send: (frameJson: string): void => {
        this.inbound?.(clientId, frameJson);
      },
      subscribe: (cb: FrameSink): (() => void) => {
        conn.sinks.add(cb);
        return () => conn.sinks.delete(cb);
      },
      close: (): void => {
        conn.sinks.clear();
        const s = this.byClient.get(clientId);
        s?.delete(conn);
        if (s && s.size === 0) this.byClient.delete(clientId);
      },
    };
  }

  broadcastToClient(clientId: string, frame: ServerToClientFrame): void {
    const set = this.byClient.get(clientId);
    if (!set) return;
    const payload = JSON.stringify(frame);
    for (const conn of set) {
      for (const sink of conn.sinks) deliver(sink, payload);
    }
  }

  broadcastAll(frame: ServerToClientFrame): void {
    const payload = JSON.stringify(frame);
    for (const set of this.byClient.values()) {
      for (const conn of set) {
        for (const sink of conn.sinks) deliver(sink, payload);
      }
    }
  }

  // Drop every connection for a client (revocation / disconnect).
  closeClient(clientId: string): void {
    const set = this.byClient.get(clientId);
    if (!set) return;
    for (const conn of set) conn.sinks.clear();
    this.byClient.delete(clientId);
  }

  closeAll(): void {
    this.byClient.clear();
  }
}

function deliver(sink: FrameSink, payload: string): void {
  try {
    sink(payload);
  } catch {
    // A sink mid-detach must never break delivery to the others.
  }
}

// The process-wide FrameBus. Engine services broadcast through it
// (frameBus().broadcastToClient / broadcastAll); the Deno shell registers each
// upgraded WebSocket as a connection and pipes delivered frames to the socket,
// so the two share one registry.
let _instance: FrameBus | null = null;
export function frameBus(): FrameBus {
  if (!_instance) _instance = new FrameBus();
  return _instance;
}

// Test-only: drop every connection and the cached bus.
export function __resetFrameBusForTesting(): void {
  _instance?.closeAll();
  _instance = null;
}

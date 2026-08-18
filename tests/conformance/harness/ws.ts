// The WebSocket upgrade, performed by hand over a raw TLS socket.
//
// Why not `new WebSocket(url)`: that API hides the HTTP status of a failed
// upgrade. The headless E2E lane documents this as the reason it cannot cover
// the unauthorized / re-pair state at all. Doing the upgrade by hand means the
// suite can assert "a missing ticket is a 401" and "a spent ticket is a 401",
// which are real contract requirements that no other lane can check.
//
// The codec below covers what the core actually speaks on this socket: text
// frames plus ping/pong control frames. Fragmentation is handled on read; we
// never send it.

import { encodeBase64 } from "@std/encoding/base64";

export interface Upgrade {
  status: number;
  statusText: string;
  headers: Headers;
  /** Present only on 101. */
  socket?: WsSocket;
}

export interface WsSocket {
  send(text: string): Promise<void>;
  /** Next text frame, or null on close/timeout. Answers server pings itself,
   *  since a core that heartbeats expects the client to keep the socket alive. */
  next(timeoutMs: number): Promise<string | null>;
  /** Distinguishes "timed out waiting" from "the peer went away". Without it a
   *  polling reader spins on a dead socket instead of stopping. */
  isClosed(): boolean;
  close(): Promise<void>;
}

function readHeaderBlock(buf: Uint8Array): number {
  for (let i = 3; i < buf.length; i++) {
    if (buf[i - 3] === 13 && buf[i - 2] === 10 && buf[i - 1] === 13 && buf[i] === 10) return i + 1;
  }
  return -1;
}

function frame(opcode: number, payload: Uint8Array): Uint8Array {
  const mask = crypto.getRandomValues(new Uint8Array(4));
  const len = payload.length;
  const header: number[] = [0x80 | opcode];
  if (len < 126) header.push(0x80 | len);
  else if (len < 65536) header.push(0x80 | 126, (len >> 8) & 0xff, len & 0xff);
  else {
    header.push(0x80 | 127);
    for (let i = 7; i >= 0; i--) header.push(i < 4 ? (len >>> (i * 8)) & 0xff : 0);
  }
  const out = new Uint8Array(header.length + 4 + len);
  out.set(header, 0);
  out.set(mask, header.length);
  for (let i = 0; i < len; i++) out[header.length + 4 + i] = payload[i] ^ mask[i % 4];
  return out;
}

interface Decoded {
  opcode: number;
  payload: Uint8Array;
  consumed: number;
}

function decode(buf: Uint8Array): Decoded | null {
  if (buf.length < 2) return null;
  const opcode = buf[0] & 0x0f;
  const masked = (buf[1] & 0x80) !== 0;
  let len = buf[1] & 0x7f;
  let off = 2;
  if (len === 126) {
    if (buf.length < 4) return null;
    len = (buf[2] << 8) | buf[3];
    off = 4;
  } else if (len === 127) {
    if (buf.length < 10) return null;
    len = 0;
    for (let i = 2; i < 10; i++) len = len * 256 + buf[i];
    off = 10;
  }
  const maskKey = masked ? buf.subarray(off, off + 4) : null;
  if (masked) off += 4;
  if (buf.length < off + len) return null;
  const payload = buf.slice(off, off + len);
  if (maskKey) for (let i = 0; i < len; i++) payload[i] ^= maskKey[i % 4];
  return { opcode, payload, consumed: off + len };
}

/** Open a WS connection, returning the upgrade result. `ticket` of undefined
 *  omits the query parameter entirely. */
export async function upgrade(
  host: string,
  port: number,
  path: string,
  ticket: string | undefined,
): Promise<Upgrade> {
  const conn = await Deno.connectTls({ hostname: host, port });
  const key = encodeBase64(crypto.getRandomValues(new Uint8Array(16)));
  const url = ticket === undefined ? path : `${path}?ticket=${encodeURIComponent(ticket)}`;
  const req = [
    `GET ${url} HTTP/1.1`,
    `Host: ${host}:${port}`,
    "Upgrade: websocket",
    "Connection: Upgrade",
    `Sec-WebSocket-Key: ${key}`,
    "Sec-WebSocket-Version: 13",
    "",
    "",
  ].join("\r\n");
  await conn.write(new TextEncoder().encode(req));

  let buf = new Uint8Array(0);
  const deadline = Date.now() + 10_000;
  let split = -1;
  while (split < 0) {
    if (Date.now() > deadline) {
      conn.close();
      throw new Error("timed out reading the upgrade response");
    }
    const chunk = new Uint8Array(4096);
    const n = await conn.read(chunk);
    if (n === null) break;
    const merged = new Uint8Array(buf.length + n);
    merged.set(buf);
    merged.set(chunk.subarray(0, n), buf.length);
    buf = merged;
    split = readHeaderBlock(buf);
  }
  if (split < 0) {
    conn.close();
    throw new Error("the connection closed before the upgrade response completed");
  }

  const head = new TextDecoder().decode(buf.subarray(0, split));
  const lines = head.split("\r\n");
  const [, statusRaw, ...rest] = lines[0].split(" ");
  const headers = new Headers();
  for (const line of lines.slice(1)) {
    const idx = line.indexOf(":");
    if (idx > 0) headers.append(line.slice(0, idx).trim(), line.slice(idx + 1).trim());
  }
  const status = Number(statusRaw);
  const statusText = rest.join(" ");

  if (status !== 101) {
    conn.close();
    return { status, statusText, headers };
  }

  // ONE reader loop owns the socket. `next()` reads from the queue it fills,
  // never from the connection. Racing a read against a timeout leaves the read
  // pending, and a second concurrent read on the same TlsConn fails with
  // "Interrupted: operation canceled" - which is exactly what a long-lived
  // watch connection does that a self-contained send/expect step never did.
  const queue: string[] = [];
  let waiter: ((v: string | null) => void) | null = null;
  let closed = false;

  const deliver = (text: string | null): void => {
    if (waiter) {
      const w = waiter;
      waiter = null;
      w(text);
      return;
    }
    if (text !== null) queue.push(text);
  };

  const readerLoop = async (): Promise<void> => {
    let pending = buf.slice(split);
    try {
      for (;;) {
        let progressed = true;
        while (progressed) {
          const d = decode(pending);
          if (!d) break;
          pending = pending.slice(d.consumed);
          if (d.opcode === 0x1 || d.opcode === 0x0) {
            deliver(new TextDecoder().decode(d.payload));
          } else if (d.opcode === 0x9) {
            // Answer a protocol ping so the core does not reap us mid-case.
            await conn.write(frame(0xa, d.payload));
          } else if (d.opcode === 0x8) {
            closed = true;
            deliver(null);
            return;
          }
          progressed = pending.length > 0;
        }
        const chunk = new Uint8Array(8192);
        const n = await conn.read(chunk);
        if (n === null) break;
        const merged = new Uint8Array(pending.length + n);
        merged.set(pending);
        merged.set(chunk.subarray(0, n), pending.length);
        pending = merged;
      }
    } catch {
      // The connection went away, which close() also causes deliberately.
    }
    closed = true;
    deliver(null);
  };

  const reading = readerLoop();

  const socket: WsSocket = {
    send: async (text) => {
      await conn.write(frame(0x1, new TextEncoder().encode(text)));
    },
    next: (timeoutMs) => {
      const queued = queue.shift();
      if (queued !== undefined) return Promise.resolve(queued);
      if (closed) return Promise.resolve(null);
      return new Promise<string | null>((resolve) => {
        const timer = setTimeout(() => {
          if (waiter) waiter = null;
          resolve(null);
        }, timeoutMs);
        waiter = (v) => {
          clearTimeout(timer);
          resolve(v);
        };
      });
    },
    isClosed: () => closed,
    close: async () => {
      closed = true;
      try {
        await conn.write(frame(0x8, new Uint8Array(0)));
      } catch {
        // already gone
      }
      try {
        conn.close();
      } catch {
        // already closed
      }
      deliver(null);
      await reading;
    },
  };

  return { status, statusText, headers, socket };
}

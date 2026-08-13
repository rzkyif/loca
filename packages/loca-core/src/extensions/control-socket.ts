// Per-worker loopback control socket.
//
// On Windows a tool worker runs under a ConPTY so Deno can show its interactive
// permission prompt (stdin + stderr must be console handles). ConPTY merges and
// reflows the child's stdout, so the byte-exact NDJSON worker protocol cannot
// ride it. Instead the worker connects back to this listener over loopback TCP
// and speaks the protocol here, leaving the pseudoconsole free for the prompt.
//
// Security: the listener binds 127.0.0.1 only, on an ephemeral port, and hands
// out a fresh 256-bit token per worker. The worker must present that token as
// its first line or the connection is dropped. The token is passed to the
// worker via a spawn argument; on Windows another user cannot read a process's
// command line without elevation, so this is a private channel. The listener is
// one-shot: it stops accepting once the authentic worker connects.

import { getLogger } from "../shared/log.ts";

const log = getLogger("controlsock");

// A worker that never connects (crashed before boot, wrong Deno flags) must not
// leave us awaiting forever; the pool's boot timeout is 10 s, so give the
// handshake a little less and let the boot timeout report the real failure.
const ACCEPT_TIMEOUT_MS = 8_000;
// Per-connection budget to read the token line. A connection that opens but
// sends nothing (a stray probe of the loopback port) must not stall the accept
// loop for the whole ACCEPT_TIMEOUT_MS.
const TOKEN_READ_TIMEOUT_MS = 1_000;

/** The authenticated protocol channel for one worker: NDJSON in and out. */
export interface ControlChannel {
  /** Serialize an outgoing line (already newline-terminated by the caller's
   *  framing, matching the stdio writer). Writes are chained so they never
   *  interleave. */
  writeLine(line: string): void;
  /** Yields raw NDJSON lines from the worker (the token line is already
   *  consumed by the handshake). */
  readLines(): AsyncIterableIterator<string>;
  close(): void;
}

export class ControlListener {
  private listener: Deno.Listener;
  private closed = false;
  readonly addr: string;
  readonly token: string;

  private constructor(listener: Deno.Listener, token: string) {
    this.listener = listener;
    this.token = token;
    const a = listener.addr as Deno.NetAddr;
    this.addr = `${a.hostname}:${a.port}`;
  }

  /** Bind a one-shot loopback listener with a fresh token. */
  static create(): ControlListener {
    const listener = Deno.listen({ hostname: "127.0.0.1", port: 0, transport: "tcp" });
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    const token = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    return new ControlListener(listener, token);
  }

  /** Accept connections until one presents the correct token, then stop
   *  listening and return its channel. Rejects if none authenticate in time. */
  async accept(): Promise<ControlChannel> {
    const deadline = Date.now() + ACCEPT_TIMEOUT_MS;
    try {
      while (Date.now() < deadline) {
        const conn = await Promise.race([
          this.listener.accept(),
          delay(deadline - Date.now()).then(() => null),
        ]);
        if (conn === null) break; // overall timeout
        const channel = await this.authenticate(conn);
        if (channel) {
          this.close();
          return channel;
        }
        // Wrong or missing token: drop this connection, keep listening. A racing
        // probe cannot lock out the real worker.
        try {
          conn.close();
        } catch {
          /* already gone */
        }
      }
      throw new Error("worker did not connect to the control socket in time");
    } finally {
      this.close();
    }
  }

  /** Read the first line and compare it to the token. Returns a channel seeded
   *  with any bytes that followed the token line, or null on mismatch. */
  private async authenticate(conn: Deno.Conn): Promise<ControlChannel | null> {
    const reader = conn.readable.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    const deadline = Date.now() + TOKEN_READ_TIMEOUT_MS;
    while (!buf.includes("\n")) {
      const chunk = await Promise.race([
        reader.read(),
        delay(deadline - Date.now()).then(() => "timeout" as const),
      ]);
      if (chunk === "timeout" || chunk.done) {
        reader.releaseLock();
        return null;
      }
      buf += decoder.decode(chunk.value, { stream: true });
      // Bound the token line: a real token line is 64 hex chars + newline.
      if (buf.length > 256 && !buf.includes("\n")) {
        reader.releaseLock();
        return null;
      }
    }
    const nl = buf.indexOf("\n");
    const presented = buf.slice(0, nl);
    if (presented !== this.token) {
      log.warn("control socket connection presented an invalid token");
      reader.releaseLock();
      return null;
    }
    const remainder = buf.slice(nl + 1);
    return makeChannel(conn, reader, remainder);
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    try {
      this.listener.close();
    } catch {
      /* already closed */
    }
  }
}

function makeChannel(
  conn: Deno.Conn,
  reader: ReadableStreamDefaultReader<Uint8Array>,
  seed: string,
): ControlChannel {
  const encoder = new TextEncoder();
  const writer = conn.writable.getWriter();
  let chain: Promise<void> = Promise.resolve();
  let closed = false;
  return {
    writeLine(line) {
      if (closed) return;
      chain = chain.then(() => writer.write(encoder.encode(line))).catch(() => {});
    },
    async *readLines() {
      const decoder = new TextDecoder();
      let buf = seed;
      // Emit any complete lines already buffered from the handshake read.
      let lines = buf.split("\n");
      buf = lines.pop() ?? "";
      for (const line of lines) yield line;
      while (true) {
        let chunk: ReadableStreamReadResult<Uint8Array>;
        try {
          chunk = await reader.read();
        } catch {
          break;
        }
        if (chunk.done) break;
        buf += decoder.decode(chunk.value, { stream: true });
        lines = buf.split("\n");
        buf = lines.pop() ?? "";
        for (const line of lines) yield line;
      }
    },
    close() {
      if (closed) return;
      closed = true;
      chain.finally(() => {
        try {
          writer.releaseLock();
        } catch {
          /* ignore */
        }
        try {
          conn.close();
        } catch {
          /* already closed */
        }
      });
    },
  };
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, Math.max(0, ms)));
}

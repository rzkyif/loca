// Observing broadcasts that an HTTP request triggers.
//
// Most of what this core does on the WS is broadcast-on-mutation: PATCH the
// settings and every client gets `settings.updated`, start a download and every
// client gets `downloads.snapshot`. A self-contained WS step (upgrade, send,
// expect, close) cannot see any of it, because the trigger is a different
// request on a different connection. So the watcher opens ONE connection for
// the whole case, before the first step, and buffers frames continuously.
//
// Two choices here are load-bearing and both look like over-engineering.
//
// 1. The connection opens before the FIRST STEP, not when the assertion runs.
//    Opening it lazily at assertion time is the obvious simplification and it
//    reintroduces the race this design exists to remove: the broadcast fires
//    while the HTTP request is in flight, so a connection opened afterwards can
//    miss it entirely and the case then depends on the core being slower than
//    the runner. Buffering from before the trigger means a broadcast that lands
//    mid-request is ALREADY in the buffer when the assertion reads it. Measured:
//    the validated case waits 0ms. The bound only covers the tail, which is what
//    a bound is for.
//
// 2. A slow wait prints a note on SUCCESS as well as on failure. A case that
//    only passes because it waited 4.9s of a 5s bound is one timing change away
//    from failing, and the single moment anyone can act on that cheaply is while
//    it is still green. Reporting it only on failure means it surfaces for the
//    first time as a CI failure in somebody else's package.
//
// Neither is worth "cleaning up" later.

import type { WsSocket } from "../harness/ws.ts";
import { compare, type Mismatch, type NormaliseContext } from "./match.ts";
import type { Json } from "./types.ts";

/** A wait longer than this passed, but only just. Surfaced in the report so a
 *  case that is quietly racing becomes visible before it becomes flaky. */
const SLOW_FRAME_MS = 1_000;

interface Received {
  kind: string;
  frame: Json;
  atMs: number;
}

export interface FrameResult {
  mismatches: Mismatch[];
  waitedMs: number;
  /** Set when the wait was long enough to be worth mentioning. */
  warning?: string;
}

export class FrameWatcher {
  private received: Received[] = [];
  /** Frames before this index have already been claimed by an earlier
   *  assertion, so a later step cannot match a stale broadcast. */
  private cursor = 0;
  private stopped = false;
  private readonly pump: Promise<void>;

  constructor(
    private readonly socket: WsSocket,
    private readonly kinds: Set<string>,
  ) {
    this.pump = this.run();
  }

  private async run(): Promise<void> {
    while (!this.stopped && !this.socket.isClosed()) {
      const text = await this.socket.next(250);
      if (text === null) continue;
      let frame: Json;
      try {
        frame = JSON.parse(text) as Json;
      } catch {
        continue;
      }
      const kind = (frame as { kind?: string }).kind;
      if (kind && this.kinds.has(kind)) {
        this.received.push({ kind, frame, atMs: Date.now() });
      }
    }
  }

  private pending(kinds: Set<string>): Received[] {
    return this.received.slice(this.cursor).filter((r) => kinds.has(r.kind));
  }

  /** Wait until `expected.length` frames of the expected kinds have arrived,
   *  then compare. Claims the frames it matched. */
  async expect(expected: Json[], ctx: NormaliseContext, timeoutMs: number): Promise<FrameResult> {
    const kinds = new Set(
      expected.map((f) => (f as { kind?: string }).kind).filter((k): k is string => !!k),
    );
    const began = Date.now();
    const deadline = began + timeoutMs;
    while (this.pending(kinds).length < expected.length && Date.now() < deadline) {
      if (this.socket.isClosed()) break;
      await new Promise((r) => setTimeout(r, 20));
    }
    const waitedMs = Date.now() - began;
    const got = this.pending(kinds);
    this.cursor = this.received.length;

    // Say which of the two failures this is. "No frame arrived" is a missing
    // broadcast; "the frame arrived and was wrong" is a payload defect. They
    // have completely different causes and lumping them together wastes the
    // reader's first debugging step.
    if (got.length < expected.length) {
      const want = [...kinds].join(", ");
      return {
        waitedMs,
        mismatches: [
          {
            path: "$.frames",
            detail:
              got.length === 0
                ? `no ${want} frame arrived within ${timeoutMs}ms (expected ${expected.length})`
                : `only ${got.length} of ${expected.length} expected frames arrived within ${timeoutMs}ms`,
          },
        ],
      };
    }

    const mismatches = compare(
      got.map((r) => r.frame),
      expected,
      ctx,
      {},
    );
    const warning =
      waitedMs >= SLOW_FRAME_MS
        ? `waited ${waitedMs}ms of a ${timeoutMs}ms bound for the broadcast; ` +
          `close enough to the bound to be worth watching`
        : undefined;
    return { mismatches, waitedMs, warning };
  }

  /** Assert that NO frame of these kinds arrives within the bound. Costs the
   *  full wait by construction: proving a negative means waiting it out. */
  async expectNone(kinds: string[], timeoutMs: number): Promise<FrameResult> {
    const set = new Set(kinds);
    const began = Date.now();
    await new Promise((r) => setTimeout(r, timeoutMs));
    const got = this.pending(set);
    this.cursor = this.received.length;
    return {
      waitedMs: Date.now() - began,
      mismatches:
        got.length === 0
          ? []
          : [
              {
                path: "$.frames",
                detail:
                  `expected no ${kinds.join(", ")} frame, got ${got.length}: ` +
                  JSON.stringify(got.map((r) => r.frame)).slice(0, 300),
              },
            ],
    };
  }

  async stop(): Promise<void> {
    this.stopped = true;
    await this.socket.close();
    await this.pump;
  }
}

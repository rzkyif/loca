// Sliding-window event counter keyed by an arbitrary string. Each key holds the
// timestamps of recent events; entries older than `windowMs` are pruned on
// access. Shared by the auth rate limiters (per-IP pairing attempts and
// admin-password failures), which layer their own policy on top.

export class SlidingWindowCounter {
  private events = new Map<string, number[]>();

  constructor(private readonly windowMs: number) {}

  /** Live (non-expired) event count for `key`, pruning expired entries. */
  liveCount(key: string, now: number = Date.now()): number {
    const prior = this.events.get(key);
    if (!prior) return 0;
    const live = prior.filter((t) => now - t < this.windowMs);
    if (live.length > 0) this.events.set(key, live);
    else this.events.delete(key);
    return live.length;
  }

  /** Append an event for `key`, pruning expired entries in the same pass. */
  record(key: string, now: number = Date.now()): void {
    const live = this.events.get(key)?.filter((t) => now - t < this.windowMs) ?? [];
    live.push(now);
    this.events.set(key, live);
  }

  delete(key: string): void {
    this.events.delete(key);
  }

  /** Number of distinct keys currently tracked. */
  get size(): number {
    return this.events.size;
  }

  /** Drop every key whose window is fully expired, bounding the map across many
   *  distinct keys. */
  sweepExpired(now: number = Date.now()): void {
    for (const [k, ts] of this.events) {
      if (ts.every((t) => now - t >= this.windowMs)) this.events.delete(k);
    }
  }
}

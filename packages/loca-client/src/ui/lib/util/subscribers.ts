// A tiny set of callbacks with isolated error handling. `add` returns an
// unsubscribe; `emit` invokes every listener and swallows individual throws so
// one bad listener can't break the others. Replaces the hand-rolled
// `Set<fn>` + subscribe + try/catch-notify loops in client.ts / cores.ts.

export class Subscribers<T extends (...args: never[]) => void> {
  private fns = new Set<T>();

  add(fn: T): () => void {
    this.fns.add(fn);
    return () => {
      this.fns.delete(fn);
    };
  }

  emit(...args: Parameters<T>): void {
    for (const fn of this.fns) {
      try {
        fn(...args);
      } catch {
        /* a listener throwing must not break the others */
      }
    }
  }

  get size(): number {
    return this.fns.size;
  }
}

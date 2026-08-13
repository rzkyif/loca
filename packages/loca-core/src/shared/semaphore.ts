// Counting semaphore for capping concurrent async work. FIFO: waiters are
// granted a permit in the order they queued, so none is starved. The downloads
// manager runs it with a single permit (at most one transfer at a time, to stay
// HuggingFace rate-limit friendly); the primitive itself is general.

export class Semaphore {
  private inUse = 0;
  private readonly waiters: Array<() => void> = [];

  constructor(private readonly permits: number) {}

  /** Resolves once a permit is free, reserving it for the caller. The caller
   *  must `release()` exactly once when done (typically in a `finally`). */
  acquire(): Promise<void> {
    if (this.inUse < this.permits) {
      this.inUse++;
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      this.waiters.push(() => {
        this.inUse++;
        resolve();
      });
    });
  }

  /** Return a permit, handing it straight to the next waiter (if any). */
  release(): void {
    this.inUse = Math.max(0, this.inUse - 1);
    const next = this.waiters.shift();
    if (next) next();
  }
}

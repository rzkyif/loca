// Tracks a state store's WS / connection-state subscriptions as one unit:
// `attach` wires them once (idempotent), `detach` tears them all down.
// Replaces the per-store `private unsubscribe* = null` fields, the
// `if (this.unsubscribe) return` guard, and the null-checking teardown that
// every store's detach() repeated.

export class Subscriptions {
  #offs: (() => void)[] = [];
  #attached = false;

  /** Wire the subscriptions once. `wire` returns the unsubscribe fns to track;
   *  a no-op if already attached. */
  attach(wire: () => Array<() => void>): void {
    if (this.#attached) return;
    this.#attached = true;
    this.#offs = wire();
  }

  /** Tear down every tracked subscription. */
  detach(): void {
    for (const off of this.#offs) off();
    this.#offs = [];
    this.#attached = false;
  }
}

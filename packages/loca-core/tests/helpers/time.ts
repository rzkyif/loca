// Date.now() mock harness for tests that exercise TTL, rate-limit windows,
// or other wall-clock logic. Patches Date.now globally; restore() puts the
// original implementation back. Does NOT mock the Date constructor itself
// or setTimeout. Anything that touches those must be redesigned around
// the seam this provides.

export interface ClockHandle {
  /** Set absolute time. */
  set(ms: number): void;
  /** Advance time by the given delta. */
  advance(deltaMs: number): void;
  /** Restore the real Date.now. */
  restore(): void;
  /** Current mocked time. */
  now(): number;
}

export function mockClock(initialMs = 1_700_000_000_000): ClockHandle {
  const original = Date.now;
  let current = initialMs;
  Date.now = () => current;
  return {
    set(ms) {
      current = ms;
    },
    advance(deltaMs) {
      current += deltaMs;
    },
    restore() {
      Date.now = original;
    },
    now() {
      return current;
    },
  };
}

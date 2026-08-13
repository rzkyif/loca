// A tool call's time budget, expressed as a pausable countdown rather than a
// fixed deadline. While the call waits on the user (askUser, a permission
// prompt, a schedule confirm) the budget is paused, so a slow human answer
// never counts against the tool's compute time; the call resumes with whatever
// budget remained. `arm` after a `pause` therefore acts as "resume": the
// remaining budget persists across pause/arm cycles.
//
// The owner (InFlightCall) supplies `onExpire`, invoked once if the budget runs
// out while armed. The timer handle is cleared before `onExpire` runs, so the
// owner may re-`arm` from within it (the defensive re-arm when a prompt is
// somehow still open).

export class CallTimeout {
  private timer: ReturnType<typeof setTimeout> | undefined;
  private armedAt: number | undefined;
  private budgetMs: number;

  constructor(
    budgetMs: number,
    private readonly onExpire: () => void,
  ) {
    this.budgetMs = budgetMs;
  }

  /** True while a timer is pending (the budget is counting down). */
  get armed(): boolean {
    return this.timer !== undefined;
  }

  /** Start (or resume) the countdown over the remaining budget. A spent or
   *  zero budget is a no-op, matching a deadline that has already passed. */
  arm(): void {
    if (this.budgetMs <= 0) return;
    this.armedAt = Date.now();
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.armedAt = undefined;
      this.onExpire();
    }, this.budgetMs);
  }

  /** Stop the countdown and subtract the time consumed since `arm` from the
   *  remaining budget, so a later `arm` resumes where this left off. */
  pause(): void {
    if (this.timer === undefined || this.armedAt === undefined) return;
    const elapsed = Date.now() - this.armedAt;
    this.budgetMs = Math.max(0, this.budgetMs - elapsed);
    clearTimeout(this.timer);
    this.timer = undefined;
    this.armedAt = undefined;
  }

  /** Cancel the countdown for good (terminal settle); the budget is not
   *  preserved. */
  disarm(): void {
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
      this.armedAt = undefined;
    }
  }
}

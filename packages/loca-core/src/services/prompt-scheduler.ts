// Scheduled prompts: SQLite-backed CRUD plus a single armed timer that
// fires automated sessions when a schedule's next occurrence arrives.
//
// Arming model: one setTimeout, always pointed at the soonest enabled
// `next_run_at_ms` (re-armed after every CRUD and every tick, capped at
// MAX_ARM_MS so long waits self-heal against clock drift and sleep). A
// tick fires every due row, recomputes its next occurrence, and re-arms.
//
// Missed runs: a row is "missed" when its due time is more than
// MISSED_GRACE_MS in the past at tick time, which covers both core being
// off (boot catch-up: arm() sees the overdue row and ticks immediately)
// and the machine sleeping through the timer. A missed row fires only when
// its `runMissed` flag is set, and at most once: the recompute right after
// always moves `next_run_at_ms` forward from now.

import type { ScheduledPrompt, ScheduledPromptDraft, ScheduleSpec, Session } from "@loca/shared";
import { errMessage } from "@loca/shared";
import { db } from "@loca/core-engine";
import { sidecarManager } from "../sidecars/manager.ts";
import { AppError } from "@loca/core-engine";
import { newScheduledPromptId } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { type AutomatedSessionInput, runAutomatedSession } from "./automated-session.ts";
import { loadCoreSettings } from "@loca/core-engine/services/core-settings";

const log = getLogger("prompt-scheduler");

// Re-arm at least daily even when the next occurrence is further out.
const MAX_ARM_MS = 24 * 60 * 60 * 1000;
// A due run older than this counts as missed (honors `runMissed`) instead
// of merely late (always fires).
const MISSED_GRACE_MS = 10 * 60 * 1000;
// Retry delay while the local model is still loading at tick time.
const SIDECAR_RETRY_MS = 10 * 1000;

/** The soonest occurrence of `spec` strictly after `afterMs`, in the
 *  core's local time, or null when there is none (a passed "once").
 *  Monthly/yearly days past a month's end clamp to its last day. */
export function nextOccurrence(spec: ScheduleSpec, afterMs: number): number | null {
  switch (spec.kind) {
    case "once":
      return spec.atMs > afterMs ? spec.atMs : null;
    case "interval":
      return afterMs + spec.everyMinutes * 60_000;
    case "weekly": {
      const after = new Date(afterMs);
      // Same wall-clock time each day, so candidates ascend with the day
      // offset and the first match is the soonest.
      for (let offset = 0; offset <= 7; offset++) {
        const candidate = new Date(
          after.getFullYear(),
          after.getMonth(),
          after.getDate() + offset,
          spec.hour,
          spec.minute,
          0,
          0,
        );
        if (!spec.weekdays.includes(candidate.getDay())) continue;
        if (candidate.getTime() > afterMs) return candidate.getTime();
      }
      return null; // unreachable with a non-empty weekday list
    }
    case "monthly": {
      const after = new Date(afterMs);
      for (let offset = 0; offset <= 1; offset++) {
        const year = after.getFullYear();
        const month = after.getMonth() + offset;
        const day = Math.min(spec.day, daysInMonth(year, month));
        const t = new Date(year, month, day, spec.hour, spec.minute, 0, 0).getTime();
        if (t > afterMs) return t;
      }
      return null; // unreachable: the offset-1 candidate is in a later month
    }
    case "yearly": {
      const after = new Date(afterMs);
      for (let offset = 0; offset <= 1; offset++) {
        const year = after.getFullYear() + offset;
        const day = Math.min(spec.day, daysInMonth(year, spec.month - 1));
        const t = new Date(year, spec.month - 1, day, spec.hour, spec.minute, 0, 0).getTime();
        if (t > afterMs) return t;
      }
      return null; // unreachable: the offset-1 candidate is in a later year
    }
  }
}

function daysInMonth(year: number, monthIndex: number): number {
  // Day 0 of the following month; Date normalizes out-of-range months.
  return new Date(year, monthIndex + 1, 0).getDate();
}

interface Row {
  id: string;
  owner_client_id: string;
  title: string;
  instruction: string;
  schedule_json: string;
  run_missed: number;
  enabled: number;
  last_run_at_ms: number | null;
  next_run_at_ms: number | null;
  created_at_ms: number;
  updated_at_ms: number;
}

function rowToPrompt(row: Row): ScheduledPrompt {
  return {
    id: row.id,
    ownerClientId: row.owner_client_id,
    title: row.title,
    instruction: row.instruction,
    schedule: JSON.parse(row.schedule_json) as ScheduleSpec,
    runMissed: row.run_missed === 1,
    enabled: row.enabled === 1,
    ...(row.last_run_at_ms != null ? { lastRunAtMs: row.last_run_at_ms } : {}),
    ...(row.next_run_at_ms != null ? { nextRunAtMs: row.next_run_at_ms } : {}),
    createdAtMs: row.created_at_ms,
    updatedAtMs: row.updated_at_ms,
  };
}

export interface ScheduledPromptPatch {
  title?: string;
  instruction?: string;
  schedule?: ScheduleSpec;
  runMissed?: boolean;
  enabled?: boolean;
}

type FireFn = (input: AutomatedSessionInput) => Promise<Session>;

// Bound the table per owner: schedules are user-curated, and an unbounded
// list only arises from a runaway tool proposing them in a loop.
const MAX_SCHEDULES_PER_OWNER = 100;

export class PromptScheduler {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;
  private ticking = false;

  // Test seams: tests stub the fire path so a due tick doesn't reach the LLM,
  // and stub the loading check to exercise the defer/refuse-while-loading paths.
  constructor(
    private readonly fire: FireFn = runAutomatedSession,
    private readonly stillLoading: () => Promise<boolean> = llmStillLoading,
  ) {}

  /** Arm the timer from the persisted rows. Overdue rows (core was off)
   *  make the first tick fire immediately, which is the boot catch-up. */
  start(): void {
    this.arm();
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  // --- CRUD (owner-scoped) -------------------------------------------------

  list(ownerClientId: string): ScheduledPrompt[] {
    const rows = db()
      .prepare(
        `SELECT * FROM scheduled_prompts WHERE owner_client_id = ? ORDER BY created_at_ms ASC`,
      )
      .all(ownerClientId) as Row[];
    return rows.map(rowToPrompt);
  }

  get(ownerClientId: string, id: string): ScheduledPrompt {
    const row = db()
      .prepare(`SELECT * FROM scheduled_prompts WHERE id = ? AND owner_client_id = ?`)
      .get(id, ownerClientId) as Row | undefined;
    if (!row) {
      throw new AppError("not_found", `scheduled prompt ${id} not found`);
    }
    return rowToPrompt(row);
  }

  create(ownerClientId: string, draft: ScheduledPromptDraft): ScheduledPrompt {
    const id = newScheduledPromptId();
    const now = Date.now();
    const count = db()
      .prepare(`SELECT COUNT(*) AS n FROM scheduled_prompts WHERE owner_client_id = ?`)
      .get(ownerClientId) as { n: number };
    if (count.n >= MAX_SCHEDULES_PER_OWNER) {
      throw new AppError(
        "validation_error",
        `scheduled prompt limit reached (${MAX_SCHEDULES_PER_OWNER}); delete one first`,
      );
    }
    // Only a passed "once" yields no occurrence; saving it would create a
    // schedule that reports success but can never fire.
    if (nextOccurrence(draft.schedule, now) === null) {
      throw new AppError("validation_error", "the scheduled time is already in the past");
    }
    db()
      .prepare(`
      INSERT INTO scheduled_prompts
        (id, owner_client_id, title, instruction, schedule_json, run_missed,
         enabled, next_run_at_ms, created_at_ms, updated_at_ms)
      VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?)
    `)
      .run(
        id,
        ownerClientId,
        draft.title,
        draft.instruction,
        JSON.stringify(draft.schedule),
        draft.runMissed ? 1 : 0,
        nextOccurrence(draft.schedule, now),
        now,
        now,
      );
    this.arm();
    return this.get(ownerClientId, id);
  }

  update(ownerClientId: string, id: string, patch: ScheduledPromptPatch): ScheduledPrompt {
    const current = this.get(ownerClientId, id);
    const schedule = patch.schedule ?? current.schedule;
    const enabled = patch.enabled ?? current.enabled;
    // Keep the armed occurrence unless something that defines it changed:
    // disabling parks the row, a new schedule or a re-enable re-derives it.
    let next: number | null = current.nextRunAtMs ?? null;
    if (!enabled) next = null;
    else if (patch.schedule !== undefined || !current.enabled) {
      next = nextOccurrence(schedule, Date.now());
      // Only a passed "once" yields no occurrence; an enabled schedule that
      // can never fire is a dead row the user thinks is armed.
      if (next === null) {
        throw new AppError("validation_error", "the scheduled time is already in the past");
      }
    }
    db()
      .prepare(`
      UPDATE scheduled_prompts
      SET title = ?, instruction = ?, schedule_json = ?, run_missed = ?,
          enabled = ?, next_run_at_ms = ?, updated_at_ms = ?
      WHERE id = ?
    `)
      .run(
        patch.title ?? current.title,
        patch.instruction ?? current.instruction,
        JSON.stringify(schedule),
        (patch.runMissed ?? current.runMissed) ? 1 : 0,
        enabled ? 1 : 0,
        next,
        Date.now(),
        id,
      );
    this.arm();
    return this.get(ownerClientId, id);
  }

  delete(ownerClientId: string, id: string): void {
    this.get(ownerClientId, id);
    db().prepare(`DELETE FROM scheduled_prompts WHERE id = ?`).run(id);
    this.arm();
  }

  /** Fire a schedule immediately (the settings "Run Now" action). Leaves
   *  the armed occurrence untouched. */
  async runNow(ownerClientId: string, id: string): Promise<Session> {
    const prompt = this.get(ownerClientId, id);
    if (!prompt.enabled) {
      throw new AppError("validation_error", "scheduled prompt is disabled; enable it to run");
    }
    // The tick defers due rows while the local model loads (a turn fired then
    // races the load and errors); a manual run must refuse for the same reason,
    // loudly, so the user retries rather than getting a session that errors.
    if (await this.stillLoading()) {
      throw new AppError(
        "validation_error",
        "the local model is still loading; try again in a moment",
      );
    }
    const session = await this.fire({
      ownerClientId,
      title: prompt.title,
      instruction: prompt.instruction,
      reason: "schedule",
      scheduledPromptId: prompt.id,
      focus: "show",
    });
    db()
      .prepare(`UPDATE scheduled_prompts SET last_run_at_ms = ? WHERE id = ?`)
      .run(Date.now(), id);
    return session;
  }

  // --- arming / firing -------------------------------------------------

  /** Process every due row, then re-arm. Public for tests; production
   *  reaches it only through the armed timer. */
  async tick(): Promise<void> {
    // The loading check awaits, so a CRUD-triggered re-arm can fire a second
    // tick while this one is parked there; the guard makes that a no-op (the
    // running tick re-arms from fresh rows when it finishes).
    if (this.disposed || this.ticking) return;
    this.ticking = true;
    // null re-arms from the rows; a number is an explicit retry delay.
    let rearmDelayMs: number | null = null;
    try {
      const now = Date.now();
      const due = db()
        .prepare(`
      SELECT * FROM scheduled_prompts
      WHERE enabled = 1 AND next_run_at_ms IS NOT NULL AND next_run_at_ms <= ?
    `)
        .all(now) as Row[];
      // While llama is mid-load (boot, or a settings change is restarting it)
      // ensureLoaded() in the chat path no-ops, so a turn fired now would race
      // the model load and error. Hold the due rows and retry shortly.
      if (due.length > 0 && (await this.stillLoading())) {
        log.debug(`local model still loading; deferring ${due.length} due schedule(s)`);
        rearmDelayMs = SIDECAR_RETRY_MS;
        return;
      }
      for (const snapshot of due) {
        // The loading check awaited above is a window in which the row could
        // have been edited, advanced by a concurrent runNow, disabled, or
        // parked. Re-read the FULL row and act on the latest state, not the
        // pre-await snapshot, so we never fire a stale instruction or clobber a
        // freshly-advanced next_run_at_ms.
        const row = db()
          .prepare(`SELECT * FROM scheduled_prompts WHERE id = ?`)
          .get(snapshot.id) as Row | undefined;
        if (!row || row.enabled !== 1) continue;
        if (row.next_run_at_ms === null || row.next_run_at_ms > now) {
          // Advanced, parked, or rescheduled in the gap: no longer due.
          continue;
        }
        let prompt: ScheduledPrompt;
        try {
          prompt = rowToPrompt(row);
        } catch (err) {
          // A corrupt schedule_json must not wedge the whole subsystem in a
          // throw/re-arm loop: park the row (next run cleared, user edit
          // revives it) and keep processing the others.
          log.error(
            `scheduled prompt ${row.id} has an unreadable schedule; parking it: ${errMessage(err)}`,
          );
          db()
            .prepare(`UPDATE scheduled_prompts SET next_run_at_ms = NULL WHERE id = ?`)
            .run(row.id);
          continue;
        }
        const missed = now - (prompt.nextRunAtMs ?? now) > MISSED_GRACE_MS;
        const fires = !missed || prompt.runMissed;
        const next = nextOccurrence(prompt.schedule, now);
        if (fires) {
          // Persist the advanced occurrence BEFORE firing (at-most-once): a
          // crash mid-run skips that occurrence instead of duplicating the
          // automated session on reboot.
          db()
            .prepare(
              `UPDATE scheduled_prompts SET last_run_at_ms = ?, next_run_at_ms = ? WHERE id = ?`,
            )
            .run(now, next, prompt.id);
          try {
            await this.fire({
              ownerClientId: prompt.ownerClientId,
              title: prompt.title,
              instruction: prompt.instruction,
              reason: "schedule",
              scheduledPromptId: prompt.id,
              focus: "show",
            });
          } catch (err) {
            log.error(`scheduled prompt "${prompt.title}" failed to fire: ${errMessage(err)}`);
          }
        } else {
          log.info(
            `scheduled prompt "${prompt.title}" missed its run; skipping (run missed is off)`,
          );
          db()
            .prepare(`UPDATE scheduled_prompts SET next_run_at_ms = ? WHERE id = ?`)
            .run(next, prompt.id);
        }
      }
    } finally {
      // Re-arm even when a row's processing threw: a transient error must
      // not leave the scheduler permanently disarmed.
      this.ticking = false;
      if (rearmDelayMs !== null) this.schedule(rearmDelayMs);
      else this.arm();
    }
  }

  private arm(): void {
    if (this.disposed) return;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    const row = db()
      .prepare(`
      SELECT MIN(next_run_at_ms) AS soonest FROM scheduled_prompts
      WHERE enabled = 1 AND next_run_at_ms IS NOT NULL
    `)
      .get() as { soonest: number | null } | undefined;
    if (row?.soonest == null) return;
    this.schedule(Math.min(Math.max(row.soonest - Date.now(), 0), MAX_ARM_MS));
  }

  private schedule(delayMs: number): void {
    if (this.disposed) return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.tick().catch((err) => {
        log.error(`tick failed: ${errMessage(err)}`);
      });
    }, delayMs);
  }
}

/** True while the local llama sidecar is mid-load (and the provider is
 *  local): an automated turn fired now would race the model load and error.
 *  Shared by the scheduler tick and the greetings route. */
export async function llmStillLoading(): Promise<boolean> {
  const settings = await loadCoreSettings();
  const provider = settings["llm.provider"];
  if (typeof provider === "string" && provider !== "local") return false;
  return sidecarManager().status("llama").status === "Loading";
}

let _instance: PromptScheduler | null = null;
export function promptScheduler(): PromptScheduler {
  if (!_instance) _instance = new PromptScheduler();
  return _instance;
}

export function __resetForTesting(): void {
  _instance?.dispose();
  _instance = null;
}

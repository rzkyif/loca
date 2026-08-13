// nextOccurrence local-time math (table-driven: weekday wrap, month-length
// clamping, year wrap) and PromptScheduler behavior against a real SQLite
// file: CRUD owner scoping, next-run recompute rules on update, due-row
// firing with the missed-run grace, and boot catch-up through start().
//
// Expectations are built with the same `new Date(y, m, d, h, min)` local
// constructor the implementation uses, so they hold in any timezone.

import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import type { ScheduleSpec } from "@loca/shared";
import { db } from "@loca/core-engine";
import { AppError } from "@loca/core-engine";
import { createTestClient, setupTestEnv } from "../../tests/helpers/db.ts";
import type { AutomatedSessionInput } from "./automated-session.ts";
import { nextOccurrence, PromptScheduler } from "./prompt-scheduler.ts";

/** Local-time instant; month is 1-12 for readability. */
function at(y: number, m: number, d: number, h = 0, min = 0): number {
  return new Date(y, m - 1, d, h, min, 0, 0).getTime();
}

Deno.test("nextOccurrence: once fires only while still in the future", () => {
  const spec: ScheduleSpec = { kind: "once", atMs: at(2026, 6, 15, 9, 0) };
  assertEquals(nextOccurrence(spec, at(2026, 6, 14)), at(2026, 6, 15, 9, 0));
  assertEquals(nextOccurrence(spec, at(2026, 6, 15, 9, 0)), null); // not strictly after
  assertEquals(nextOccurrence(spec, at(2026, 6, 16)), null);
});

Deno.test("nextOccurrence: interval counts from the given instant", () => {
  const spec: ScheduleSpec = { kind: "interval", everyMinutes: 90 };
  const after = at(2026, 6, 10, 8, 0);
  assertEquals(nextOccurrence(spec, after), after + 90 * 60_000);
});

Deno.test("nextOccurrence: weekly picks the soonest listed weekday", () => {
  // 2026-06-10 is a Wednesday (getDay 3); anchor the table on that.
  assertEquals(new Date(at(2026, 6, 10)).getDay(), 3);
  const cases: Array<{ spec: ScheduleSpec; after: number; want: number }> = [
    {
      // Same day, time still ahead.
      spec: { kind: "weekly", weekdays: [3], hour: 9, minute: 30 },
      after: at(2026, 6, 10, 8, 0),
      want: at(2026, 6, 10, 9, 30),
    },
    {
      // Same day, time passed: wraps a full week.
      spec: { kind: "weekly", weekdays: [3], hour: 9, minute: 30 },
      after: at(2026, 6, 10, 10, 0),
      want: at(2026, 6, 17, 9, 30),
    },
    {
      // Friday afternoon waiting on Monday: crosses the weekend.
      spec: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
      after: at(2026, 6, 12, 15, 0),
      want: at(2026, 6, 15, 9, 0),
    },
    {
      // Several weekdays: Friday is sooner than next Monday.
      spec: { kind: "weekly", weekdays: [1, 5], hour: 9, minute: 0 },
      after: at(2026, 6, 10, 12, 0),
      want: at(2026, 6, 12, 9, 0),
    },
    {
      // Sunday listed as 0, reached from Saturday.
      spec: { kind: "weekly", weekdays: [0], hour: 7, minute: 0 },
      after: at(2026, 6, 13, 12, 0),
      want: at(2026, 6, 14, 7, 0),
    },
  ];
  for (const c of cases) assertEquals(nextOccurrence(c.spec, c.after), c.want);
});

Deno.test("nextOccurrence: monthly clamps the day to the month's length", () => {
  const cases: Array<{ spec: ScheduleSpec; after: number; want: number }> = [
    {
      spec: { kind: "monthly", day: 15, hour: 9, minute: 0 },
      after: at(2026, 6, 10),
      want: at(2026, 6, 15, 9, 0),
    },
    {
      // Day already passed: next month.
      spec: { kind: "monthly", day: 15, hour: 9, minute: 0 },
      after: at(2026, 6, 20),
      want: at(2026, 7, 15, 9, 0),
    },
    {
      // Day 31 in February 2026 (non-leap): clamps to the 28th.
      spec: { kind: "monthly", day: 31, hour: 9, minute: 0 },
      after: at(2026, 2, 1),
      want: at(2026, 2, 28, 9, 0),
    },
    {
      // Jan 31 occurrence just passed: the Feb candidate is the clamped 28th.
      spec: { kind: "monthly", day: 31, hour: 9, minute: 0 },
      after: at(2026, 1, 31, 10, 0),
      want: at(2026, 2, 28, 9, 0),
    },
  ];
  for (const c of cases) assertEquals(nextOccurrence(c.spec, c.after), c.want);
});

Deno.test("nextOccurrence: yearly wraps the year and clamps Feb 29", () => {
  const cases: Array<{ spec: ScheduleSpec; after: number; want: number }> = [
    {
      spec: { kind: "yearly", month: 8, day: 17, hour: 9, minute: 0 },
      after: at(2026, 6, 1),
      want: at(2026, 8, 17, 9, 0),
    },
    {
      // Date already passed this year.
      spec: { kind: "yearly", month: 8, day: 17, hour: 9, minute: 0 },
      after: at(2026, 9, 1),
      want: at(2027, 8, 17, 9, 0),
    },
    {
      // Feb 29 in a non-leap year clamps to the 28th.
      spec: { kind: "yearly", month: 2, day: 29, hour: 9, minute: 0 },
      after: at(2026, 3, 1),
      want: at(2027, 2, 28, 9, 0),
    },
    {
      // ...and lands back on the 29th when the next year is a leap year.
      spec: { kind: "yearly", month: 2, day: 29, hour: 9, minute: 0 },
      after: at(2027, 3, 1),
      want: at(2028, 2, 29, 9, 0),
    },
  ];
  for (const c of cases) assertEquals(nextOccurrence(c.spec, c.after), c.want);
});

Deno.test("nextOccurrence: weekly keeps the wall-clock time across a DST boundary", () => {
  // US clocks spring forward 2026-03-08 (02:00 -> 03:00) and fall back
  // 2026-11-01 (02:00 -> 01:00); both are Sundays. The impl builds each
  // candidate with the wall-clock date constructor rather than adding
  // 86_400_000 ms per day, so a 09:00 weekly stays 09:00 local even though
  // those transition days are only 23h / 25h long. Expectations use the same
  // constructor, so this holds in any timezone and pins the wall-clock
  // semantics against a naive ms-addition regression.
  assertEquals(new Date(at(2026, 3, 8)).getDay(), 0); // spring-forward Sunday
  assertEquals(new Date(at(2026, 11, 1)).getDay(), 0); // fall-back Sunday
  const cases: Array<{ spec: ScheduleSpec; after: number; want: number }> = [
    {
      // Saturday before spring-forward, waiting on Sunday 09:00 (the DST day).
      spec: { kind: "weekly", weekdays: [0], hour: 9, minute: 0 },
      after: at(2026, 3, 7, 12, 0),
      want: at(2026, 3, 8, 9, 0),
    },
    {
      // Saturday before fall-back, waiting on Sunday 09:00.
      spec: { kind: "weekly", weekdays: [0], hour: 9, minute: 0 },
      after: at(2026, 10, 31, 12, 0),
      want: at(2026, 11, 1, 9, 0),
    },
  ];
  for (const c of cases) assertEquals(nextOccurrence(c.spec, c.after), c.want);
});

// --- PromptScheduler ---------------------------------------------------

interface Harness {
  scheduler: PromptScheduler;
  fired: AutomatedSessionInput[];
  owner: string;
}

function makeScheduler(
  stillLoading: () => Promise<boolean> = () => Promise.resolve(false),
): Harness {
  const fired: AutomatedSessionInput[] = [];
  const scheduler = new PromptScheduler((input) => {
    fired.push(input);
    return {
      id: `session-${fired.length}`,
      ownerClientId: input.ownerClientId,
      title: input.title,
      createdAtMs: Date.now(),
      updatedAtMs: Date.now(),
    };
  }, stillLoading);
  return { scheduler, fired, owner: createTestClient() };
}

function setNextRun(id: string, atMs: number): void {
  db().prepare(`UPDATE scheduled_prompts SET next_run_at_ms = ? WHERE id = ?`).run(atMs, id);
}

const weeklySpec: ScheduleSpec = {
  kind: "weekly",
  weekdays: [1],
  hour: 9,
  minute: 0,
};

Deno.test("scheduler: create arms a future run and CRUD stays owner-scoped", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, owner } = makeScheduler();
    try {
      const created = scheduler.create(owner, {
        title: "Weekly review",
        instruction: "Summarize my week.",
        schedule: weeklySpec,
        runMissed: false,
      });
      assert(created.enabled);
      assert((created.nextRunAtMs ?? 0) > Date.now());
      assertEquals(
        scheduler.list(owner).map((p) => p.id),
        [created.id],
      );

      const stranger = createTestClient("other-client");
      assertEquals(scheduler.list(stranger), []);
      assertThrows(() => scheduler.get(stranger, created.id), AppError, "not found");
      assertThrows(() => scheduler.delete(stranger, created.id), AppError, "not found");

      scheduler.delete(owner, created.id);
      assertEquals(scheduler.list(owner), []);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: rejects a once draft whose time has already passed", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, owner } = makeScheduler();
    try {
      assertThrows(
        () =>
          scheduler.create(owner, {
            title: "Past",
            instruction: "won't fire",
            schedule: { kind: "once", atMs: Date.now() - 60_000 },
            runMissed: false,
          }),
        AppError,
        "in the past",
      );
      assertEquals(scheduler.list(owner), []);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: runNow refuses a disabled schedule", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, fired, owner } = makeScheduler();
    try {
      const created = scheduler.create(owner, {
        title: "Weekly review",
        instruction: "Summarize my week.",
        schedule: weeklySpec,
        runMissed: false,
      });
      scheduler.update(owner, created.id, { enabled: false });
      await assertRejects(() => scheduler.runNow(owner, created.id), AppError, "disabled");
      assertEquals(fired.length, 0);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: update recomputes the next run only when it must", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, owner } = makeScheduler();
    try {
      const created = scheduler.create(owner, {
        title: "Weekly review",
        instruction: "Summarize my week.",
        schedule: weeklySpec,
        runMissed: false,
      });
      const armed = created.nextRunAtMs;

      // Title-only patch keeps the armed occurrence.
      const renamed = scheduler.update(owner, created.id, { title: "Review" });
      assertEquals(renamed.nextRunAtMs, armed);

      // Disabling parks the row.
      const disabled = scheduler.update(owner, created.id, { enabled: false });
      assertEquals(disabled.enabled, false);
      assertEquals(disabled.nextRunAtMs, undefined);

      // Re-enabling re-derives it.
      const enabled = scheduler.update(owner, created.id, { enabled: true });
      assert((enabled.nextRunAtMs ?? 0) > Date.now());

      // A new schedule re-derives it too.
      const oneOff: ScheduleSpec = {
        kind: "once",
        atMs: Date.now() + 60 * 60 * 1000,
      };
      const switched = scheduler.update(owner, created.id, {
        schedule: oneOff,
      });
      assertEquals(switched.nextRunAtMs, oneOff.atMs);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: tick fires due rows and honors the missed-run flag", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, fired, owner } = makeScheduler();
    try {
      const draft = (title: string, runMissed: boolean) =>
        scheduler.create(owner, {
          title,
          instruction: "Do the thing.",
          schedule: weeklySpec,
          runMissed,
        });
      const now = Date.now();
      // Barely late: inside the grace window, fires regardless of the flag.
      const recent = draft("Recent", false);
      setNextRun(recent.id, now - 60 * 1000);
      // An hour late with run-missed off: skipped.
      const skipped = draft("Skipped", false);
      setNextRun(skipped.id, now - 60 * 60 * 1000);
      // An hour late with run-missed on: made up once.
      const madeUp = draft("Made up", true);
      setNextRun(madeUp.id, now - 60 * 60 * 1000);

      await scheduler.tick();

      assertEquals(fired.map((f) => f.title).sort(), ["Made up", "Recent"]);
      assertEquals(fired[0].reason, "schedule");
      for (const id of [recent.id, skipped.id, madeUp.id]) {
        const row = scheduler.get(owner, id);
        assert((row.nextRunAtMs ?? 0) > now, `${row.title} re-armed in the future`);
        assertEquals(row.lastRunAtMs !== undefined, row.title !== "Skipped");
      }
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: a fired once-schedule goes dormant", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, fired, owner } = makeScheduler();
    try {
      const created = scheduler.create(owner, {
        title: "One reminder",
        instruction: "Remind me once.",
        schedule: { kind: "once", atMs: Date.now() + 60 * 60 * 1000 },
        runMissed: false,
      });
      // Simulate the armed occurrence coming due: a fired "once" always has
      // its atMs in the past at tick time, so rewrite the spec along with
      // the armed run instead of leaving atMs in the future.
      const pastAt = Date.now() - 1000;
      db()
        .prepare(`UPDATE scheduled_prompts SET schedule_json = ?, next_run_at_ms = ? WHERE id = ?`)
        .run(JSON.stringify({ kind: "once", atMs: pastAt }), pastAt, created.id);

      await scheduler.tick();

      assertEquals(fired.length, 1);
      const after = scheduler.get(owner, created.id);
      assertEquals(after.nextRunAtMs, undefined);
      assert(after.enabled);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: runNow fires immediately and keeps the armed run", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, fired, owner } = makeScheduler();
    try {
      const created = scheduler.create(owner, {
        title: "Weekly review",
        instruction: "Summarize my week.",
        schedule: weeklySpec,
        runMissed: false,
      });
      const session = await scheduler.runNow(owner, created.id);
      assertEquals(fired.length, 1);
      assertEquals(fired[0].scheduledPromptId, created.id);
      assertEquals(fired[0].focus, "show");
      assertEquals(session.title, "Weekly review");
      const after = scheduler.get(owner, created.id);
      assertEquals(after.nextRunAtMs, created.nextRunAtMs);
      assert(after.lastRunAtMs !== undefined);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: runNow refuses while the local model is loading", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, fired, owner } = makeScheduler(() => Promise.resolve(true));
    try {
      const created = scheduler.create(owner, {
        title: "Weekly review",
        instruction: "Summarize my week.",
        schedule: weeklySpec,
        runMissed: false,
      });
      await assertRejects(() => scheduler.runNow(owner, created.id), AppError, "still loading");
      assertEquals(fired.length, 0);
      // The armed occurrence is untouched, so the next tick can still fire it.
      assertEquals(scheduler.get(owner, created.id).lastRunAtMs, undefined);
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("scheduler: start() catches up an overdue row through the timer", async () => {
  const env = await setupTestEnv();
  try {
    const { scheduler, fired, owner } = makeScheduler();
    try {
      const created = scheduler.create(owner, {
        title: "Catch up",
        instruction: "Run me.",
        schedule: weeklySpec,
        runMissed: true,
      });
      setNextRun(created.id, Date.now() - 60 * 60 * 1000);

      scheduler.start();
      // The overdue row arms a zero-delay timer; give the async tick a beat.
      await new Promise((r) => setTimeout(r, 100));

      assertEquals(
        fired.map((f) => f.title),
        ["Catch up"],
      );
      assert((scheduler.get(owner, created.id).nextRunAtMs ?? 0) > Date.now());
    } finally {
      scheduler.dispose();
    }
  } finally {
    await env.teardown();
  }
});

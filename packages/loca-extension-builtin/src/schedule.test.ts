// schedule_prompt over a scripted ctx.schedulePrompt: schedule assembly
// from the flat arguments, validation errors, and outcome reporting.

import { assertEquals, assertRejects } from "@std/assert";
import { schedulePrompt } from "./schedule.ts";
import type { ScheduledPromptDraft, ToolContext } from "./types.ts";

interface MockCtx extends ToolContext {
  /** Replay log: every draft proposed via ctx.schedulePrompt. */
  proposed: ScheduledPromptDraft[];
}

function makeMockCtx(
  outcome: (draft: ScheduledPromptDraft) => { accepted: boolean; draft?: ScheduledPromptDraft },
): MockCtx {
  const ctx = {
    proposed: [],
    setProgress() {},
    askUser: () => Promise.resolve([]),
    log() {},
    display: { markdown() {}, image() {}, table() {}, diff() {} },
    memories: {
      list: () => Promise.resolve([]),
      get: () => Promise.reject(new Error("not scripted")),
      write: () => Promise.reject(new Error("not scripted")),
      edit: () => Promise.reject(new Error("not scripted")),
    },
    db: {
      query: () => Promise.reject(new Error("not scripted")),
      execute: () => Promise.reject(new Error("not scripted")),
    },
    llm: { complete: () => Promise.reject(new Error("not scripted")) },
    tts: { speak: () => Promise.reject(new Error("not scripted")) },
    stt: { transcribe: () => Promise.reject(new Error("not scripted")) },
    schedulePrompt(draft: ScheduledPromptDraft) {
      ctx.proposed.push(draft);
      return Promise.resolve(outcome(draft));
    },
    signal: new AbortController().signal,
    getChatContext() {
      return { userMessage: "", sessionId: null };
    },
  } as MockCtx;
  return ctx;
}

const accept = (draft: ScheduledPromptDraft) => ({ accepted: true, draft });

Deno.test("schedule_prompt: assembles a weekly draft and reports the saved one", async () => {
  const ctx = makeMockCtx(accept);
  const result = await schedulePrompt(
    {
      title: "Timesheet",
      instruction: "Remind me to fill in the timesheet.",
      kind: "weekly",
      weekdays: [1, 5],
      hour: 9,
      minute: 0,
      runMissed: true,
    },
    ctx,
  );
  assertEquals(ctx.proposed, [
    {
      title: "Timesheet",
      instruction: "Remind me to fill in the timesheet.",
      schedule: { kind: "weekly", weekdays: [1, 5], hour: 9, minute: 0 },
      runMissed: true,
    },
  ]);
  assertEquals(result.scheduled, true);
  assertEquals(result.draft?.title, "Timesheet");
});

Deno.test("schedule_prompt: builds once/interval/monthly/yearly specs", async () => {
  const ctx = makeMockCtx(accept);
  await schedulePrompt(
    {
      title: "T",
      instruction: "I",
      kind: "once",
      at: "2123-06-15 09:30",
    },
    ctx,
  );
  await schedulePrompt(
    {
      title: "T",
      instruction: "I",
      kind: "interval",
      everyMinutes: 45,
    },
    ctx,
  );
  await schedulePrompt(
    {
      title: "T",
      instruction: "I",
      kind: "monthly",
      day: 31,
      hour: 8,
      minute: 15,
    },
    ctx,
  );
  await schedulePrompt(
    {
      title: "T",
      instruction: "I",
      kind: "yearly",
      month: 2,
      day: 29,
      hour: 0,
      minute: 0,
    },
    ctx,
  );
  assertEquals(
    ctx.proposed.map((d) => d.schedule),
    [
      { kind: "once", atMs: new Date(2123, 5, 15, 9, 30, 0, 0).getTime() },
      { kind: "interval", everyMinutes: 45 },
      { kind: "monthly", day: 31, hour: 8, minute: 15 },
      { kind: "yearly", month: 2, day: 29, hour: 0, minute: 0 },
    ],
  );
  // runMissed defaults to false when absent.
  assertEquals(
    ctx.proposed.every((d) => d.runMissed === false),
    true,
  );
});

Deno.test("schedule_prompt: rejects bad arguments before proposing", async () => {
  const ctx = makeMockCtx(accept);
  await assertRejects(
    () => schedulePrompt({ title: "T", instruction: "I", kind: "later" }, ctx),
    Error,
    "once, interval, weekly, monthly, yearly",
  );
  await assertRejects(
    () =>
      schedulePrompt(
        {
          title: "T",
          instruction: "I",
          kind: "once",
          at: "tomorrow",
        },
        ctx,
      ),
    Error,
    "local date-time",
  );
  await assertRejects(
    () =>
      schedulePrompt(
        {
          title: "T",
          instruction: "I",
          kind: "once",
          at: "2020-01-01 09:00",
        },
        ctx,
      ),
    Error,
    "in the past",
  );
  await assertRejects(
    () =>
      schedulePrompt(
        {
          title: "T",
          instruction: "I",
          kind: "weekly",
          weekdays: [],
        },
        ctx,
      ),
    Error,
    "weekdays",
  );
  await assertRejects(
    () =>
      schedulePrompt(
        {
          title: "T",
          instruction: "I",
          kind: "weekly",
          weekdays: [1],
          hour: 24,
          minute: 0,
        },
        ctx,
      ),
    Error,
    '"hour"',
  );
  assertEquals(ctx.proposed, []);
});

Deno.test("schedule_prompt: rejects out-of-range / non-existent once dates", async () => {
  const ctx = makeMockCtx(accept);
  for (const at of [
    "2123-13-01 09:00", // month 13
    "2123-00-10 09:00", // month 0
    "2123-02-30 09:00", // Feb 30 (would silently roll into March)
    "2123-04-31 09:00", // April has 30 days
    "2123-06-15 24:00", // hour 24
    "2123-06-15 09:60", // minute 60
  ]) {
    await assertRejects(
      () => schedulePrompt({ title: "T", instruction: "I", kind: "once", at }, ctx),
      Error,
      "local date-time",
      `should reject ${at}`,
    );
  }
  assertEquals(ctx.proposed, []);
});

Deno.test("schedule_prompt: a declined proposal reports scheduled: false", async () => {
  const ctx = makeMockCtx(() => ({ accepted: false }));
  const result = await schedulePrompt(
    { title: "T", instruction: "I", kind: "interval", everyMinutes: 60 },
    ctx,
  );
  assertEquals(result, { scheduled: false });
  assertEquals(ctx.proposed.length, 1);
});

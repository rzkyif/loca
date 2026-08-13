// schedule_prompt: assemble a ScheduledPromptDraft from the model's flat
// arguments and hand it to ctx.schedulePrompt, which pauses on the host's
// editable in-chat confirm form. The form is the consent gate; the tool
// just reports whether (and how) the schedule was saved.

import type { ScheduledPromptDraft, ScheduleSpec, ToolContext } from "./types.ts";

function stringArg(args: Record<string, unknown>, key: string): string {
  const value = args[key];
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`missing required string argument "${key}"`);
  }
  return value;
}

function intArg(args: Record<string, unknown>, key: string, min: number, max?: number): number {
  const value = args[key];
  if (
    typeof value !== "number" ||
    !Number.isInteger(value) ||
    value < min ||
    (max !== undefined && value > max)
  ) {
    const range = max !== undefined ? `${min} to ${max}` : `${min} or more`;
    throw new Error(`argument "${key}" must be an integer (${range})`);
  }
  return value;
}

/** Parse "YYYY-MM-DD HH:MM" (or with a "T" separator) as a local instant.
 *  Returns null on a malformed string, an out-of-range field, or a date that
 *  doesn't exist (e.g. 2026-02-30): without the round-trip check, Date would
 *  silently roll those over to a valid-but-wrong instant. */
function parseLocalDateTime(text: string): number | null {
  const m = text.trim().match(/^(\d{4})-(\d{2})-(\d{2})[T ](\d{1,2}):(\d{2})$/);
  if (!m) return null;
  const [year, month, day, hour, minute] = [+m[1], +m[2], +m[3], +m[4], +m[5]];
  if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59) return null;
  const d = new Date(year, month - 1, day, hour, minute, 0, 0);
  // Reject any field Date had to normalize (rollover), so a non-existent
  // calendar date doesn't become a different real one.
  if (
    d.getFullYear() !== year ||
    d.getMonth() !== month - 1 ||
    d.getDate() !== day ||
    d.getHours() !== hour ||
    d.getMinutes() !== minute
  ) {
    return null;
  }
  const t = d.getTime();
  return Number.isFinite(t) ? t : null;
}

function buildSchedule(args: Record<string, unknown>): ScheduleSpec {
  const kind = stringArg(args, "kind");
  switch (kind) {
    case "once": {
      const at = stringArg(args, "at");
      const atMs = parseLocalDateTime(at);
      if (atMs === null) {
        throw new Error('argument "at" must be a local date-time like "2026-06-15 09:00"');
      }
      if (atMs <= Date.now()) {
        throw new Error(`the "at" date-time (${at}) is already in the past`);
      }
      return { kind: "once", atMs };
    }
    case "interval":
      return {
        kind: "interval",
        everyMinutes: intArg(args, "everyMinutes", 1),
      };
    case "weekly": {
      const weekdays = args.weekdays;
      if (
        !Array.isArray(weekdays) ||
        weekdays.length === 0 ||
        !weekdays.every((d) => Number.isInteger(d) && d >= 0 && d <= 6)
      ) {
        throw new Error(
          'argument "weekdays" must be a non-empty array of integers 0 (Sunday) to 6 (Saturday)',
        );
      }
      return {
        kind: "weekly",
        weekdays: weekdays as number[],
        hour: intArg(args, "hour", 0, 23),
        minute: intArg(args, "minute", 0, 59),
      };
    }
    case "monthly":
      return {
        kind: "monthly",
        day: intArg(args, "day", 1, 31),
        hour: intArg(args, "hour", 0, 23),
        minute: intArg(args, "minute", 0, 59),
      };
    case "yearly":
      return {
        kind: "yearly",
        month: intArg(args, "month", 1, 12),
        day: intArg(args, "day", 1, 31),
        hour: intArg(args, "hour", 0, 23),
        minute: intArg(args, "minute", 0, 59),
      };
    default:
      throw new Error('argument "kind" must be one of once, interval, weekly, monthly, yearly');
  }
}

export async function schedulePrompt(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ scheduled: boolean; draft?: ScheduledPromptDraft }> {
  const draft: ScheduledPromptDraft = {
    title: stringArg(args, "title"),
    instruction: stringArg(args, "instruction"),
    schedule: buildSchedule(args),
    runMissed: args.runMissed === true,
  };
  const outcome = await ctx.schedulePrompt(draft);
  if (!outcome.accepted) {
    ctx.setProgress(1, "Schedule declined");
    return { scheduled: false };
  }
  ctx.setProgress(1, "Prompt scheduled", outcome.draft?.title);
  return { scheduled: true, draft: outcome.draft };
}

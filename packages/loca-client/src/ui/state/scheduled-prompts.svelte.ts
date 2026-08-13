/**
 * Reactive mirror of the active core's scheduled prompt list for the
 * Scheduled Prompts settings manager. The schedules themselves arm and run
 * core-side; the client just lists and edits them over REST.
 */

import type { ScheduledPrompt, ScheduledPromptDraft, ScheduleSpec } from "@loca/shared";
import type { ScheduledPromptPatch } from "$lib/core/scheduled-prompts";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";

const log = getLogger("scheduled-prompts");

const WEEKDAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_NAMES = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

function timeText(hour: number, minute: number): string {
  return `${pad(hour)}:${pad(minute)}`;
}

function dateText(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${timeText(
    d.getHours(),
    d.getMinutes(),
  )}`;
}

/** One-line description of when a schedule fires, for cards and headers. */
export function describeSchedule(spec: ScheduleSpec): string {
  switch (spec.kind) {
    case "once":
      return `Once on ${dateText(spec.atMs)}`;
    case "interval":
      return spec.everyMinutes === 1 ? "Every minute" : `Every ${spec.everyMinutes} minutes`;
    case "weekly": {
      const days = [...spec.weekdays]
        .sort((a, b) => a - b)
        .map((d) => WEEKDAY_NAMES[d] ?? String(d));
      return `Weekly on ${days.join(", ")} at ${timeText(spec.hour, spec.minute)}`;
    }
    case "monthly":
      return `Monthly on day ${spec.day} at ${timeText(spec.hour, spec.minute)}`;
    case "yearly":
      return `Yearly on ${MONTH_NAMES[spec.month - 1] ?? spec.month} ${spec.day} at ${timeText(
        spec.hour,
        spec.minute,
      )}`;
  }
}

/** Short status line for a schedule's card: next armed run, or why none. */
export function nextRunText(p: ScheduledPrompt): string {
  if (!p.enabled) return "Off";
  if (p.nextRunAtMs === undefined) return "Done";
  return `Next ${dateText(p.nextRunAtMs)}`;
}

export function lastRunText(p: ScheduledPrompt): string | null {
  return p.lastRunAtMs === undefined ? null : `Last ran ${dateText(p.lastRunAtMs)}`;
}

class ScheduledPromptsState {
  prompts = $state<ScheduledPrompt[]>([]);

  private subs = new Subscriptions();

  /** Subscribe to the active core's connection state and (re)load the list on
   *  every connected edge, so the manager opens onto fresh rows instead of an
   *  empty store. Idempotent, mirroring memoriesState.attach(). */
  attach(): void {
    this.subs.attach(() => [
      cores().subscribeConnectionState((state) => {
        if (state === "connected") {
          void this.load().catch((err) =>
            log.warn("scheduled prompt load on ws connect failed:", err),
          );
        }
      }),
    ]);
  }

  async load(): Promise<void> {
    this.prompts = await cores().api().scheduledPrompts.list();
  }

  async create(draft: ScheduledPromptDraft): Promise<ScheduledPrompt> {
    const created = await cores().api().scheduledPrompts.create(draft);
    await this.load();
    return created;
  }

  async update(id: string, patch: ScheduledPromptPatch): Promise<ScheduledPrompt> {
    const updated = await cores().api().scheduledPrompts.update(id, patch);
    await this.load();
    return updated;
  }

  async delete(id: string): Promise<void> {
    await cores().api().scheduledPrompts.delete(id);
    this.prompts = this.prompts.filter((p) => p.id !== id);
  }

  /** Fire a schedule immediately; the run lands as a new session. */
  async run(id: string): Promise<{ sessionId: string }> {
    const result = await cores().api().scheduledPrompts.run(id);
    await this.load();
    return result;
  }
}

export const scheduledPromptsState = new ScheduledPromptsState();

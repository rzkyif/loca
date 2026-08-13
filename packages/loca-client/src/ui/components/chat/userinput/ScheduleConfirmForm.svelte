<!-- Live wrapper for the pre-save Scheduled Prompt confirm card. Owns the
     schedule serialization (decomposing the draft's ScheduleSpec into the flat
     fields the editor renders, and turning the editor's raw input/click
     callbacks back into a valid spec); ScheduleConfirmFormView renders the
     card. -->
<script lang="ts">
  import { untrack } from "svelte";
  import type { ScheduledPromptDraft, ScheduleSpec } from "@loca/shared";
  import ScheduleConfirmFormView from "@loca/shared/ui/components/chat/userinput/ScheduleConfirmFormView.svelte";

  let {
    draft,
    onChange,
  }: {
    draft: ScheduledPromptDraft;
    onChange: (next: ScheduledPromptDraft) => void;
  } = $props();

  const KIND_OPTIONS = [
    { value: "once", label: "Once" },
    { value: "interval", label: "Interval" },
    { value: "weekly", label: "Weekly" },
    { value: "monthly", label: "Monthly" },
    { value: "yearly", label: "Yearly" },
  ];

  const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

  const MONTH_OPTIONS = [
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
  ].map((label, i) => ({ value: i + 1, label }));

  function pad(n: number): string {
    return String(n).padStart(2, "0");
  }

  /** Epoch ms -> the local "YYYY-MM-DDTHH:MM" a datetime-local input wants. */
  function toLocalInput(ms: number): string {
    const d = new Date(ms);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(
      d.getMinutes(),
    )}`;
  }

  function fromLocalInput(text: string): number | null {
    const m = text.match(/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/);
    if (!m) return null;
    const t = new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], 0, 0).getTime();
    return Number.isFinite(t) ? t : null;
  }

  /** Hour/minute carried between kinds so switching doesn't lose the time. */
  function timeOf(spec: ScheduleSpec): { hour: number; minute: number } {
    if (spec.kind === "weekly" || spec.kind === "monthly" || spec.kind === "yearly") {
      return { hour: spec.hour, minute: spec.minute };
    }
    if (spec.kind === "once") {
      const d = new Date(spec.atMs);
      return { hour: d.getHours(), minute: d.getMinutes() };
    }
    return { hour: 9, minute: 0 };
  }

  function setSchedule(schedule: ScheduleSpec): void {
    onChange({ ...draft, schedule });
  }

  function switchKind(kind: string): void {
    const schedule = draft.schedule;
    if (kind === schedule.kind) return;
    const { hour, minute } = timeOf(schedule);
    switch (kind) {
      case "once": {
        // Default to this time tomorrow so the spec starts out in the future.
        const now = new Date();
        const atMs = new Date(
          now.getFullYear(),
          now.getMonth(),
          now.getDate() + 1,
          hour,
          minute,
          0,
          0,
        ).getTime();
        setSchedule({ kind: "once", atMs });
        return;
      }
      case "interval":
        setSchedule({ kind: "interval", everyMinutes: 60 });
        return;
      case "weekly":
        setSchedule({ kind: "weekly", weekdays: [1], hour, minute });
        return;
      case "monthly":
        setSchedule({ kind: "monthly", day: 1, hour, minute });
        return;
      case "yearly":
        setSchedule({ kind: "yearly", month: 1, day: 1, hour, minute });
        return;
    }
  }

  function toggleWeekday(day: number): void {
    const schedule = draft.schedule;
    if (schedule.kind !== "weekly") return;
    const has = schedule.weekdays.includes(day);
    // Keep at least one weekday selected so the spec stays valid.
    if (has && schedule.weekdays.length === 1) return;
    const weekdays = has
      ? schedule.weekdays.filter((d) => d !== day)
      : [...schedule.weekdays, day].sort((a, b) => a - b);
    setSchedule({ ...schedule, weekdays });
  }

  function setTime(text: string): void {
    const schedule = draft.schedule;
    if (schedule.kind === "once" || schedule.kind === "interval") return;
    const m = text.match(/^(\d{2}):(\d{2})/);
    if (!m) return;
    setSchedule({ ...schedule, hour: +m[1], minute: +m[2] });
  }

  function setIntField(key: "day" | "everyMinutes", raw: string, min: number, max: number): void {
    const n = Math.round(Number(raw));
    if (!Number.isFinite(n)) return;
    setSchedule({ ...draft.schedule, [key]: Math.min(max, Math.max(min, n)) } as ScheduleSpec);
  }

  function setWhen(text: string): void {
    const ms = fromLocalInput(text);
    if (ms !== null) setSchedule({ kind: "once", atMs: ms });
  }

  // The View's text fields + toggle are $bindable; mirror them in local state
  // synced from the draft, and push writes back out through onChange so the
  // parent stays the source of truth.
  let title = $state(untrack(() => draft.title));
  let instruction = $state(untrack(() => draft.instruction));
  let runMissed = $state(untrack(() => draft.runMissed));
  $effect(() => {
    title = draft.title;
    instruction = draft.instruction;
    runMissed = draft.runMissed;
  });
  $effect(() => {
    if (
      title !== draft.title ||
      instruction !== draft.instruction ||
      runMissed !== draft.runMissed
    ) {
      onChange({ ...draft, title, instruction, runMissed });
    }
  });
</script>

<ScheduleConfirmFormView
  kind={draft.schedule.kind}
  kindOptions={KIND_OPTIONS}
  weekdayLabels={WEEKDAYS}
  monthOptions={MONTH_OPTIONS}
  whenLocal={draft.schedule.kind === "once" ? toLocalInput(draft.schedule.atMs) : undefined}
  everyMinutes={draft.schedule.kind === "interval" ? draft.schedule.everyMinutes : undefined}
  weekdays={draft.schedule.kind === "weekly" ? draft.schedule.weekdays : []}
  monthlyDay={draft.schedule.kind === "monthly" ? draft.schedule.day : undefined}
  yearlyMonth={draft.schedule.kind === "yearly" ? draft.schedule.month : undefined}
  yearlyDay={draft.schedule.kind === "yearly" ? draft.schedule.day : undefined}
  timeText={draft.schedule.kind === "weekly" ||
  draft.schedule.kind === "monthly" ||
  draft.schedule.kind === "yearly"
    ? `${pad(draft.schedule.hour)}:${pad(draft.schedule.minute)}`
    : undefined}
  onKindChange={switchKind}
  onWhenChange={setWhen}
  onEveryMinutesChange={(v) => setIntField("everyMinutes", v, 1, 10080)}
  onToggleWeekday={toggleWeekday}
  onMonthlyDayChange={(v) => setIntField("day", v, 1, 31)}
  onYearlyMonthChange={(v) => setSchedule({ ...draft.schedule, month: Number(v) } as ScheduleSpec)}
  onYearlyDayChange={(v) => setIntField("day", v, 1, 31)}
  onTimeChange={setTime}
  bind:title
  bind:instruction
  bind:runMissed
/>

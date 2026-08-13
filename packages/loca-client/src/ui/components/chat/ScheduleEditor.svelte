<!-- Editor for one ScheduleSpec: a kind select plus the fields that kind
     needs. Controlled component (value + onchange), shared by the in-chat
     schedule confirm form and the Scheduled Prompt detail view in Settings.
     Edits always produce a structurally valid spec; the one thing a parent
     may still want to check is a "once" date-time that is already past.

     This shell owns all schedule serialization and the kind-switch logic; the
     pure ScheduleEditorView renders the decomposed draft fields. -->
<script lang="ts">
  import type { ScheduleSpec } from "@loca/shared";
  import ScheduleEditorView from "@loca/shared/ui/components/chat/ScheduleEditorView.svelte";

  let {
    schedule,
    onchange,
    disabled = false,
  }: {
    schedule: ScheduleSpec;
    onchange: (next: ScheduleSpec) => void;
    disabled?: boolean;
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

  function switchKind(kind: string): void {
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
        onchange({ kind: "once", atMs });
        return;
      }
      case "interval":
        onchange({ kind: "interval", everyMinutes: 60 });
        return;
      case "weekly":
        onchange({ kind: "weekly", weekdays: [1], hour, minute });
        return;
      case "monthly":
        onchange({ kind: "monthly", day: 1, hour, minute });
        return;
      case "yearly":
        onchange({ kind: "yearly", month: 1, day: 1, hour, minute });
        return;
    }
  }

  function toggleWeekday(day: number): void {
    if (schedule.kind !== "weekly") return;
    const has = schedule.weekdays.includes(day);
    // Keep at least one weekday selected so the spec stays valid.
    if (has && schedule.weekdays.length === 1) return;
    const weekdays = has
      ? schedule.weekdays.filter((d) => d !== day)
      : [...schedule.weekdays, day].sort((a, b) => a - b);
    onchange({ ...schedule, weekdays });
  }

  function setTime(text: string): void {
    if (schedule.kind === "once" || schedule.kind === "interval") return;
    const m = text.match(/^(\d{2}):(\d{2})/);
    if (!m) return;
    onchange({ ...schedule, hour: +m[1], minute: +m[2] });
  }

  function setIntField(key: "day" | "everyMinutes", raw: string, min: number, max: number): void {
    const n = Math.round(Number(raw));
    if (!Number.isFinite(n)) return;
    onchange({ ...schedule, [key]: Math.min(max, Math.max(min, n)) } as ScheduleSpec);
  }

  function setWhen(text: string): void {
    const ms = fromLocalInput(text);
    if (ms !== null) onchange({ kind: "once", atMs: ms });
  }
</script>

<ScheduleEditorView
  kind={schedule.kind}
  kindOptions={KIND_OPTIONS}
  weekdayLabels={WEEKDAYS}
  monthOptions={MONTH_OPTIONS}
  whenLocal={schedule.kind === "once" ? toLocalInput(schedule.atMs) : undefined}
  everyMinutes={schedule.kind === "interval" ? schedule.everyMinutes : undefined}
  weekdays={schedule.kind === "weekly" ? schedule.weekdays : []}
  monthlyDay={schedule.kind === "monthly" ? schedule.day : undefined}
  yearlyMonth={schedule.kind === "yearly" ? schedule.month : undefined}
  yearlyDay={schedule.kind === "yearly" ? schedule.day : undefined}
  timeText={schedule.kind === "weekly" || schedule.kind === "monthly" || schedule.kind === "yearly"
    ? `${pad(schedule.hour)}:${pad(schedule.minute)}`
    : undefined}
  {disabled}
  onKindChange={switchKind}
  onWhenChange={setWhen}
  onEveryMinutesChange={(v) => setIntField("everyMinutes", v, 1, 10080)}
  onToggleWeekday={toggleWeekday}
  onMonthlyDayChange={(v) => setIntField("day", v, 1, 31)}
  onYearlyMonthChange={(v) => onchange({ ...schedule, month: Number(v) } as ScheduleSpec)}
  onYearlyDayChange={(v) => setIntField("day", v, 1, 31)}
  onTimeChange={setTime}
/>

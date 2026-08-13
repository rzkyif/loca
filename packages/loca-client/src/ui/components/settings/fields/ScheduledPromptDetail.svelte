<!-- Live shell for one scheduled prompt's detail pane. Owns the draft store, the
     debounced save, the run-now action, the run-status date formatting, and the
     ScheduleSpec serialization; feeds it all into the pure
     ScheduledPromptDetailView, which renders the form and the embedded schedule
     editor. No bespoke markup lives here. -->
<script lang="ts">
  import { untrack } from "svelte";
  import type { ScheduledPrompt, ScheduleSpec } from "@loca/shared";
  import { scheduledPromptsState } from "$stores";
  import { lastRunText, nextRunText } from "$stores/scheduled-prompts.svelte";
  import { getLogger } from "$lib/util/log";
  import { createDebouncedSave } from "$lib/util/debounced-save";
  import ScheduledPromptDetailView from "@loca/shared/ui/components/settings/ScheduledPromptDetailView.svelte";

  const log = getLogger("scheduled-prompts");

  // `reload` refreshes the list behind the detail so the card reflects edits.
  let { item, reload }: { item: ScheduledPrompt; reload: () => void } = $props();

  // One-time snapshot of the opened schedule; later store updates must not
  // clobber an in-progress edit, so these are intentionally not derived.
  let draftTitle = $state(untrack(() => item.title));
  let draftInstruction = $state(untrack(() => item.instruction));
  // `item` is a reactive store proxy when opened from the manager, so its
  // schedule is deep Proxy state; structuredClone would throw DataCloneError on
  // it. $state.snapshot returns a plain editable clone (the same call the chat
  // schedule-confirm draft uses).
  let draftSchedule = $state<ScheduleSpec>(untrack(() => $state.snapshot(item.schedule)));
  let draftRunMissed = $state(untrack(() => item.runMissed));
  let draftEnabled = $state(untrack(() => item.enabled));

  // Run bookkeeping (next/last run) updates server-side on every save, so
  // read it from the live store row rather than the opening snapshot.
  const live = $derived(scheduledPromptsState.prompts.find((p) => p.id === item.id) ?? item);

  const titleError = $derived(draftTitle.trim() ? null : "Title cannot be empty");
  const instructionError = $derived(draftInstruction.trim() ? null : "Prompt cannot be empty");

  const { scheduleSave, flushSave } = createDebouncedSave(async () => {
    if (titleError || instructionError) return;
    try {
      await scheduledPromptsState.update(item.id, {
        title: draftTitle.trim(),
        instruction: draftInstruction,
        schedule: $state.snapshot(draftSchedule),
        runMissed: draftRunMissed,
        enabled: draftEnabled,
      });
      reload();
    } catch (e) {
      log.error("Failed to save scheduled prompt:", e);
    }
  });

  async function runNow() {
    await flushSave();
    try {
      await scheduledPromptsState.run(item.id);
      reload();
    } catch (e) {
      log.error("Failed to run scheduled prompt:", e);
    }
  }

  // --- Schedule serialization (decompose a ScheduleSpec into flat editor
  //     fields, then turn the editor's raw callbacks back into a valid spec). ---
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

  function setSchedule(next: ScheduleSpec): void {
    draftSchedule = next;
    scheduleSave();
  }

  function switchKind(kind: string): void {
    if (kind === draftSchedule.kind) return;
    const { hour, minute } = timeOf(draftSchedule);
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
    if (draftSchedule.kind !== "weekly") return;
    const has = draftSchedule.weekdays.includes(day);
    // Keep at least one weekday selected so the spec stays valid.
    if (has && draftSchedule.weekdays.length === 1) return;
    const weekdays = has
      ? draftSchedule.weekdays.filter((d) => d !== day)
      : [...draftSchedule.weekdays, day].sort((a, b) => a - b);
    setSchedule({ ...draftSchedule, weekdays });
  }

  function setTime(text: string): void {
    if (draftSchedule.kind === "once" || draftSchedule.kind === "interval") return;
    const m = text.match(/^(\d{2}):(\d{2})/);
    if (!m) return;
    setSchedule({ ...draftSchedule, hour: +m[1], minute: +m[2] });
  }

  function setIntField(key: "day" | "everyMinutes", raw: string, min: number, max: number): void {
    const n = Math.round(Number(raw));
    if (!Number.isFinite(n)) return;
    setSchedule({ ...draftSchedule, [key]: Math.min(max, Math.max(min, n)) } as ScheduleSpec);
  }

  function setWhen(text: string): void {
    const ms = fromLocalInput(text);
    if (ms !== null) setSchedule({ kind: "once", atMs: ms });
  }
</script>

<ScheduledPromptDetailView
  nextRunText={nextRunText(live)}
  lastRunText={lastRunText(live) ?? ""}
  enabled={draftEnabled}
  runMissed={draftRunMissed}
  {draftTitle}
  {titleError}
  {draftInstruction}
  {instructionError}
  kind={draftSchedule.kind}
  kindOptions={KIND_OPTIONS}
  weekdayLabels={WEEKDAYS}
  monthOptions={MONTH_OPTIONS}
  whenLocal={draftSchedule.kind === "once" ? toLocalInput(draftSchedule.atMs) : undefined}
  everyMinutes={draftSchedule.kind === "interval" ? draftSchedule.everyMinutes : undefined}
  weekdays={draftSchedule.kind === "weekly" ? draftSchedule.weekdays : []}
  monthlyDay={draftSchedule.kind === "monthly" ? draftSchedule.day : undefined}
  yearlyMonth={draftSchedule.kind === "yearly" ? draftSchedule.month : undefined}
  yearlyDay={draftSchedule.kind === "yearly" ? draftSchedule.day : undefined}
  timeText={draftSchedule.kind === "weekly" ||
  draftSchedule.kind === "monthly" ||
  draftSchedule.kind === "yearly"
    ? `${pad(draftSchedule.hour)}:${pad(draftSchedule.minute)}`
    : undefined}
  onRunNow={() => void runNow()}
  onToggleEnabled={(v) => {
    draftEnabled = v;
    void flushSave();
  }}
  onToggleRunMissed={(v) => {
    draftRunMissed = v;
    void flushSave();
  }}
  onTitleInput={(v) => {
    draftTitle = v;
    scheduleSave();
  }}
  onTitleBlur={() => flushSave()}
  onInstructionInput={(v) => {
    draftInstruction = v;
    scheduleSave();
  }}
  onInstructionBlur={() => flushSave()}
  onKindChange={switchKind}
  onWhenChange={setWhen}
  onEveryMinutesChange={(v) => setIntField("everyMinutes", v, 1, 10080)}
  onToggleWeekday={toggleWeekday}
  onMonthlyDayChange={(v) => setIntField("day", v, 1, 31)}
  onYearlyMonthChange={(v) => setSchedule({ ...draftSchedule, month: Number(v) } as ScheduleSpec)}
  onYearlyDayChange={(v) => setIntField("day", v, 1, 31)}
  onTimeChange={setTime}
/>

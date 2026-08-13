<script lang="ts">
  // Presentational body of a scheduled prompt's detail pane: a run-status header
  // (next-run / last-run text plus the Run Now button and enable toggle), the
  // title and prompt fields, the embedded schedule editor, and the "run if
  // missed" labeled toggle. All values arrive pre-resolved: the client owns the
  // draft store, the date formatting (the run-status arrives as ready display
  // strings), the schedule serialization (the flat schedule props are decomposed
  // by the client and fed straight into the embedded ScheduleEditorView), and the
  // save wiring. So this stays pure: props in, callbacks out. `draftTitle`,
  // `draftInstruction`, and the schedule fields are not bindable; the client
  // re-feeds them on every change via the on* callbacks.
  import FormField from "../primitives/FormField.svelte";
  import Input from "../primitives/Input.svelte";
  import Textarea from "../primitives/Textarea.svelte";
  import Toggle from "../primitives/Toggle.svelte";
  import Button from "../primitives/Button.svelte";
  import ScheduleEditorView from "../chat/ScheduleEditorView.svelte";

  type OptionValue = string | number;
  type Option = { value: OptionValue; label: string };

  let {
    nextRunText = "",
    lastRunText = "",
    enabled = false,
    runMissed = false,
    draftTitle = "",
    titleError = null,
    draftInstruction = "",
    instructionError = null,
    // Flat schedule fields, forwarded straight to the embedded editor.
    kind,
    kindOptions,
    weekdayLabels,
    monthOptions,
    whenLocal,
    everyMinutes,
    weekdays = [],
    monthlyDay,
    yearlyMonth,
    yearlyDay,
    timeText,
    onRunNow,
    onToggleEnabled,
    onToggleRunMissed,
    onTitleInput,
    onTitleBlur,
    onInstructionInput,
    onInstructionBlur,
    onKindChange,
    onWhenChange,
    onEveryMinutesChange,
    onToggleWeekday,
    onMonthlyDayChange,
    onYearlyMonthChange,
    onYearlyDayChange,
    onTimeChange,
  }: {
    nextRunText?: string;
    lastRunText?: string;
    enabled?: boolean;
    runMissed?: boolean;
    draftTitle?: string;
    titleError?: string | null;
    draftInstruction?: string;
    instructionError?: string | null;
    kind: string;
    kindOptions: Option[];
    weekdayLabels: string[];
    monthOptions: Option[];
    whenLocal?: string;
    everyMinutes?: number;
    weekdays?: number[];
    monthlyDay?: number;
    yearlyMonth?: number;
    yearlyDay?: number;
    timeText?: string;
    onRunNow?: () => void;
    onToggleEnabled?: (enabled: boolean) => void;
    onToggleRunMissed?: (runMissed: boolean) => void;
    onTitleInput?: (v: string) => void;
    onTitleBlur?: () => void;
    onInstructionInput?: (v: string) => void;
    onInstructionBlur?: () => void;
    onKindChange?: (kind: string) => void;
    onWhenChange?: (text: string) => void;
    onEveryMinutesChange?: (v: string) => void;
    onToggleWeekday?: (day: number) => void;
    onMonthlyDayChange?: (v: string) => void;
    onYearlyMonthChange?: (v: string) => void;
    onYearlyDayChange?: (v: string) => void;
    onTimeChange?: (text: string) => void;
  } = $props();

  const noop = (): void => {};
</script>

<div class="flex flex-col gap-3">
  <div class="flex items-center justify-between gap-2">
    <div class="flex flex-col text-xs text-default-600">
      <span>{nextRunText}</span>
      {#if lastRunText}
        <span>{lastRunText}</span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      <div class="w-24 shrink-0">
        <Button class="w-full h-8" onclick={() => (onRunNow ?? noop)()}>Run Now</Button>
      </div>
      <div class="w-24 shrink-0">
        <Toggle
          checked={enabled}
          ariaLabel="Enable schedule"
          onchange={(v) => (onToggleEnabled ?? noop)(v)}
        />
      </div>
    </div>
  </div>

  <FormField label="Title" error={titleError}>
    <Input
      type="text"
      value={draftTitle}
      ariaLabel="Scheduled prompt title"
      error={!!titleError}
      oninput={(v) => (onTitleInput ?? noop)(v)}
      onblur={() => (onTitleBlur ?? noop)()}
    />
  </FormField>

  <FormField label="Prompt" error={instructionError}>
    <Textarea
      ariaLabel="Scheduled prompt instruction"
      autoResize="none"
      class="max-h-60 min-h-24 overflow-y-auto resize-none"
      value={draftInstruction}
      error={!!instructionError}
      oninput={(v) => (onInstructionInput ?? noop)(v)}
      onblur={() => (onInstructionBlur ?? noop)()}
    />
  </FormField>

  <FormField label="Schedule">
    <ScheduleEditorView
      {kind}
      {kindOptions}
      {weekdayLabels}
      {monthOptions}
      {whenLocal}
      {everyMinutes}
      {weekdays}
      {monthlyDay}
      {yearlyMonth}
      {yearlyDay}
      {timeText}
      onKindChange={onKindChange ?? noop}
      onWhenChange={onWhenChange ?? noop}
      onEveryMinutesChange={onEveryMinutesChange ?? noop}
      onToggleWeekday={onToggleWeekday ?? noop}
      onMonthlyDayChange={onMonthlyDayChange ?? noop}
      onYearlyMonthChange={onYearlyMonthChange ?? noop}
      onYearlyDayChange={onYearlyDayChange ?? noop}
      onTimeChange={onTimeChange ?? noop}
    />
  </FormField>

  <FormField label="Make up a missed run on the next start" horizontal>
    <Toggle
      checked={runMissed}
      ariaLabel="Make up a missed run"
      onchange={(v) => (onToggleRunMissed ?? noop)(v)}
    />
  </FormField>
</div>

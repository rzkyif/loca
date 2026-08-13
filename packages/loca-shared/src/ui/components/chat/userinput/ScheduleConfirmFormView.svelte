<!-- Pre-save confirmation card for a Scheduled Prompt: an accent header with
     prose, the editable title + prompt fields, the decomposed schedule editor,
     and the "run if missed" toggle. Pure: the client feeds the live draft
     fields (bound) and the schedule editor's flat decomposed props + callbacks,
     so the gallery and the app render the card identically. -->
<script lang="ts">
  import Input from "../../primitives/Input.svelte";
  import Textarea from "../../primitives/Textarea.svelte";
  import Toggle from "../../primitives/Toggle.svelte";
  import IconText from "../../primitives/IconText.svelte";
  import ScheduleEditorView from "../ScheduleEditorView.svelte";

  type OptionValue = string | number;
  type Option = { value: OptionValue; label: string };

  const noop = (): void => {};

  let {
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
    onKindChange = noop,
    onWhenChange = noop,
    onEveryMinutesChange = noop,
    onToggleWeekday = noop,
    onMonthlyDayChange = noop,
    onYearlyMonthChange = noop,
    onYearlyDayChange = noop,
    onTimeChange = noop,
    title = $bindable(""),
    instruction = $bindable(""),
    runMissed = $bindable(false),
  }: {
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
    onKindChange?: (kind: string) => void;
    onWhenChange?: (text: string) => void;
    onEveryMinutesChange?: (v: string) => void;
    onToggleWeekday?: (day: number) => void;
    onMonthlyDayChange?: (v: string) => void;
    onYearlyMonthChange?: (v: string) => void;
    onYearlyDayChange?: (v: string) => void;
    onTimeChange?: (text: string) => void;
    title?: string;
    instruction?: string;
    runMissed?: boolean;
  } = $props();
</script>

<div class="flex flex-col gap-2 min-w-0 w-120 max-w-[calc(100vw-135px)] text-sm">
  <IconText icon="i-material-symbols-calendar-clock-rounded" color="text-default-800">
    Review Scheduled Prompt
  </IconText>
  <Input
    value={title}
    oninput={(v) => (title = v)}
    placeholder="Title"
    ariaLabel="Scheduled prompt title"
  />
  <Textarea
    value={instruction}
    oninput={(v) => (instruction = v)}
    autoResize="scroll"
    minHeight="min-h-16"
    placeholder="The prompt to send when it runs"
    ariaLabel="Scheduled prompt instruction"
  />
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
    {onKindChange}
    {onWhenChange}
    {onEveryMinutesChange}
    {onToggleWeekday}
    {onMonthlyDayChange}
    {onYearlyMonthChange}
    {onYearlyDayChange}
    {onTimeChange}
  />
  <div class="flex items-center justify-between gap-3 text-xs text-default-700">
    <span>Make up a missed run on the next start</span>
    <div class="w-24 shrink-0">
      <Toggle
        compact
        labels={{ on: "ON", off: "OFF" }}
        checked={runMissed}
        onchange={(v) => (runMissed = v)}
        ariaLabel="Make up a missed run"
      />
    </div>
  </div>
</div>

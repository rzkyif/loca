<!-- Presentational editor for one schedule: a kind select plus the fields that
     the chosen kind needs (when, every-N-minutes, weekday buttons, month/day,
     time-of-day). Pure: the client decomposes its ScheduleSpec into the flat
     draft fields below and feeds them in, then turns the raw input/click
     callbacks back into a valid spec. No serialization or store logic lives
     here. -->
<script lang="ts">
  import Input from "../primitives/Input.svelte";
  import Select from "../primitives/Select.svelte";

  type OptionValue = string | number;
  type Option = { value: OptionValue; label: string };

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
    disabled = false,
    onKindChange = () => {},
    onWhenChange = () => {},
    onEveryMinutesChange = () => {},
    onToggleWeekday = () => {},
    onMonthlyDayChange = () => {},
    onYearlyMonthChange = () => {},
    onYearlyDayChange = () => {},
    onTimeChange = () => {},
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
    disabled?: boolean;
    onKindChange?: (kind: string) => void;
    onWhenChange?: (text: string) => void;
    onEveryMinutesChange?: (v: string) => void;
    onToggleWeekday?: (day: number) => void;
    onMonthlyDayChange?: (v: string) => void;
    onYearlyMonthChange?: (v: string) => void;
    onYearlyDayChange?: (v: string) => void;
    onTimeChange?: (text: string) => void;
  } = $props();

  // The native calendar / clock picker glyphs follow the app theme via the
  // inherited `color-scheme` set on the theme root (base.css), so a dark window
  // paints them light-on-dark with no per-input override here.
  const rawInputClass =
    "bg-surface-inset text-default-800 rounded-medium h-8 px-2 text-sm outline-none";
</script>

<div class="flex flex-col gap-2 text-sm {disabled ? 'opacity-60 pointer-events-none' : ''}">
  <div class="flex items-center gap-2">
    <span class="text-xs text-default-600 w-16 shrink-0">Repeat</span>
    <Select
      value={kind}
      options={kindOptions}
      onchange={onKindChange}
      {disabled}
      ariaLabel="Repeat"
      class="max-w-44"
    />
  </div>

  {#if kind === "once"}
    <div class="flex items-center gap-2">
      <span class="text-xs text-default-600 w-16 shrink-0">When</span>
      <input
        type="datetime-local"
        class="{rawInputClass} w-full max-w-44"
        value={whenLocal}
        aria-label="Date and time"
        {disabled}
        onchange={(e) => onWhenChange((e.target as HTMLInputElement).value)}
      />
    </div>
  {:else if kind === "interval"}
    <div class="flex items-center gap-2">
      <span class="text-xs text-default-600 w-16 shrink-0">Every</span>
      <div class="w-24">
        <Input
          type="number"
          value={everyMinutes}
          min={1}
          max={10080}
          {disabled}
          ariaLabel="Minutes between runs"
          onchange={(v) => onEveryMinutesChange(v)}
        />
      </div>
      <span class="text-xs text-default-600">minutes</span>
    </div>
  {:else}
    {#if kind === "weekly"}
      <div class="flex items-center gap-2">
        <span class="text-xs text-default-600 w-16 shrink-0">Days</span>
        <div class="flex flex-wrap gap-1">
          {#each weekdayLabels as label, day}
            {@const active = weekdays.includes(day)}
            <button
              type="button"
              {disabled}
              aria-pressed={active}
              class="px-2 h-7 rounded-medium text-xs font-medium transition-colors hover:cursor-pointer {active
                ? 'bg-default-inverted-300 text-default-inverted-800'
                : 'bg-surface-inset text-default-700 hover:text-default-900'}"
              onclick={() => onToggleWeekday(day)}
            >
              {label}
            </button>
          {/each}
        </div>
      </div>
    {:else if kind === "monthly"}
      <div class="flex items-center gap-2">
        <span class="text-xs text-default-600 w-16 shrink-0">Day</span>
        <div class="w-24">
          <Input
            type="number"
            value={monthlyDay}
            min={1}
            max={31}
            {disabled}
            ariaLabel="Day of the month"
            onchange={(v) => onMonthlyDayChange(v)}
          />
        </div>
        <span class="text-xs text-default-600">of the month</span>
      </div>
    {:else if kind === "yearly"}
      <div class="flex items-center gap-2">
        <span class="text-xs text-default-600 w-16 shrink-0">Date</span>
        <Select
          value={yearlyMonth ?? 1}
          options={monthOptions}
          onchange={onYearlyMonthChange}
          {disabled}
          ariaLabel="Month"
          class="max-w-36"
        />
        <div class="w-20">
          <Input
            type="number"
            value={yearlyDay}
            min={1}
            max={31}
            {disabled}
            ariaLabel="Day of the month"
            onchange={(v) => onYearlyDayChange(v)}
          />
        </div>
      </div>
    {/if}
    <div class="flex items-center gap-2">
      <span class="text-xs text-default-600 w-16 shrink-0">Time</span>
      <input
        type="time"
        class={rawInputClass}
        value={timeText}
        aria-label="Time of day"
        {disabled}
        onchange={(e) => onTimeChange((e.target as HTMLInputElement).value)}
      />
    </div>
  {/if}
</div>

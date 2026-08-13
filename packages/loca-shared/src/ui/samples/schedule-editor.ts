import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ScheduleEditorView from "../components/chat/ScheduleEditorView.svelte";

const KIND_OPTIONS = [
  { value: "once", label: "Once" },
  { value: "interval", label: "Interval" },
  { value: "weekly", label: "Weekly" },
  { value: "monthly", label: "Monthly" },
  { value: "yearly", label: "Yearly" },
];

const WEEKDAY_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

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

export const scheduleEditorSamples = {
  daily: {
    kind: "weekly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    weekdays: [0, 1, 2, 3, 4, 5, 6],
    timeText: "09:00",
  },
  weekly: {
    kind: "weekly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    weekdays: [1, 3, 5],
    timeText: "08:30",
  },
  once: {
    kind: "once",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    whenLocal: "2026-07-01T14:00",
  },
  interval: {
    kind: "interval",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    everyMinutes: 60,
  },
  yearly: {
    kind: "yearly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    yearlyMonth: 1,
    yearlyDay: 1,
    timeText: "00:00",
  },
  monthly: {
    kind: "monthly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    monthlyDay: 15,
    timeText: "09:00",
  },
  disabled: {
    kind: "weekly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    weekdays: [1, 3, 5],
    timeText: "08:30",
    disabled: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ScheduleEditorView>>>;

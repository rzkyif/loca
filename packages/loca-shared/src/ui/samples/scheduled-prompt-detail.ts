import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ScheduledPromptDetailView from "../components/settings/ScheduledPromptDetailView.svelte";

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

export const scheduledPromptDetailSamples = {
  hasRun: {
    nextRunText: "Next run in 3 hours",
    lastRunText: "Last run 2 days ago",
    enabled: true,
    runMissed: true,
    draftTitle: "Morning briefing",
    draftInstruction: "Summarize my unread messages and today's calendar.",
    kind: "weekly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    weekdays: [1, 2, 3, 4, 5],
    timeText: "09:00",
  },
  neverRun: {
    nextRunText: "Next run in 1 hour",
    lastRunText: "",
    enabled: true,
    runMissed: false,
    draftTitle: "Weekly digest",
    draftInstruction: "Compile a digest of the week's notable changes.",
    kind: "interval",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    everyMinutes: 60,
  },
  disabled: {
    nextRunText: "Paused",
    lastRunText: "Last run a week ago",
    enabled: false,
    runMissed: false,
    draftTitle: "Year-end report",
    draftInstruction: "Draft the annual summary report.",
    kind: "yearly",
    kindOptions: KIND_OPTIONS,
    weekdayLabels: WEEKDAY_LABELS,
    monthOptions: MONTH_OPTIONS,
    yearlyMonth: 12,
    yearlyDay: 31,
    timeText: "08:00",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ScheduledPromptDetailView>>>;

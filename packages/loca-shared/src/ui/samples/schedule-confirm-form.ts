import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ScheduleConfirmFormView from "../components/chat/userinput/ScheduleConfirmFormView.svelte";

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

const BASE = {
  kindOptions: KIND_OPTIONS,
  weekdayLabels: WEEKDAY_LABELS,
  monthOptions: MONTH_OPTIONS,
};

export const scheduleConfirmFormSamples = {
  default: {
    ...BASE,
    kind: "weekly",
    weekdays: [1, 3, 5],
    timeText: "08:30",
    title: "Morning digest",
    instruction: "Summarize my unread mail and today's calendar.",
    runMissed: false,
  },
  runMissedOn: {
    ...BASE,
    kind: "interval",
    everyMinutes: 60,
    title: "Hourly check",
    instruction: "Check the build status and flag any failures.",
    runMissed: true,
  },
  once: {
    ...BASE,
    kind: "once",
    whenLocal: "2026-07-01T14:00",
    title: "One-off reminder",
    instruction: "Draft the release notes.",
    runMissed: false,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ScheduleConfirmFormView>>>;

// Scheduled prompt shapes shared between core and client. A scheduled
// prompt fires an automated session at the times its ScheduleSpec
// describes; the core scheduler owns persistence and arming.

// When a schedule fires, expressed in the core's local time. `weekdays`
// uses 0 = Sunday .. 6 = Saturday (Date.getDay()). Monthly/yearly days
// past a month's end clamp to its last day.
export type ScheduleSpec =
  | { kind: "once"; atMs: number }
  | { kind: "interval"; everyMinutes: number }
  | { kind: "weekly"; weekdays: number[]; hour: number; minute: number }
  | { kind: "monthly"; day: number; hour: number; minute: number }
  | {
      kind: "yearly";
      month: number;
      day: number;
      hour: number;
      minute: number;
    };

// The user-editable fields, as proposed by the agent in the schedule
// confirm flow and as edited in Settings.
export interface ScheduledPromptDraft {
  title: string;
  // The automated user prompt sent when the schedule fires.
  instruction: string;
  schedule: ScheduleSpec;
  // When true and the core was off at fire time, the missed run is made
  // up once on the next boot.
  runMissed: boolean;
}

export interface ScheduledPrompt extends ScheduledPromptDraft {
  id: string;
  ownerClientId: string;
  enabled: boolean;
  lastRunAtMs?: number;
  // Absent when the schedule has no future occurrence (a fired "once").
  nextRunAtMs?: number;
  createdAtMs: number;
  updatedAtMs: number;
}

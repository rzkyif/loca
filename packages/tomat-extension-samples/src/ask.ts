// askUser samples: one tool per question kind (choice, diff, files, image,
// table). Each is a minimal, standalone demo of how a tool pauses on a
// user prompt and reads the answer back. Answers are tolerated when empty
// or wrong-typed so the demos never throw on an odd reply.

import type { ToolContext } from "./types.ts";
import { intArg, SAMPLE_PNG_B64, SAMPLE_PNG_MIME, stringArg } from "./sample-data.ts";

/** First answer as a plain string, or "" when missing/array. */
function asString(answer: unknown): string {
  return typeof answer === "string" ? answer : "";
}

/** Answer as a string[], lifting a single string into a one-item array. */
function asStringArray(answer: unknown): string[] {
  if (Array.isArray(answer)) {
    return answer.filter((v): v is string => typeof v === "string");
  }
  return typeof answer === "string" && answer.length > 0 ? [answer] : [];
}

export async function sampleChoice(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ name: string; color: string; preferences: string[] }> {
  const answers = await ctx.askUser([
    {
      question: "What is your name?",
      allowFreeformInput: true,
    },
    {
      question: "Pick a favorite color.",
      options: [
        { label: "Red", value: "red" },
        { label: "Blue", value: "blue" },
        { label: "Green", value: "green" },
      ],
    },
    {
      question: "Choose any preferences (or add your own).",
      options: [
        { label: "Dark mode", value: "dark" },
        { label: "Notifications", value: "notifications" },
        { label: "Compact layout", value: "compact" },
      ],
      multiselect: true,
      allowFreeformInput: true,
    },
  ]);

  return {
    name: asString(answers[0]),
    color: asString(answers[1]),
    preferences: asStringArray(answers[2]),
  };
}

export async function sampleDiff(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ accepted: boolean }> {
  const answers = await ctx.askUser([
    {
      kind: "diff",
      question: "Apply this change?",
      title: "greeting.txt",
      before: "Hello there.",
      after: "Hello there, friend!",
    },
  ]);
  return { accepted: asString(answers[0]) === "accept" };
}

export async function sampleFiles(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ chosen: string[] }> {
  const answers = await ctx.askUser([
    {
      kind: "files",
      question: "Which files should I work on?",
      multiselect: true,
      entries: [
        {
          path: "/example/report.pdf",
          label: "report.pdf",
          description: "Quarterly report",
        },
        {
          path: "/example/photo.png",
          label: "photo.png",
          description: "A screenshot",
        },
        {
          path: "/example/notes.md",
          label: "notes.md",
          description: "Meeting notes",
        },
      ],
    },
  ]);
  return { chosen: asStringArray(answers[0]) };
}

export async function sampleImage(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ action: string }> {
  const answers = await ctx.askUser([
    {
      kind: "image",
      question: "Keep this image?",
      dataB64: SAMPLE_PNG_B64,
      mime: SAMPLE_PNG_MIME,
      actions: [
        { label: "Keep", value: "keep" },
        { label: "Discard", value: "discard" },
      ],
    },
  ]);
  return { action: asString(answers[0]) };
}

export async function sampleTable(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ saved: number; rows: Record<string, string>[] }> {
  const columns =
    Array.isArray(args.columns) && args.columns.every((c) => typeof c === "string")
      ? (args.columns as string[])
      : ["item", "amount"];

  const answers = await ctx.askUser([
    {
      kind: "table",
      question: "Review the rows before saving:",
      columns,
      rows: [
        ["Apples", "3"],
        ["Bananas", "6"],
      ],
    },
  ]);

  const answer = answers[0];
  const rows = Array.isArray(answer)
    ? answer.filter((r): r is Record<string, string> => typeof r === "object" && r !== null)
    : [];

  await ctx.db.execute(
    "CREATE TABLE IF NOT EXISTS sample_rows (id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL, saved_at INTEGER NOT NULL)",
  );
  let saved = 0;
  const savedAt = Date.now();
  for (const row of rows) {
    await ctx.db.execute("INSERT INTO sample_rows (data, saved_at) VALUES (?, ?)", [
      JSON.stringify(row),
      savedAt,
    ]);
    saved++;
  }

  ctx.setProgress(1, "Saved rows", `${saved} stored`);
  return { saved, rows };
}

export async function sampleStt(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ text: string }> {
  const dataB64 = stringArg(args, "dataB64", SAMPLE_PNG_B64);
  const mime = stringArg(args, "mime", "audio/wav");
  const { text } = await ctx.stt.transcribe({ dataB64, mime });
  return { text };
}

export async function sampleSchedule(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ accepted: boolean; draft?: unknown }> {
  const everyMinutes = Math.max(1, intArg(args, "everyMinutes", 60));
  const result = await ctx.schedulePrompt({
    title: "Sample reminder",
    instruction: "This is a sample scheduled prompt from the samples extension.",
    schedule: { kind: "interval", everyMinutes },
    runMissed: false,
  });
  return { accepted: result.accepted, draft: result.draft };
}

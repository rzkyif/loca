// Minimal local copy of the ToolContext shape the Tomat worker injects.
// Keeping this in-source means the extension has no compile-time dependency
// on a Tomat SDK package. The worker just calls the exported function
// with the structure described here. If the shape ever drifts, this file
// is the source of truth for what the built-in extension relies on.

/** One askUser question, discriminated by `kind` (absent = "choice").
 *  Per-kind answer shapes (one entry per question, in order): choice =
 *  chosen value (string[] when multiselect), diff = "accept" | "reject",
 *  files = chosen path(s), image = chosen action value, table = the rows
 *  as the user edited them, keyed by column name. */
export type AskUserQuestion =
  | {
      kind?: "choice";
      question: string;
      options?: { label: string; description?: string; value: string }[];
      multiselect?: boolean;
      allowFreeformInput?: boolean;
    }
  | {
      kind: "diff";
      question: string;
      before: string;
      after: string;
      title?: string;
    }
  | {
      kind: "files";
      question: string;
      entries: Array<{ path: string; label?: string; description?: string }>;
      multiselect?: boolean;
    }
  | {
      kind: "image";
      question: string;
      dataB64: string;
      mime: string;
      actions: Array<{ label: string; value: string }>;
    }
  | { kind: "table"; question: string; columns: string[]; rows: string[][] };

export type AskUserAnswer = string | string[] | Array<Record<string, string>>;

export interface ToolChatContext {
  userMessage: string;
  sessionId: string | null;
  locale?: string;
}

export interface MemoryListing {
  title: string;
  kind: "knowledge" | "skill";
  summary?: string;
  updatedAtMs: number;
}

/** When a scheduled prompt fires, in the host's local time. `weekdays`
 *  uses 0 = Sunday .. 6 = Saturday. Monthly/yearly days past a month's
 *  end clamp to its last day. */
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

export interface ScheduledPromptDraft {
  title: string;
  /** The automated user prompt sent when the schedule fires. */
  instruction: string;
  schedule: ScheduleSpec;
  /** Make up a run missed while the host was off (once, on its next boot). */
  runMissed: boolean;
}

export interface ToolContext {
  setProgress(progress: number, label?: string, description?: string): void;
  askUser(questions: AskUserQuestion[]): Promise<AskUserAnswer[]>;
  log(level: "debug" | "info" | "warn" | "error", message: string): void;
  /** One-way display pushes: each renders a standalone bubble in the chat.
   *  Never awaited; nothing flows back to the tool. */
  display: {
    markdown(markdown: string): void;
    image(dataB64: string, mime: string, alt?: string): void;
    table(columns: string[], rows: string[][]): void;
    diff(before: string, after: string, title?: string): void;
  };
  /** The user's memory store, title-keyed. Calls are gated by the host's
   *  memories permission (read for list/get, write for write/edit); the
   *  first ungranted use pauses on a user prompt and rejects when refused.
   *  `write` creates the memory or replaces its content; `edit` does an
   *  exact single-occurrence find/replace and errors when the find text
   *  matches zero times or more than once. */
  memories: {
    list(): Promise<MemoryListing[]>;
    get(title: string): Promise<{ title: string; content: string }>;
    getFile(title: string, name: string): Promise<{ title: string; name: string; content: string }>;
    write(
      title: string,
      content: string,
    ): Promise<{ title: string; before: string; after: string; created: boolean }>;
    edit(
      title: string,
      find: string,
      replace: string,
    ): Promise<{ title: string; before: string; after: string }>;
  };
  /** Per-extension private SQLite, proxied to the core. Requires the
   *  extension's tomat.json to declare "database": true; deleted with the
   *  extension on uninstall. */
  db: {
    query(
      sql: string,
      params?: Array<string | number | boolean | null>,
    ): Promise<Record<string, unknown>[]>;
    execute(
      sql: string,
      params?: Array<string | number | boolean | null>,
    ): Promise<{ changes: number; lastInsertRowId: number }>;
  };
  /** Single-shot completion against the user's configured model. Output is
   *  capped host-side; gated by the llm permission. */
  llm: {
    complete(opts: {
      prompt: string;
      systemPrompt?: string;
      maxTokens?: number;
    }): Promise<{ text: string }>;
  };
  /** Synthesize speech from text (WAV, base64). Gated by the tts permission
   *  and the host's Text-to-Speech enable setting. */
  tts: {
    speak(text: string): Promise<{ dataB64: string; mime: string; sampleRate: number }>;
  };
  /** Transcribe audio to text. Gated by the stt permission and the host's
   *  Speech-to-Text enable setting. */
  stt: {
    transcribe(opts: {
      dataB64: string;
      mime?: string;
      language?: string;
    }): Promise<{ text: string }>;
  };
  /** Propose a scheduled prompt. The host pauses the call on an editable
   *  in-chat confirm form and resolves with the user's decision; when
   *  accepted, `draft` is the final (possibly user-edited) draft that was
   *  saved. The form is the consent gate, so no permission is involved. */
  schedulePrompt(
    draft: ScheduledPromptDraft,
  ): Promise<{ accepted: boolean; draft?: ScheduledPromptDraft }>;
  signal: AbortSignal;
  getChatContext(): ToolChatContext;
}

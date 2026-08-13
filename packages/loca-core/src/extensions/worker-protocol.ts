// NDJSON frames between the worker pool and a tool worker subprocess.
// Mirrors the shape used today by the Bun sidecar (src/bun/extensions/types.ts)
// with one addition: stderr_log, so the worker can surface stderr text the
// pool will forward to its log subscribers.

// Askuser question/answer shapes are the shared wire shapes: the pool
// forwards them between the worker and the client verbatim.
export type { AskUserAnswer, AskUserQuestion } from "@loca/shared";
import type {
  AskUserAnswer,
  AskUserQuestion,
  DisplayContent,
  ScheduledPromptDraft,
} from "@loca/shared";

// Core modules a tool can reach over the stdio protocol (module_request).
// Access is gated by the module broker: grants for memories/llm/tts/stt,
// the extension's `database` declaration for db.
export type ModuleName = "memories" | "db" | "llm" | "tts" | "stt";

export interface ChatContext {
  userMessage: string;
  sessionId: string | null;
  locale?: string;
}

export type PoolToWorkerFrame =
  | { kind: "boot"; extensionId: string; entryPath: string }
  | {
      kind: "call";
      callId: string;
      toolName: string;
      fnExport: string;
      arguments: string;
      chatContext: ChatContext;
    }
  | { kind: "cancel"; callId: string }
  | {
      kind: "ask_user_response";
      callId: string;
      requestId: string;
      answers: AskUserAnswer[];
    }
  // Reply to a module_request: `result` when ok, `error` otherwise.
  | {
      kind: "module_response";
      callId: string;
      requestId: string;
      ok: boolean;
      result?: unknown;
      error?: string;
    }
  // Reply to a schedule_request. `draft` carries the user's edits when
  // accepted; absent on rejection.
  | {
      kind: "schedule_confirm_response";
      callId: string;
      requestId: string;
      accepted: boolean;
      draft?: ScheduledPromptDraft;
    }
  | { kind: "shutdown" };

export type WorkerToPoolFrame =
  | { kind: "ready" }
  | { kind: "booted"; extensionId: string }
  | { kind: "boot_failed"; extensionId: string; error: string }
  // Synthesized by WorkerHandle (not the worker process) when a Deno
  // permission prompt on the worker's PTY needs the user's decision; the
  // pool forwards it to chat and answers via WorkerHandle.answerPrompt().
  // No callId: a prompt blocks the worker's whole JS thread. WorkerHandle
  // only emits this when exactly one call is in flight (it fails the prompt
  // closed otherwise), so the pool can attribute it to that call.
  | {
      kind: "permission_prompt";
      requestId: string;
      permission: import("@loca/shared").PermissionKind;
      resource: string;
      apiName?: string;
      declared: boolean;
      reason?: string;
    }
  // Synthesized by WorkerHandle when the worker process exits. No callId: it
  // settles every in-flight call on the dead worker (crash, OOM, give-up kill,
  // or a refreshPermissions teardown) instead of leaving them to hang until
  // the pool's call timeout fires.
  | { kind: "worker_exited"; code: number }
  | {
      kind: "progress";
      callId: string;
      progress: number;
      label?: string;
      description?: string;
    }
  | {
      kind: "ask_user_request";
      callId: string;
      requestId: string;
      questions: AskUserQuestion[];
    }
  | {
      kind: "log";
      callId: string;
      level: "debug" | "info" | "warn" | "error";
      message: string;
    }
  | { kind: "tool_result"; callId: string; result: unknown }
  | { kind: "tool_error"; callId: string; error: string }
  // One-way: push a display bubble into the chat (ctx.display.*). Never
  // awaited; the pool forwards it as a CallEvent and chat persists it.
  | { kind: "display"; callId: string; content: DisplayContent }
  // Awaited call into a core module (memories, db, llm, tts, stt). The
  // module broker gates access and the pool replies with module_response.
  | {
      kind: "module_request";
      callId: string;
      requestId: string;
      module: ModuleName;
      op: string;
      args: unknown;
    }
  // Awaited proposal of a scheduled prompt (ctx.schedulePrompt). The user
  // confirms or rejects in chat; the pool replies with
  // schedule_confirm_response.
  | {
      kind: "schedule_request";
      callId: string;
      requestId: string;
      draft: ScheduledPromptDraft;
    }
  | { kind: "stderr_log"; line: string };

// --- wire screening ---------------------------------------------------------

// Structural screen for frames parsed off the worker's stdout. The worker
// process is sandboxed but runs untrusted extension code, so a frame is dropped
// (null) when it is malformed or claims a kind only the WorkerHandle itself
// may synthesize (permission_prompt, worker_exited, stderr_log); a extension
// must not be able to forge a permission prompt or a fake exit by printing
// JSON. Deep payloads keep their declared types here and are validated
// semantically where they are consumed: askUser questions and schedule drafts
// against their Zod schemas in the pool, module args in the broker.
export function parseWorkerFrame(value: unknown): WorkerToPoolFrame | null {
  if (typeof value !== "object" || value === null) return null;
  const f = value as Record<string, unknown>;
  const id = (v: unknown): v is string => typeof v === "string" && v.length > 0;
  switch (f.kind) {
    case "ready":
      return { kind: "ready" };
    case "booted":
      return id(f.extensionId) ? { kind: "booted", extensionId: f.extensionId } : null;
    case "boot_failed":
      return id(f.extensionId) && typeof f.error === "string"
        ? { kind: "boot_failed", extensionId: f.extensionId, error: f.error }
        : null;
    case "progress":
      return id(f.callId) && typeof f.progress === "number" && Number.isFinite(f.progress)
        ? {
            kind: "progress",
            callId: f.callId,
            progress: f.progress,
            label: typeof f.label === "string" ? f.label : undefined,
            description: typeof f.description === "string" ? f.description : undefined,
          }
        : null;
    case "ask_user_request":
      return id(f.callId) && id(f.requestId) && Array.isArray(f.questions)
        ? {
            kind: "ask_user_request",
            callId: f.callId,
            requestId: f.requestId,
            questions: f.questions as AskUserQuestion[],
          }
        : null;
    case "log": {
      const levels = ["debug", "info", "warn", "error"] as const;
      const level = levels.find((l) => l === f.level);
      return id(f.callId) && level !== undefined && typeof f.message === "string"
        ? { kind: "log", callId: f.callId, level, message: f.message }
        : null;
    }
    case "tool_result":
      return id(f.callId) ? { kind: "tool_result", callId: f.callId, result: f.result } : null;
    case "tool_error":
      return id(f.callId) && typeof f.error === "string"
        ? { kind: "tool_error", callId: f.callId, error: f.error }
        : null;
    case "display":
      return id(f.callId) && isDisplayContent(f.content)
        ? {
            kind: "display",
            callId: f.callId,
            content: canonicalDisplayContent(f.content),
          }
        : null;
    case "module_request": {
      const modules: readonly ModuleName[] = ["memories", "db", "llm", "tts", "stt"];
      const module = modules.find((m) => m === f.module);
      return id(f.callId) && id(f.requestId) && module !== undefined && typeof f.op === "string"
        ? {
            kind: "module_request",
            callId: f.callId,
            requestId: f.requestId,
            module,
            op: f.op,
            args: f.args,
          }
        : null;
    }
    case "schedule_request":
      return id(f.callId) && id(f.requestId) && typeof f.draft === "object" && f.draft !== null
        ? {
            kind: "schedule_request",
            callId: f.callId,
            requestId: f.requestId,
            draft: f.draft as ScheduledPromptDraft,
          }
        : null;
    default:
      return null;
  }
}

function isDisplayContent(v: unknown): v is DisplayContent {
  if (typeof v !== "object" || v === null) return false;
  const c = v as Record<string, unknown>;
  const strings = (arr: unknown): arr is string[] =>
    Array.isArray(arr) && arr.every((x) => typeof x === "string");
  switch (c.type) {
    case "markdown":
      return typeof c.markdown === "string";
    case "image":
      return typeof c.dataB64 === "string" && typeof c.mime === "string";
    case "table":
      return strings(c.columns) && Array.isArray(c.rows) && c.rows.every(strings);
    case "diff":
      return typeof c.before === "string" && typeof c.after === "string";
    default:
      return false;
  }
}

// Rebuild the content with only its declared fields so worker-supplied
// extras never ride along into persistence.
function canonicalDisplayContent(c: DisplayContent): DisplayContent {
  switch (c.type) {
    case "markdown":
      return { type: "markdown", markdown: c.markdown };
    case "image":
      return {
        type: "image",
        dataB64: c.dataB64,
        mime: c.mime,
        alt: typeof c.alt === "string" ? c.alt : undefined,
      };
    case "table":
      return { type: "table", columns: c.columns, rows: c.rows };
    case "diff":
      return {
        type: "diff",
        before: c.before,
        after: c.after,
        title: typeof c.title === "string" ? c.title : undefined,
      };
  }
}

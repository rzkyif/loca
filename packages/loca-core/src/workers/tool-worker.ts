// Tool worker subprocess (one per enabled extension).
// Spawned by extensions/worker-handle.ts with exactly the --allow-* flags
// derived from the extension's enabled tools' granted permissions.
//
// CLI:
//   deno run [flags] tool-worker.ts --extension-id=<id> --entry=<absPath>
//     [--control-addr=127.0.0.1:<port> --control-token=<hex>]
//
// Protocol: NDJSON, either on stdio (the default) or, when --control-addr is
// given, over a loopback TCP control socket. The socket transport exists for
// the Windows ConPTY path: there the worker's stdout+stderr are bound to the
// pseudoconsole (so Deno can show its interactive permission prompt), which
// mangles a byte-exact protocol stream. Routing the protocol over a private
// loopback socket keeps stdio free for the prompt and the NDJSON stream clean.
// See extensions/worker-protocol.ts and extensions/control-socket.ts.

import type {
  AskUserAnswer,
  AskUserQuestion,
  ModuleName,
  PoolToWorkerFrame,
  WorkerToPoolFrame,
} from "../extensions/worker-protocol.ts";
import type { DisplayContent, ScheduledPromptDraft } from "@loca/shared";

// Inlined from @loca/shared: there is no import map next to the installed
// worker to resolve workspace aliases.
function errMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

type MemoryListing = {
  title: string;
  kind: "knowledge" | "skill";
  summary?: string;
  updatedAtMs: number;
};

type ToolContext = {
  setProgress(progress: number, label?: string, description?: string): void;
  askUser(questions: AskUserQuestion[]): Promise<AskUserAnswer[]>;
  log(level: "debug" | "info" | "warn" | "error", message: string): void;
  // One-way display pushes: each renders a standalone bubble in the chat.
  display: {
    markdown(markdown: string): void;
    image(dataB64: string, mime: string, alt?: string): void;
    table(columns: string[], rows: string[][]): void;
    diff(before: string, after: string, title?: string): void;
  };
  // Awaited host-module calls, gated by the core's module broker (a first
  // use may pause on a user permission prompt).
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
   *  extension's loca.json to declare "database": true; deleted with the
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
  // Awaited scheduled-prompt proposal: the user confirms (possibly after
  // editing the draft) or rejects it in chat. The confirm form is the
  // consent gate, so no permission grant is involved.
  schedulePrompt(
    draft: ScheduledPromptDraft,
  ): Promise<{ accepted: boolean; draft?: ScheduledPromptDraft }>;
  signal: AbortSignal;
  getChatContext(): {
    userMessage: string;
    sessionId: string | null;
    locale?: string;
  };
};

type ToolFn = (args: Record<string, unknown>, ctx: ToolContext) => Promise<unknown>;

interface CallState {
  abort: AbortController;
  chatContext: {
    userMessage: string;
    sessionId: string | null;
    locale?: string;
  };
  pendingAskUser: Map<
    string,
    {
      resolve: (a: AskUserAnswer[]) => void;
      reject: (e: Error) => void;
    }
  >;
  // Awaited module_request calls (memories, db, llm, tts, stt) waiting on
  // the pool's module_response.
  pendingModule: Map<
    string,
    {
      resolve: (result: unknown) => void;
      reject: (e: Error) => void;
    }
  >;
  // Awaited schedule_request calls waiting on the pool's
  // schedule_confirm_response.
  pendingSchedule: Map<
    string,
    {
      resolve: (outcome: { accepted: boolean; draft?: ScheduledPromptDraft }) => void;
      reject: (e: Error) => void;
    }
  >;
}

const calls = new Map<string, CallState>();
let extensionMod: Record<string, unknown> | null = null;
let _extensionId = "";

// The protocol transport, one direction each. `writeLine` serializes an
// outgoing frame; `readLines` yields raw NDJSON lines from the pool. The stdio
// and socket implementations differ only in the OS handles they bind to.
interface WorkerTransport {
  writeLine(line: string): void;
  readLines(): AsyncIterableIterator<string>;
  /** Await any queued writes; used before a deliberate exit so the last frame
   *  is not dropped on a socket whose flush is async. */
  flush(): Promise<void>;
}

let transport: WorkerTransport;

function send(frame: WorkerToPoolFrame): void {
  transport.writeLine(JSON.stringify(frame) + "\n");
}

// stdio transport: synchronous stdout writes (flush immediately) and the
// stdin readable. This is the default on macOS/Linux and in Windows pipe mode.
function stdioTransport(): WorkerTransport {
  const encoder = new TextEncoder();
  return {
    writeLine(line) {
      Deno.stdout.writeSync(encoder.encode(line));
    },
    readLines: () => lineIterator(Deno.stdin.readable),
    flush: () => Promise.resolve(),
  };
}

// Loopback TCP transport (Windows ConPTY path). Connects to the core's
// per-worker listener, presents the one-shot token as the first line, then
// carries the NDJSON protocol both ways. Writes are chained so frames never
// interleave; the token is written before any frame so the core can
// authenticate the connection before trusting it.
async function socketTransport(addr: string, token: string): Promise<WorkerTransport> {
  const colon = addr.lastIndexOf(":");
  const hostname = addr.slice(0, colon);
  const port = Number(addr.slice(colon + 1));
  const conn = await Deno.connect({ hostname, port, transport: "tcp" });
  const encoder = new TextEncoder();
  const writer = conn.writable.getWriter();
  let chain = writer.write(encoder.encode(token + "\n"));
  return {
    writeLine(line) {
      chain = chain.then(() => writer.write(encoder.encode(line))).catch(() => {});
    },
    readLines: () => lineIterator(conn.readable),
    flush: () => chain.catch(() => {}),
  };
}

// Split a byte stream into NDJSON lines. Shared by both transports.
async function* lineIterator(stream: ReadableStream<Uint8Array>): AsyncIterableIterator<string> {
  const decoder = new TextDecoder();
  const reader = stream.getReader();
  let buf = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) yield line;
  }
}

async function handleBoot(extension: string, entry: string): Promise<void> {
  _extensionId = extension;
  try {
    extensionMod = await import("file://" + entry);
    send({ kind: "booted", extensionId: extension });
  } catch (err) {
    send({
      kind: "boot_failed",
      extensionId: extension,
      error: errMessage(err),
    });
  }
}

async function handleCall(frame: Extract<PoolToWorkerFrame, { kind: "call" }>): Promise<void> {
  if (!extensionMod) {
    send({
      kind: "tool_error",
      callId: frame.callId,
      error: "worker not booted",
    });
    return;
  }
  const fn = extensionMod[frame.fnExport] as ToolFn | undefined;
  if (typeof fn !== "function") {
    send({
      kind: "tool_error",
      callId: frame.callId,
      error: `no export "${frame.fnExport}"`,
    });
    return;
  }
  let args: Record<string, unknown>;
  try {
    args = JSON.parse(frame.arguments);
    if (!args || typeof args !== "object") {
      throw new Error("arguments must be an object");
    }
  } catch (err) {
    send({
      kind: "tool_error",
      callId: frame.callId,
      error: `invalid arguments JSON: ${errMessage(err)}`,
    });
    return;
  }
  const state: CallState = {
    abort: new AbortController(),
    chatContext: frame.chatContext,
    pendingAskUser: new Map(),
    pendingModule: new Map(),
    pendingSchedule: new Map(),
  };
  calls.set(frame.callId, state);

  // Awaited call into a core module; the pool gates access (permission
  // prompts may pause here) and replies with module_response.
  function moduleRequest(module: ModuleName, op: string, args: unknown): Promise<unknown> {
    const requestId = crypto.randomUUID();
    send({
      kind: "module_request",
      callId: frame.callId,
      requestId,
      module,
      op,
      args,
    });
    return new Promise<unknown>((resolve, reject) => {
      state.pendingModule.set(requestId, { resolve, reject });
    });
  }

  function sendDisplay(content: DisplayContent): void {
    send({ kind: "display", callId: frame.callId, content });
  }

  const ctx: ToolContext = {
    setProgress(progress, label, description) {
      send({
        kind: "progress",
        callId: frame.callId,
        progress: Math.max(0, Math.min(1, progress)),
        label,
        description,
      });
    },
    askUser(questions) {
      const requestId = crypto.randomUUID();
      send({
        kind: "ask_user_request",
        callId: frame.callId,
        requestId,
        questions,
      });
      return new Promise<AskUserAnswer[]>((resolve, reject) => {
        state.pendingAskUser.set(requestId, { resolve, reject });
      });
    },
    log(level, message) {
      send({ kind: "log", callId: frame.callId, level, message });
    },
    display: {
      markdown(markdown) {
        sendDisplay({ type: "markdown", markdown });
      },
      image(dataB64, mime, alt) {
        sendDisplay({ type: "image", dataB64, mime, alt });
      },
      table(columns, rows) {
        sendDisplay({ type: "table", columns, rows });
      },
      diff(before, after, title) {
        sendDisplay({ type: "diff", before, after, title });
      },
    },
    memories: {
      list() {
        return moduleRequest("memories", "list", {}) as Promise<MemoryListing[]>;
      },
      get(title) {
        return moduleRequest("memories", "get", { title }) as Promise<{
          title: string;
          content: string;
        }>;
      },
      getFile(title, name) {
        return moduleRequest("memories", "getFile", { title, name }) as Promise<{
          title: string;
          name: string;
          content: string;
        }>;
      },
      write(title, content) {
        return moduleRequest("memories", "write", {
          title,
          content,
        }) as Promise<{
          title: string;
          before: string;
          after: string;
          created: boolean;
        }>;
      },
      edit(title, find, replace) {
        return moduleRequest("memories", "edit", {
          title,
          find,
          replace,
        }) as Promise<{
          title: string;
          before: string;
          after: string;
        }>;
      },
    },
    db: {
      query(sql, params) {
        return moduleRequest("db", "query", {
          sql,
          params: params ?? [],
        }) as Promise<Record<string, unknown>[]>;
      },
      execute(sql, params) {
        return moduleRequest("db", "execute", {
          sql,
          params: params ?? [],
        }) as Promise<{
          changes: number;
          lastInsertRowId: number;
        }>;
      },
    },
    llm: {
      complete(opts) {
        return moduleRequest("llm", "complete", opts) as Promise<{ text: string }>;
      },
    },
    tts: {
      speak(text) {
        return moduleRequest("tts", "speak", { text }) as Promise<{
          dataB64: string;
          mime: string;
          sampleRate: number;
        }>;
      },
    },
    stt: {
      transcribe(opts) {
        return moduleRequest("stt", "transcribe", opts) as Promise<{ text: string }>;
      },
    },
    schedulePrompt(draft) {
      const requestId = crypto.randomUUID();
      send({
        kind: "schedule_request",
        callId: frame.callId,
        requestId,
        draft,
      });
      return new Promise((resolve, reject) => {
        state.pendingSchedule.set(requestId, { resolve, reject });
      });
    },
    signal: state.abort.signal,
    getChatContext() {
      return state.chatContext;
    },
  };

  try {
    const result = await fn(args, ctx);
    send({ kind: "tool_result", callId: frame.callId, result });
  } catch (err) {
    send({
      kind: "tool_error",
      callId: frame.callId,
      error: errMessage(err),
    });
  } finally {
    calls.delete(frame.callId);
  }
}

function handleCancel(callId: string): void {
  const state = calls.get(callId);
  if (!state) return;
  state.abort.abort();
  for (const p of state.pendingAskUser.values()) {
    p.reject(new Error("User interrupted"));
  }
  state.pendingAskUser.clear();
  for (const p of state.pendingModule.values()) {
    p.reject(new Error("User interrupted"));
  }
  state.pendingModule.clear();
  for (const p of state.pendingSchedule.values()) {
    p.reject(new Error("User interrupted"));
  }
  state.pendingSchedule.clear();
}

function handleModuleResponse(
  frame: Extract<PoolToWorkerFrame, { kind: "module_response" }>,
): void {
  const state = calls.get(frame.callId);
  if (!state) return;
  const p = state.pendingModule.get(frame.requestId);
  if (!p) return;
  state.pendingModule.delete(frame.requestId);
  if (frame.ok) p.resolve(frame.result);
  else p.reject(new Error(frame.error ?? "module request failed"));
}

function handleScheduleConfirmResponse(
  frame: Extract<PoolToWorkerFrame, { kind: "schedule_confirm_response" }>,
): void {
  const state = calls.get(frame.callId);
  if (!state) return;
  const p = state.pendingSchedule.get(frame.requestId);
  if (!p) return;
  state.pendingSchedule.delete(frame.requestId);
  p.resolve({ accepted: frame.accepted, draft: frame.draft });
}

function handleAskUserResponse(callId: string, requestId: string, answers: AskUserAnswer[]): void {
  const state = calls.get(callId);
  if (!state) return;
  const p = state.pendingAskUser.get(requestId);
  if (!p) return;
  state.pendingAskUser.delete(requestId);
  p.resolve(answers);
}

interface WorkerArgs {
  extensionId: string;
  entry: string;
  controlAddr: string | null;
  controlToken: string | null;
}

function parseArgs(): WorkerArgs {
  let extensionId = "";
  let entry = "";
  let controlAddr: string | null = null;
  let controlToken: string | null = null;
  for (const arg of Deno.args) {
    if (arg.startsWith("--extension-id=")) {
      extensionId = arg.slice("--extension-id=".length);
    } else if (arg.startsWith("--entry=")) entry = arg.slice("--entry=".length);
    else if (arg.startsWith("--control-addr=")) controlAddr = arg.slice("--control-addr=".length);
    else if (arg.startsWith("--control-token=")) {
      controlToken = arg.slice("--control-token=".length);
    }
  }
  if (!extensionId || !entry) {
    Deno.stderr.writeSync(new TextEncoder().encode("missing --extension-id / --entry\n"));
    Deno.exit(1);
  }
  return { extensionId, entry, controlAddr, controlToken };
}

async function main(): Promise<void> {
  const args = parseArgs();
  transport =
    args.controlAddr && args.controlToken
      ? await socketTransport(args.controlAddr, args.controlToken)
      : stdioTransport();
  // Boot frame is unsolicited: the pool sees ready -> sends boot. Send ready
  // first so the pool can start tracking.
  send({ kind: "ready" });
  await handleBoot(args.extensionId, args.entry);

  for await (const line of transport.readLines()) {
    if (!line.trim()) continue;
    let frame: PoolToWorkerFrame;
    try {
      frame = JSON.parse(line) as PoolToWorkerFrame;
    } catch {
      send({ kind: "stderr_log", line: `bad frame: ${line}` });
      continue;
    }
    if (frame.kind === "call") void handleCall(frame);
    else if (frame.kind === "cancel") handleCancel(frame.callId);
    else if (frame.kind === "ask_user_response") {
      handleAskUserResponse(frame.callId, frame.requestId, frame.answers);
    } else if (frame.kind === "module_response") {
      handleModuleResponse(frame);
    } else if (frame.kind === "schedule_confirm_response") {
      handleScheduleConfirmResponse(frame);
    } else if (frame.kind === "shutdown") {
      await transport.flush();
      Deno.exit(0);
    } else if (frame.kind === "boot") {
      // Re-boot would be unusual but legal: drop the previous module and re-import.
      await handleBoot(frame.extensionId, frame.entryPath);
    }
  }
}

if (import.meta.main) {
  await main();
}

// Mock external services for the headless E2E harness (Node side).
//
// One local HTTP server stands in for every outbound dependency a real core
// would reach: the OpenAI-compatible LLM, STT, and TTS endpoints, plus the
// HuggingFace / storage artifact host used by the model downloader. Core is
// pointed at this server through settings (llm/stt/tts `provider: "external"`,
// baseUrl -> here) and the `LOCA_HF_BASE_URL` / `LOCA_STORAGE_BASE_URL` env
// overrides, so no real network is ever touched.
//
// The LLM behaviour is scriptable per core instance via `setLlmScript`, so a
// test can ask for a plain reply, a fixed string, or a tool-call round.

import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { existsSync, statSync, createReadStream } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { SHARED_MODELS_DIR } from "./repo.ts";

// Cache the streamed sha256 of each served artifact so the HEAD probe and the
// GET stream report the same `x-linked-etag` (HF's LFS content hash) without
// re-hashing on every request. Keyed by absolute path; multi-GB weights are
// hashed once per process. Concurrent callers share the in-flight promise.
const shaCache = new Map<string, Promise<string>>();
function artifactSha256(absPath: string): Promise<string> {
  const cached = shaCache.get(absPath);
  if (cached) return cached;
  const p = new Promise<string>((resolve, reject) => {
    const hash = createHash("sha256");
    const s = createReadStream(absPath);
    s.on("error", reject);
    s.on("data", (c) => hash.update(c));
    s.on("end", () => resolve(hash.digest("hex")));
  });
  shaCache.set(absPath, p);
  return p;
}

export type LlmScript =
  | { kind: "echo" } // stream back the last user message
  | { kind: "text"; text: string } // stream a fixed reply
  | {
      kind: "toolThenText";
      tool: { name: string; arguments: Record<string, unknown> };
      text: string;
    }
  | {
      // Several tool calls in a single assistant turn; core executes them all,
      // then the next turn (tool results present) returns `text`.
      kind: "multiTool";
      tools: Array<{ name: string; arguments: Record<string, unknown> }>;
      text: string;
    }
  | { kind: "classify"; label: string } // dual-model complexity classifier reply
  | { kind: "slowText"; text: string; perChunkMs: number } // throttled stream a spec can interrupt
  | { kind: "errorMidStream"; afterChars?: number } // emit a few deltas, then abort the stream
  | { kind: "reasoning"; reasoning: string; text: string }; // reasoning deltas, then content

/** A recorded chat-completion request, for assertions (which model answered,
 *  what system prompt / tools were sent). */
export interface RecordedLlmRequest {
  model: string;
  messages: Array<{ role: string; content: unknown }>;
  toolNames: string[];
  stream: boolean;
}

export interface MockServices {
  readonly baseUrl: string; // http://127.0.0.1:<port>
  setLlmScript(script: LlmScript): void;
  /** Every chat-completion request core has made (classifier + generation). */
  requests(): RecordedLlmRequest[];
  close(): Promise<void>;
}

function readBody(req: IncomingMessage): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (c) => chunks.push(c as Buffer));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    // A client that disconnects mid-upload must reject (not hang the handler).
    req.on("error", reject);
  });
}

function sseChunk(obj: unknown): string {
  return `data: ${JSON.stringify(obj)}\n\n`;
}

function lastUserText(messages: Array<{ role: string; content: unknown }>): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "user") {
      const c = messages[i].content;
      if (typeof c === "string") return c;
      if (Array.isArray(c)) {
        return c
          .map((p) =>
            typeof p === "object" && p && "text" in p ? String((p as { text: unknown }).text) : "",
          )
          .join("");
      }
    }
  }
  return "";
}

function hasToolResult(messages: Array<{ role: string }>): boolean {
  return messages.some((m) => m.role === "tool");
}

// A minimal valid 16-bit mono 8kHz WAV with a few ms of silence. Enough for the
// TTS path to receive `audio/wav` bytes and for the client to construct an
// HTMLAudioElement without decoding errors mattering to the happy path.
function silenceWav(): Buffer {
  const sampleRate = 8000;
  const numSamples = 800; // 0.1s
  const dataSize = numSamples * 2;
  const buf = Buffer.alloc(44 + dataSize);
  buf.write("RIFF", 0);
  buf.writeUInt32LE(36 + dataSize, 4);
  buf.write("WAVE", 8);
  buf.write("fmt ", 12);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(1, 20); // PCM
  buf.writeUInt16LE(1, 22); // mono
  buf.writeUInt32LE(sampleRate, 24);
  buf.writeUInt32LE(sampleRate * 2, 28);
  buf.writeUInt16LE(2, 32);
  buf.writeUInt16LE(16, 34);
  buf.write("data", 36);
  buf.writeUInt32LE(dataSize, 40);
  return buf;
}

export async function startMockServices(): Promise<MockServices> {
  let llmScript: LlmScript = { kind: "echo" };
  const recorded: RecordedLlmRequest[] = [];

  const server: Server = createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", "http://127.0.0.1");
    const path = url.pathname;

    try {
      if (req.method === "POST" && path.endsWith("/chat/completions")) {
        const body = JSON.parse((await readBody(req)).toString() || "{}");
        recorded.push({
          model: typeof body.model === "string" ? body.model : "",
          messages: Array.isArray(body.messages) ? body.messages : [],
          toolNames: Array.isArray(body.tools)
            ? body.tools
                .map((t: { function?: { name?: string } }) => t.function?.name ?? "")
                .filter(Boolean)
            : [],
          stream: body.stream === true,
        });
        return handleChat(body, llmScript, res);
      }
      if (req.method === "POST" && path.endsWith("/audio/transcriptions")) {
        // OpenAI STT: respond with a deterministic transcript.
        await readBody(req);
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ text: "hello from speech" }));
      }
      if (req.method === "POST" && path.endsWith("/audio/speech")) {
        await readBody(req);
        const wav = silenceWav();
        res.writeHead(200, { "content-type": "audio/wav", "content-length": String(wav.length) });
        return res.end(wav);
      }
      // Artifact / HuggingFace host: HEAD probe + GET download. Serve from the
      // shared models cache when the requested file exists, else a synthetic
      // payload, always reporting a content-length consistent with the bytes.
      if (req.method === "HEAD" || req.method === "GET") {
        return await handleArtifact(path, req.method, res);
      }
      res.writeHead(404).end("not found");
    } catch (err) {
      res.writeHead(500).end(String(err));
    }
  });

  const port = await new Promise<number>((resolve, reject) => {
    server.on("error", reject); // bind failure must reject, not hang startCore
    server.listen(0, "127.0.0.1", () => resolve((server.address() as { port: number }).port));
  });

  return {
    baseUrl: `http://127.0.0.1:${port}`,
    setLlmScript(s) {
      llmScript = s;
    },
    requests() {
      return recorded;
    },
    close() {
      return new Promise((resolve) => server.close(() => resolve()));
    },
  };
}

function replyText(script: LlmScript, messages: Array<{ role: string; content: unknown }>): string {
  if (script.kind === "echo") return lastUserText(messages) || "ok";
  if (script.kind === "classify") return script.label;
  return script.text;
}

function handleChat(
  body: { messages?: Array<{ role: string; content: unknown }>; stream?: boolean },
  script: LlmScript,
  res: ServerResponse,
): void {
  const messages = body.messages ?? [];

  // Non-streaming completion (e.g. the dual-model complexity classifier): return
  // a plain ChatCompletion JSON, not SSE.
  if (body.stream !== true) {
    const content = replyText(script, messages);
    res.writeHead(200, { "content-type": "application/json" });
    return void res.end(
      JSON.stringify({
        id: "chatcmpl-mock",
        object: "chat.completion",
        created: 1,
        model: "mock",
        choices: [{ index: 0, message: { role: "assistant", content }, finish_reason: "stop" }],
        usage: { prompt_tokens: 10, completion_tokens: 3, total_tokens: 13 },
      }),
    );
  }

  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });

  const id = "chatcmpl-mock";
  const base = { id, object: "chat.completion.chunk", created: 1, model: "mock" };
  const role = () =>
    res.write(
      sseChunk({
        ...base,
        choices: [{ index: 0, delta: { role: "assistant", content: "" }, finish_reason: null }],
      }),
    );
  const text = (t: string) =>
    res.write(
      sseChunk({ ...base, choices: [{ index: 0, delta: { content: t }, finish_reason: null }] }),
    );
  const stop = (reason = "stop") =>
    res.write(sseChunk({ ...base, choices: [{ index: 0, delta: {}, finish_reason: reason }] }));
  const usage = () =>
    res.write(
      sseChunk({
        ...base,
        choices: [],
        usage: { prompt_tokens: 10, completion_tokens: 3, total_tokens: 13 },
      }),
    );
  const done = () => {
    res.write("data: [DONE]\n\n");
    res.end();
  };

  if (script.kind === "toolThenText" && !hasToolResult(messages)) {
    // Emit a single tool call; core executes it then calls back, and the next
    // turn (with a tool result present) returns the final text.
    role();
    res.write(
      sseChunk({
        ...base,
        choices: [
          {
            index: 0,
            delta: {
              tool_calls: [
                {
                  index: 0,
                  id: "call_mock_1",
                  type: "function",
                  function: {
                    name: script.tool.name,
                    arguments: JSON.stringify(script.tool.arguments),
                  },
                },
              ],
            },
            finish_reason: null,
          },
        ],
      }),
    );
    stop("tool_calls");
    usage();
    return done();
  }

  if (script.kind === "multiTool" && !hasToolResult(messages)) {
    // Emit several tool calls in one assistant turn (distinct index + id); core
    // executes them all, then the next turn returns the final text.
    role();
    res.write(
      sseChunk({
        ...base,
        choices: [
          {
            index: 0,
            delta: {
              tool_calls: script.tools.map((t, i) => ({
                index: i,
                id: `call_mock_${i + 1}`,
                type: "function",
                function: { name: t.name, arguments: JSON.stringify(t.arguments) },
              })),
            },
            finish_reason: null,
          },
        ],
      }),
    );
    stop("tool_calls");
    usage();
    return done();
  }

  if (script.kind === "reasoning") {
    // Reasoning deltas (core reads delta.reasoning_content), then the answer.
    role();
    for (const piece of splitTwo(script.reasoning)) {
      res.write(
        sseChunk({
          ...base,
          choices: [{ index: 0, delta: { reasoning_content: piece }, finish_reason: null }],
        }),
      );
    }
    for (const piece of splitTwo(script.text)) text(piece);
    stop();
    usage();
    return done();
  }

  if (script.kind === "errorMidStream") {
    // Emit a little content, then abruptly drop the socket so core's stream read
    // fails: the client must surface a provider error and recover.
    role();
    text("partial before the error...".slice(0, script.afterChars ?? 27));
    res.destroy();
    return;
  }

  if (script.kind === "slowText") {
    // Throttled stream a spec can interrupt mid-flight. Each word is one delta,
    // spaced by perChunkMs, so the spec has a window to abort the turn.
    void streamSlow(res, script.text, script.perChunkMs, { role, text, stop, usage, done });
    return;
  }

  let reply: string;
  if (script.kind === "echo") reply = lastUserText(messages) || "ok";
  else if (script.kind === "classify") reply = script.label;
  else reply = script.text;

  role();
  // Stream in a couple of pieces to exercise the delta path.
  for (const piece of splitTwo(reply)) text(piece);
  stop();
  usage();
  done();
}

/** Split a string into the two halves the delta path expects (drops empties). */
function splitTwo(s: string): string[] {
  const mid = Math.ceil(s.length / 2);
  return [s.slice(0, mid), s.slice(mid)].filter((p) => p.length > 0);
}

interface StreamHelpers {
  role: () => void;
  text: (t: string) => void;
  stop: (reason?: string) => void;
  usage: () => void;
  done: () => void;
}

async function streamSlow(
  res: ServerResponse,
  reply: string,
  perChunkMs: number,
  h: StreamHelpers,
): Promise<void> {
  h.role();
  const words = reply.split(/(\s+)/).filter((w) => w.length > 0);
  for (const w of words) {
    if (res.writableEnded || res.destroyed) return; // client interrupted -> abort
    h.text(w);
    await new Promise((r) => setTimeout(r, perChunkMs));
  }
  if (res.writableEnded || res.destroyed) return;
  h.stop();
  h.usage();
  h.done();
}

async function handleArtifact(path: string, method: string, res: ServerResponse): Promise<void> {
  // Map the request path to a candidate file under the shared models cache.
  // HF resolve URLs look like /<user>/<repo>/resolve/<branch>/<file>; strip the
  // resolve segment so the cache lookup matches the on-disk layout.
  const rel = path.replace(/^\/+/, "").replace(/^(.*)\/resolve\/[^/]+\//, "$1/");
  const cached = join(SHARED_MODELS_DIR, rel);

  if (existsSync(cached) && statSync(cached).isFile()) {
    const size = statSync(cached).size;
    // The real LFS content sha256, so core's download-integrity check (which
    // reads `x-linked-etag` on the resolve redirect, then re-hashes the GET
    // bytes) actually runs and passes instead of being skipped.
    const sha = await artifactSha256(cached);
    res.writeHead(200, {
      "content-length": String(size),
      "x-linked-size": String(size),
      "x-linked-etag": sha,
      etag: `"${sha}"`,
      "content-type": "application/octet-stream",
    });
    if (method === "HEAD") return void res.end();
    const stream = createReadStream(cached);
    stream.on("error", () => res.destroy());
    res.on("close", () => stream.destroy());
    stream.pipe(res);
    return;
  }

  // Synthetic small payload; report the sha256 of exactly the bytes we stream so
  // the integrity check verifies them too.
  const synthetic = Buffer.from(`loca-e2e-mock-artifact:${rel}`);
  const sha = createHash("sha256").update(synthetic).digest("hex");
  res.writeHead(200, {
    "content-length": String(synthetic.length),
    "x-linked-size": String(synthetic.length),
    "x-linked-etag": sha,
    etag: `"${sha}"`,
    "content-type": "application/octet-stream",
  });
  if (method === "HEAD") return void res.end();
  res.end(synthetic);
}

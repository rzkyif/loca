// The loopback stand-in for every outbound dependency: the OpenAI-compatible
// LLM/STT/TTS surface, and the artifact host that serves the model catalog,
// release manifests, and weight downloads.
//
// This exists for two reasons, and the second is the important one. It keeps
// the suite fast and deterministic, and it keeps it honest about the repo's
// absolute rule: a Core under test must make no non-consented network request.
// Every base URL handed to a core points at 127.0.0.1, so a core that reached
// the real internet would fail its cases rather than quietly succeeding.

/** One tool call the mock model asks for.
 *
 *  `id` is the join key the whole dispatch path turns on: the core must carry it
 *  from `tool_calls[].id` on the way out to `tool_call_id` on the tool result
 *  message on the way back, and two cores that disagree about that produce a
 *  conversation the model cannot read. Name it distinctly per call when a case
 *  scripts more than one. */
export interface ScriptedToolCall {
  id: string;
  name: string;
  /** JSON text. Streamed in two pieces on purpose: providers fragment tool
   *  arguments across chunks, so a core that does not accumulate them produces
   *  invalid JSON, and a mock that sent them whole would never show it. */
  arguments: string;
}

/** What the mock LLM answers with. A case sets it with `llm` on the case.
 *
 *  A discriminated union rather than a bag of optional fields, so a script that
 *  names a kind without the data that kind needs does not type check. The mock
 *  never falls back to a default answer when a script does not fit: it fails the
 *  request loudly. A mock that quietly echoes when its script is wrong makes
 *  every case built on that script pass without exercising anything, which is
 *  D-050 case 1. */
export type LlmScript =
  /** Stream the last message's content straight back. With a tool result as the
   *  last message this echoes the TOOL's output, which is what makes a scripted
   *  hop assertable end to end: the text can only appear if the core really
   *  dispatched the call and fed the result back in the right place. */
  | { kind: "echo" }
  | { kind: "text"; text: string }
  | { kind: "error"; status?: number }
  /** Answer with tool calls and `finish_reason: "tool_calls"`. */
  | { kind: "tool_calls"; calls: ScriptedToolCall[] }
  /** One entry per upstream request, in order. This is what makes a multi-hop
   *  turn expressible: `[tool_calls, echo]` is call a tool, read the result,
   *  answer with it. Running past the end is an ERROR unless `loop` is set, so
   *  a core that hops more than the case expected is reported rather than
   *  absorbed. `loop: true` repeats the last entry forever, which is how the
   *  hop limit is reached. */
  | { kind: "sequence"; scripts: LlmScript[]; loop?: boolean };

export interface MockUpstream {
  baseUrl: string;
  /** Every request path the core has made, in order. Lets a case assert that a
   *  route did (or did not) reach out. */
  requests(): string[];
  /** Install a script and rewind it. Called once per case by the runner, so a
   *  scripted case cannot leak its script into the neighbour it shares a boot
   *  with, and a `sequence` starts at its first entry every time. */
  setLlm(script: LlmScript): void;
  close(): Promise<void>;
}

/** Reject a script that cannot answer, at the moment the case installs it.
 *
 *  Deliberately at install time rather than at request time. A malformed script
 *  that only shows up as a failed chat turn reads like a port defect; caught
 *  here it reads like what it is, and it cannot be mistaken for the core doing
 *  something wrong. */
function validateScript(script: LlmScript, where = "llm"): void {
  if (script.kind === "tool_calls") {
    if (!script.calls.length) {
      throw new Error(`${where}: a "tool_calls" script needs at least one call`);
    }
    for (const [i, call] of script.calls.entries()) {
      if (!call.id || !call.name) {
        throw new Error(`${where}.calls[${i}]: a scripted tool call needs both an id and a name`);
      }
    }
  }
  if (script.kind === "sequence") {
    if (!script.scripts.length) {
      throw new Error(`${where}: a "sequence" script needs at least one entry`);
    }
    if (script.scripts.some((s) => s.kind === "sequence")) {
      throw new Error(`${where}: a "sequence" cannot nest another sequence`);
    }
    for (const [i, s] of script.scripts.entries()) validateScript(s, `${where}.scripts[${i}]`);
  }
}

function sse(chunks: string[]): Response {
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      const enc = new TextEncoder();
      for (const c of chunks) controller.enqueue(enc.encode(c));
      controller.close();
    },
  });
  return new Response(body, {
    headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
  });
}

function chatChunk(id: string, delta: Record<string, unknown>, finish: string | null): string {
  const payload = {
    id,
    object: "chat.completion.chunk",
    created: Math.floor(Date.now() / 1000),
    model: "mock-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  };
  return `data: ${JSON.stringify(payload)}\n\n`;
}

/** The chunks for a tool-calling turn.
 *
 *  Each call announces its id and name first, then streams its arguments in two
 *  pieces. Both cores accumulate `tool_calls[].function.arguments` across chunks
 *  keyed on `index`, so a single whole-arguments chunk would leave that
 *  accumulation untested and a defect in it invisible until a real model
 *  fragmented a call. */
function toolCallChunks(id: string, calls: ScriptedToolCall[]): string[] {
  const out = [chatChunk(id, { role: "assistant", content: "" }, null)];
  for (const [index, call] of calls.entries()) {
    out.push(
      chatChunk(
        id,
        { tool_calls: [{ index, id: call.id, type: "function", function: { name: call.name } }] },
        null,
      ),
    );
    const cut = Math.floor(call.arguments.length / 2);
    const pieces =
      cut > 0 ? [call.arguments.slice(0, cut), call.arguments.slice(cut)] : [call.arguments];
    for (const piece of pieces) {
      out.push(chatChunk(id, { tool_calls: [{ index, function: { arguments: piece } }] }, null));
    }
  }
  out.push(chatChunk(id, {}, "tool_calls"));
  out.push("data: [DONE]\n\n");
  return out;
}

export async function startUpstream(): Promise<MockUpstream> {
  let llm: LlmScript = { kind: "echo" };
  /** How many chat completions this script has already answered. Reset by
   *  `setLlm`, so a `sequence` is per case rather than per boot. */
  let hop = 0;
  const seen: string[] = [];

  const handler = async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    seen.push(`${req.method} ${url.pathname}`);

    if (url.pathname === "/v1/chat/completions") {
      const at = hop++;
      let step = llm;
      if (llm.kind === "sequence") {
        const last = llm.scripts.length - 1;
        if (at > last && !llm.loop) {
          // Not an echo, and not a stall: the core asked for more turns than the
          // case scripted, which is a real finding (a hop loop that does not
          // terminate) and has to be visible as one.
          return Response.json(
            {
              error: {
                message:
                  `the mock LLM was scripted for ${llm.scripts.length} turn(s) and the ` +
                  `core asked for turn ${at + 1}. Add the turn to the sequence, or set ` +
                  `loop: true if the case is about a core that hops forever.`,
              },
            },
            { status: 500 },
          );
        }
        step = llm.scripts[Math.min(at, last)];
      }

      if (step.kind === "error") {
        return Response.json(
          { error: { message: "mock upstream failure" } },
          { status: step.status ?? 500 },
        );
      }
      const body = (await req.json()) as { messages?: { role: string; content: unknown }[] };
      const id = `chatcmpl-mock-${seen.length}`;
      if (step.kind === "tool_calls") return sse(toolCallChunks(id, step.calls));

      const last = body.messages?.at(-1);
      const text =
        step.kind === "text" ? step.text : typeof last?.content === "string" ? last.content : "";
      return sse([
        chatChunk(id, { role: "assistant", content: "" }, null),
        chatChunk(id, { content: text }, null),
        chatChunk(id, {}, "stop"),
        "data: [DONE]\n\n",
      ]);
    }

    if (url.pathname === "/v1/audio/transcriptions") {
      return Response.json({ text: "mock transcription" });
    }

    if (url.pathname === "/v1/audio/speech") {
      // A 44-byte silent WAV header is enough for a round trip.
      return new Response(new Uint8Array(44), { headers: { "content-type": "audio/wav" } });
    }

    if (url.pathname === "/v1/models") {
      return Response.json({ data: [{ id: "mock-model" }, { id: "secondary-model" }] });
    }

    // Artifact host: anything else is a download. Serve deterministic bytes so a
    // checksum-bearing case can compute its own expectation.
    return new Response(new Uint8Array(1024), {
      headers: { "content-type": "application/octet-stream", "content-length": "1024" },
    });
  };

  const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen: () => {} }, handler);
  const port = (server.addr as Deno.NetAddr).port;

  return {
    baseUrl: `http://127.0.0.1:${port}`,
    requests: () => [...seen],
    setLlm: (script) => {
      validateScript(script);
      llm = script;
      hop = 0;
    },
    close: () => server.shutdown(),
  };
}

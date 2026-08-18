// Executing one case against one core.
//
// A scenario is a named core boot shared by the cases declared against it, in
// declaration order. Cases are expected to be self-contained (create what they
// read), which is what makes sharing safe and keeps the suite fast; a case that
// mutates global state asks for `isolate: true` and pays for its own boot.

import type { CoreUnderTest } from "../harness/core.ts";
import { upgrade } from "../harness/ws.ts";
import { pair, type PairedIdentity, wsTicket } from "../harness/pair.ts";
import { pinFromHome } from "../harness/tls.ts";
import { compare, type Mismatch, type NormaliseContext } from "./match.ts";
import { FrameWatcher } from "./watch.ts";
import type { Auth, Case, Expect, FormPart, Json, RequestSpec, Step } from "./types.ts";

export interface Identities {
  primary: PairedIdentity;
  other?: PairedIdentity;
}

export interface StepOutcome {
  index: number;
  label: string;
  /** Non-fatal remark, e.g. a broadcast that arrived close to its bound. */
  note?: string;
  mismatches: Mismatch[];
  /** The normalised response body, kept for differential mode. */
  observed: Json;
  status: number;
}

export interface CaseOutcome {
  case: Case;
  passed: boolean;
  steps: StepOutcome[];
  error?: string;
  durationMs: number;
}

/** Pair the identities a scenario needs, once per boot. */
export async function establishIdentities(
  core: CoreUnderTest,
  needOther: boolean,
): Promise<Identities> {
  const primary = await pair(core, "conformance");
  const other = needOther ? await pair(core, "conformance-other") : undefined;
  return { primary, other };
}

function substitute(text: string, vars: Record<string, string>): string {
  return text.replace(/\$\{([A-Za-z0-9_]+)\}/g, (whole, name: string) => {
    if (!(name in vars)) throw new Error(`no captured value named "${name}" (in ${whole})`);
    return vars[name];
  });
}

function substituteDeep(value: Json, vars: Record<string, string>): Json {
  if (typeof value === "string") return substitute(value, vars);
  if (Array.isArray(value)) return value.map((v) => substituteDeep(v, vars));
  if (value && typeof value === "object") {
    const out: Record<string, Json> = {};
    for (const [k, v] of Object.entries(value)) out[k] = substituteDeep(v, vars);
    return out;
  }
  return value;
}

function authHeaders(auth: Auth, core: CoreUnderTest, ids: Identities): Record<string, string> {
  switch (auth) {
    case "none":
      return {};
    case "admin":
      return { "x-admin-token": core.adminToken };
    case "paired":
      return { authorization: `Bearer ${ids.primary.token}` };
    case "paired:other": {
      if (!ids.other)
        throw new Error("this case needs a second paired client but none was created");
      return { authorization: `Bearer ${ids.other.token}` };
    }
  }
}

function extract(body: Json, path: string): string {
  // Dot segments plus `[n]` indices, so a case can capture out of a list
  // response. Deliberately not a JSONPath engine: `$[?(@.id=='x')]` reads like
  // it should work and does not, so the bracket form is the whole extension and
  // anything more expressive should become a route that returns one object.
  const parts = path
    .replace(/^\$\.?/, "")
    .replace(/\[(\d+)\]/g, ".$1")
    .split(".")
    .filter(Boolean);
  let cur: unknown = body;
  for (const p of parts) {
    if (cur === null || typeof cur !== "object") {
      throw new Error(`capture path ${path} does not resolve in the response body`);
    }
    cur = (cur as Record<string, unknown>)[p];
  }
  if (cur === undefined || cur === null) {
    throw new Error(`capture path ${path} resolved to ${String(cur)}`);
  }
  return String(cur);
}

async function sha256Hex(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function buildForm(parts: FormPart[], vars: Record<string, string>): FormData {
  const form = new FormData();
  for (const part of parts) {
    if (part.file) {
      const { content, encoding, filename, contentType } = part.file;
      const bytes =
        encoding === "base64"
          ? Uint8Array.from(atob(content), (c) => c.charCodeAt(0))
          : new TextEncoder().encode(substitute(content, vars));
      form.append(
        part.name,
        new File([bytes], substitute(filename, vars), {
          type: contentType ?? "application/octet-stream",
        }),
      );
      continue;
    }
    form.append(part.name, substitute(part.value ?? "", vars));
  }
  return form;
}

async function readBody(res: Response): Promise<Json> {
  const type = res.headers.get("content-type") ?? "";
  if (res.status === 204) {
    await res.body?.cancel();
    return null;
  }
  const text = await res.text();
  if (!text) return null;
  if (type.includes("application/json")) {
    try {
      return JSON.parse(text) as Json;
    } catch {
      return text;
    }
  }
  return text;
}

/** Collect the `data:` payloads of an SSE response until the stream ends. */
async function readSse(res: Response, timeoutMs: number): Promise<Json[]> {
  const out: Json[] = [];
  if (!res.body) return out;
  const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
  const deadline = Date.now() + timeoutMs;
  let buf = "";
  try {
    for (;;) {
      const remaining = Math.max(0, deadline - Date.now());
      const chunk = await Promise.race([
        reader.read(),
        new Promise<{ done: true; value?: undefined }>((r) =>
          setTimeout(() => r({ done: true }), remaining),
        ),
      ]);
      if (chunk.done) break;
      buf += chunk.value ?? "";
      const parts = buf.split("\n\n");
      buf = parts.pop() ?? "";
      for (const part of parts) {
        for (const line of part.split("\n")) {
          if (!line.startsWith("data:")) continue;
          const payload = line.slice(5).trim();
          if (payload === "[DONE]") continue;
          try {
            out.push(JSON.parse(payload) as Json);
          } catch {
            out.push(payload);
          }
        }
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
  return out;
}

async function runWsStep(
  core: CoreUnderTest,
  expect: Expect,
  ids: Identities,
  ctx: NormaliseContext,
  vars: Record<string, string>,
): Promise<{ mismatches: Mismatch[]; observed: Json; status: number }> {
  const spec = expect.ws!;
  const mode = spec.ticket ?? "valid";
  let ticket: string | undefined;
  if (mode === "valid") ticket = await wsTicket(core, ids.primary.token);
  else if (mode === "invalid") ticket = "not-a-real-ticket";
  else if (mode === "spent") {
    ticket = await wsTicket(core, ids.primary.token);
    // Burn it: the first upgrade consumes it, so the second must be refused.
    const first = await upgrade("127.0.0.1", core.port, "/ws/v1", ticket);
    await first.socket?.close();
  }

  const res = await upgrade("127.0.0.1", core.port, "/ws/v1", ticket);
  const wantStatus = spec.status ?? 101;
  const mismatches: Mismatch[] = [];
  if (res.status !== wantStatus) {
    mismatches.push({
      path: "$.upgrade",
      detail: `expected HTTP ${wantStatus} on the upgrade, got ${res.status} ${res.statusText}`,
    });
  }
  if (!res.socket) return { mismatches, observed: null, status: res.status };

  const received: Json[] = [];
  try {
    // Frames carry `${captured}` like every other part of a case: a WS turn
    // almost always addresses a session an earlier HTTP step created, and
    // sending the literal `${sid}` would be answered with a not-found rather
    // than with the failure the author was looking for.
    for (const f of spec.send ?? []) await res.socket.send(JSON.stringify(substituteDeep(f, vars)));

    const wanted = (spec.expect ?? []).map((f) => substituteDeep(f, vars));
    if (wanted.length) {
      const kinds = new Set(
        wanted.map((f) => (f as { kind?: string }).kind).filter((k): k is string => !!k),
      );
      const timeout = expect.timeoutMs ?? 10_000;
      const deadline = Date.now() + timeout;
      // An ordered subsequence filtered to the kinds this case named. The core
      // interleaves heartbeats and snapshot broadcasts whose timing is not
      // deterministic, so a full-transcript match would be flaky by design.
      while (received.length < wanted.length && Date.now() < deadline) {
        const text = await res.socket.next(Math.max(0, deadline - Date.now()));
        if (text === null) break;
        let frame: Json;
        try {
          frame = JSON.parse(text) as Json;
        } catch {
          continue;
        }
        const kind = (frame as { kind?: string }).kind;
        if (kind && kinds.has(kind)) received.push(frame);
      }
      mismatches.push(...compare(received, wanted, ctx, {}));
    }
  } finally {
    await res.socket.close();
  }
  return { mismatches, observed: received, status: res.status };
}

async function runHttpStep(
  core: CoreUnderTest,
  request: RequestSpec,
  expect: Expect,
  caseAuth: Auth,
  ids: Identities,
  ctx: NormaliseContext,
  vars: Record<string, string>,
): Promise<{ mismatches: Mismatch[]; observed: Json; status: number }> {
  const path = substitute(request.path, vars);
  const headers: Record<string, string> = {
    ...authHeaders(request.auth ?? caseAuth, core, ids),
    ...request.headers,
  };
  let body: string | FormData | undefined;
  if (request.form) {
    // fetch generates the boundary; setting content-type by hand would omit it.
    body = buildForm(request.form, vars);
  } else if (request.body !== undefined) {
    body = JSON.stringify(substituteDeep(request.body, vars));
    headers["content-type"] ??= "application/json";
  }

  const res = await fetch(`${core.baseUrl}${path}`, {
    method: request.method,
    headers,
    body,
    signal: AbortSignal.timeout(expect.timeoutMs ?? 20_000),
  });

  const mismatches: Mismatch[] = [];
  if (expect.status !== undefined && res.status !== expect.status) {
    mismatches.push({
      path: "$.status",
      detail: `expected HTTP ${expect.status}, got ${res.status}`,
    });
  }
  for (const [name, want] of Object.entries(expect.headers ?? {})) {
    const got = res.headers.get(name);
    if (got !== want) {
      mismatches.push({ path: `$.headers.${name}`, detail: `expected ${want}, got ${got}` });
    }
  }

  let observed: Json;
  if (expect.bodyBytes !== undefined || expect.bodySha256 !== undefined) {
    // Raw bytes: an attachment download is binary and res.text() would mangle it.
    const bytes = new Uint8Array(await res.arrayBuffer());
    if (expect.bodyBytes !== undefined && bytes.length !== expect.bodyBytes) {
      mismatches.push({
        path: "$.bodyBytes",
        detail: `expected ${expect.bodyBytes} bytes, got ${bytes.length}`,
      });
    }
    if (expect.bodySha256 !== undefined) {
      const got = await sha256Hex(bytes);
      if (got !== expect.bodySha256) {
        mismatches.push({
          path: "$.bodySha256",
          detail: `expected ${expect.bodySha256}, got ${got}`,
        });
      }
    }
    return { mismatches, observed: { bytes: bytes.length }, status: res.status };
  }
  if (expect.sse) {
    observed = await readSse(res, expect.timeoutMs ?? 20_000);
    mismatches.push(...compare(observed, expect.sse, ctx, {}));
    return { mismatches, observed, status: res.status };
  }

  observed = await readBody(res);
  if (expect.body !== undefined) {
    mismatches.push(
      ...compare(observed, substituteDeep(expect.body, vars), ctx, {
        ignorePaths: expect.ignorePaths,
        unordered: expect.unordered,
      }),
    );
  } else if (expect.bodyContains !== undefined) {
    mismatches.push(
      ...compare(observed, substituteDeep(expect.bodyContains, vars), ctx, {
        subset: true,
        ignorePaths: expect.ignorePaths,
        unordered: expect.unordered,
      }),
    );
  }
  return { mismatches, observed, status: res.status };
}

export async function runCase(
  core: CoreUnderTest,
  ids: Identities,
  kase: Case,
  startedAtMs: number,
): Promise<CaseOutcome> {
  const began = performance.now();
  const ctx: NormaliseContext = {
    port: core.port,
    home: core.home,
    upstream: core.upstream.baseUrl,
    startedAtMs,
  };
  // Seeded rather than empty. `${home}` is the request-side counterpart of the
  // `<home>` the normaliser writes into responses, and a case that PLANTS a
  // fixture needs to name it in a request: the home is a fresh temp dir, so its
  // path cannot be written literally in a case file. Overwritable by `capture`,
  // which is what makes it a default rather than a reserved word.
  const vars: Record<string, string> = { home: core.home };
  const steps: StepOutcome[] = [];

  // Opened BEFORE the first step, so a broadcast triggered by any step is
  // already buffered by the time its assertion runs.
  let watcher: FrameWatcher | null = null;
  if (kase.watch?.length) {
    const ticket = await wsTicket(core, ids.primary.token);
    const up = await upgrade("127.0.0.1", core.port, "/ws/v1", ticket);
    if (!up.socket) {
      return {
        case: kase,
        passed: false,
        steps,
        error: `could not open the watch connection: HTTP ${up.status} ${up.statusText}`,
        durationMs: performance.now() - began,
      };
    }
    watcher = new FrameWatcher(up.socket, new Set(kase.watch));
  }

  try {
    // Installed for every case, scripted or not, so a script never outlives the
    // case that declared it and a `sequence` always starts at its first entry.
    // Inside the try: a malformed script fails THIS case with the validator's
    // message rather than taking the whole scenario down with it.
    core.upstream.setLlm(kase.llm ?? { kind: "echo" });

    for (const [index, step] of kase.steps.entries()) {
      const handoffMismatches: Mismatch[] = [];
      if (step.handoff) {
        // Exhaustive on purpose. A hand-off kind that fell through to `restart`
        // would bring the SAME core back and satisfy every assertion a cutover
        // case makes, because those assertions describe a restart too: the case
        // would be green and would have tested nothing (D-050).
        switch (step.handoff) {
          case "restart":
            await core.kill();
            await core.restart();
            break;
          case "swap":
            await core.swap();
            break;
          default: {
            const never: never = step.handoff;
            throw new Error(`unhandled handoff kind: ${String(never)}`);
          }
        }
        if (step.expect.pinUnchanged) {
          const after = await pinFromHome(core.home);
          if (after !== core.tlsPin) {
            handoffMismatches.push({
              path: "$.tlsPin",
              detail:
                `the SPKI pin changed across the hand-off: ${core.tlsPin} became ${after}. ` +
                `Every already-paired client pinned the old value.`,
            });
          }
        }
      }

      const result =
        !step.request && !step.expect.ws
          ? { mismatches: [], observed: null as Json, status: 0 }
          : step.expect.ws
            ? await runWsStep(core, step.expect, ids, ctx, vars)
            : await runHttpStep(core, requireRequest(step), step.expect, kase.auth, ids, ctx, vars);
      result.mismatches = [...handoffMismatches, ...result.mismatches];

      // Broadcast assertions run AFTER the triggering request has returned, but
      // read a buffer that has been filling since before it was sent.
      let note: string | undefined;
      if (step.expect.frames || step.expect.noFrames) {
        if (!watcher) {
          throw new Error(
            "this step asserts on frames but the case has no `watch` list; " +
              "add the frame kinds to watch at the case level",
          );
        }
        const fr = step.expect.frames
          ? await watcher.expect(step.expect.frames, ctx, step.expect.frameTimeoutMs ?? 5_000)
          : await watcher.expectNone(step.expect.noFrames!, step.expect.frameTimeoutMs ?? 1_000);
        result.mismatches = [...result.mismatches, ...fr.mismatches];
        note = fr.warning;
      }

      steps.push({
        note,
        index,
        label:
          // Name the core that is answering NOW, so a failure report sends the
          // reader to the right codebase.
          (step.handoff === "swap"
            ? `[swap -> ${core.target}] `
            : step.handoff
              ? `[${step.handoff}] `
              : "") +
          (step.expect.ws
            ? `WS /ws/v1 (ticket: ${step.expect.ws.ticket ?? "valid"})`
            : step.request
              ? `${step.request.method} ${substitute(step.request.path, vars)}`
              : "hand-off only"),
        mismatches: result.mismatches,
        observed: result.observed,
        status: result.status,
      });

      for (const [name, path] of Object.entries(step.capture ?? {})) {
        vars[name] = extract(result.observed, path);
      }
      // Stop at the first failing step: later steps depend on it and their
      // failures would be noise.
      if (result.mismatches.length) break;
    }
  } catch (err) {
    return {
      case: kase,
      passed: false,
      steps,
      error: err instanceof Error ? err.message : String(err),
      durationMs: performance.now() - began,
    };
  } finally {
    // Always close the watch connection: a leaked socket keeps the core's hub
    // alive and the next scenario's teardown hangs waiting for it.
    await watcher?.stop();
  }

  return {
    case: kase,
    passed: steps.every((s) => s.mismatches.length === 0),
    steps,
    durationMs: performance.now() - began,
  };
}

function requireRequest(step: Step): RequestSpec {
  if (!step.request) throw new Error("a step needs either `request` or `expect.ws`");
  return step.request;
}

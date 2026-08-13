// Module broker: gates and dispatches tool module_request frames (memories,
// db, llm, tts, stt) coming over the worker stdio protocol. The non-PTY
// counterpart of the runtime permission prompt flow: the access check runs
// here at the core API layer, and an ask-state (or undeclared, policy `ask`)
// access pauses the call on the SAME client prompt UI via the pool's
// `permission_request` CallEvent (`promptUser`).

import type { HostDb } from "@loca/core-engine";
import { openDenoDb } from "../db/deno-sqlite.ts";
import { encodeBase64 } from "@std/encoding/base64";
import { join } from "@std/path";
import type { PermissionDecl, PermissionKind } from "@loca/shared";
import { errMessage, permissionKey } from "@loca/shared";
import type { ModuleName } from "../extensions/worker-protocol.ts";
import { extensionsRegistry } from "../extensions/registry.ts";
import { synthesizeSpeech } from "@loca/core-engine/services/tts-synthesize";
import { STT_MAX_AUDIO_BYTES, TTS_MAX_TEXT_CHARS } from "@loca/core-engine/services/media-limits";
import { extensionDataDir } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { loadCoreSettings } from "@loca/core-engine/services/core-settings";
import { memoriesStore } from "@loca/core-engine/services/memories-store";
import { scheduleMemoryIndexing } from "./memories-indexer.ts";
import { resolveEndpoint } from "@loca/core-engine/services/endpoint-resolver";
import { llmIdle } from "./llm-idle.ts";
import { singleShot } from "@loca/core-engine/services/single-shot";
import { transcribeAudio } from "@loca/core-engine/services/stt-transcribe";

const log = getLogger("modulebroker");

// The user-facing prompt raised for an ungated module access. Mirrors the
// `permission_request` CallEvent fields the pool forwards to chat.
export interface ModulePrompt {
  permission: PermissionKind;
  // What is being touched: the access ("read" | "write") for memories,
  // empty for the all-or-nothing modules.
  resource: string;
  declared: boolean;
  reason?: string;
}

export interface ModuleRequest {
  extensionId: string;
  toolName: string;
  callId: string;
  module: ModuleName;
  op: string;
  args: unknown;
  // Forward an ask-state access to the user; resolves with their decision.
  // The grant is scoped to this tool call only ("always allow" lives in the
  // Extension detail view, exactly as for Deno prompt permissions).
  promptUser: (prompt: ModulePrompt) => Promise<boolean>;
}

/** Gate + dispatch one module request. Resolves with the op's result;
 *  rejects with AppError("forbidden") when access is denied. */
export async function handleModuleRequest(req: ModuleRequest): Promise<unknown> {
  await gate(req);
  return await dispatch(req);
}

// --- gating ----------------------------------------------------------------

async function gate(req: ModuleRequest): Promise<void> {
  if (req.module === "db") {
    // db is declaration-gated only: the extension opted in via `database: true`
    // in its loca.json, which the user saw at install time. No grant rows.
    const extension = extensionsRegistry().getOrThrow(req.extensionId);
    if (!extension.hasDatabase) {
      throw new AppError(
        "forbidden",
        `extension ${req.extensionId} does not declare "database": true`,
      );
    }
    return;
  }

  const permission: PermissionKind = req.module;
  const access = req.module === "memories" ? memoriesAccessFor(req.op) : undefined;
  const resource = access ?? "";

  const tool = extensionsRegistry()
    .listTools(req.extensionId)
    .find((t) => t.name === req.toolName);
  const decl = tool?.requiredPermissions.find((d) => declCovers(d, permission, access));

  if (!decl) {
    const extension = extensionsRegistry().getOrThrow(req.extensionId);
    if (extension.undeclaredPolicy === "deny") {
      throw new AppError("forbidden", `undeclared ${permission} access denied by extension policy`);
    }
    const allow = await req.promptUser({
      permission,
      resource,
      declared: false,
    });
    if (!allow) {
      throw new AppError("forbidden", `${permission} access rejected by user`);
    }
    return;
  }

  const state = tool!.grants.find((g) => g.permissionKey === permissionKey(decl))?.state ?? "ask";
  if (state === "denied") {
    throw new AppError("forbidden", `${permission} access denied by grant`);
  }
  if (state !== "granted") {
    const allow = await req.promptUser({
      permission,
      resource,
      declared: true,
      reason: decl.reason,
    });
    if (!allow) {
      throw new AppError("forbidden", `${permission} access rejected by user`);
    }
  }
}

function memoriesAccessFor(op: string): "read" | "write" {
  switch (op) {
    case "list":
    case "get":
    case "getFile":
      return "read";
    case "write":
    case "edit":
      return "write";
    default:
      // Unknown ops fail at dispatch; gate them at the stricter access so a
      // future op can never slip through under a read grant.
      return "write";
  }
}

// A memories:write grant covers read ops too: write is strictly broader,
// and a tool that can edit memories can trivially read them through edit
// errors anyway, so requiring a second grant would be ceremony, not safety.
function declCovers(
  decl: PermissionDecl,
  permission: PermissionKind,
  access: "read" | "write" | undefined,
): boolean {
  if (decl.kind !== permission) return false;
  if (decl.kind !== "memories") return true;
  return decl.access === access || (decl.access === "write" && access === "read");
}

// --- dispatch ---------------------------------------------------------------

function dispatch(req: ModuleRequest): Promise<unknown> {
  switch (req.module) {
    case "memories":
      return dispatchMemories(req.op, req.args);
    case "db":
      return Promise.resolve(dispatchDb(req.extensionId, req.toolName, req.op, req.args));
    case "llm":
      return dispatchLlm(req.op, req.args);
    case "tts":
      return dispatchTts(req.op, req.args);
    case "stt":
      return dispatchStt(req.op, req.args);
    default:
      // The worker speaks a typed ModuleName, so this is unreachable in
      // practice; reject rather than resolve with undefined if a malformed
      // module string ever reaches here.
      throw new AppError("validation_error", `unknown module "${req.module}"`);
  }
}

// Tool-facing memories API is title-keyed: tools never see row ids, and
// titles are what the model knows from the prompt's relevant-memories list.
async function dispatchMemories(op: string, args: unknown): Promise<unknown> {
  const store = memoriesStore();
  // A disabled memory is silenced everywhere it would reach the model: not just
  // hidden from auto-relevance and @-triggers but also unreadable through these
  // tools, so "disabled" can't be bypassed by the agent reading it by title.
  switch (op) {
    case "list":
      return store
        .list()
        .filter((m) => m.enabled)
        .map((m) => ({
          title: m.title,
          kind: m.kind,
          summary: m.summary,
          updatedAtMs: m.updatedAtMs,
        }));
    case "get": {
      const memory = await store.getByTitle(argString(args, "title"));
      if (!memory || !memory.enabled) {
        throw new AppError("not_found", `memory "${argString(args, "title")}" not found`);
      }
      return { title: memory.title, content: memory.content };
    }
    case "getFile": {
      const title = argString(args, "title");
      const name = argString(args, "name");
      const memory = await store.getByTitle(title);
      if (!memory || !memory.enabled) {
        throw new AppError("not_found", `memory "${title}" not found`);
      }
      return {
        title: memory.title,
        name,
        content: await store.getFile(memory.id, name),
      };
    }
    case "write": {
      const title = argString(args, "title");
      const content = argString(args, "content", { allowEmpty: true });
      const existing = await store.getByTitle(title);
      const before = existing?.content ?? "";
      const memory = existing
        ? await store.replaceContent(existing.id, content)
        : await store.create("knowledge", title, content);
      scheduleMemoryIndexing(memory.id);
      return {
        title: memory.title,
        before,
        after: content,
        created: !existing,
      };
    }
    case "edit": {
      const title = argString(args, "title");
      const existing = await store.getByTitle(title);
      if (!existing) {
        throw new AppError("not_found", `memory "${title}" not found`);
      }
      const { memory, before, after } = await store.editContent(
        existing.id,
        argString(args, "find"),
        argString(args, "replace", { allowEmpty: true }),
      );
      scheduleMemoryIndexing(memory.id);
      return { title: memory.title, before, after };
    }
    default:
      throw new AppError("validation_error", `unknown memories op "${op}"`);
  }
}

function argString(args: unknown, key: string, opts: { allowEmpty?: boolean } = {}): string {
  const value = (args as Record<string, unknown> | null)?.[key];
  if (typeof value !== "string" || (!opts.allowEmpty && value.length === 0)) {
    throw new AppError("validation_error", `module op requires string argument "${key}"`);
  }
  return value;
}

// --- db ----------------------------------------------------------------

// One core-owned SQLite handle per extension, opened on first use under
// extension-data/<id>/data.sqlite and proxied to the sandboxed worker over
// stdio (workers never get fs access to it). Bounded LRU: a core that has run
// many extensions over its lifetime would otherwise hold a file handle (and WAL
// memory) for every one forever. Map insertion order is the LRU order; a used
// handle is re-inserted to the most-recent end, and opening past the cap closes
// the least-recently-used. Eviction is safe because each dispatch reopens the
// handle on demand (data lives in the WAL-backed file, not the handle) and a
// dispatch runs synchronously start to finish, so no handle is mid-use across
// an eviction.
const EXTENSION_DB_MAX_OPEN = 16;
const extensionDbs = new Map<string, HostDb>();

type DbBindValue = string | number | boolean | null;

function extensionDb(extensionId: string): HostDb {
  const existing = extensionDbs.get(extensionId);
  if (existing) {
    // Touch: move to the most-recently-used end of the Map.
    extensionDbs.delete(extensionId);
    extensionDbs.set(extensionId, existing);
    return existing;
  }
  // At capacity: evict (close) the least-recently-used handle first.
  if (extensionDbs.size >= EXTENSION_DB_MAX_OPEN) {
    const oldest = extensionDbs.keys().next().value;
    if (oldest !== undefined) closeExtensionDb(oldest);
  }
  const dir = extensionDataDir(extensionId);
  Deno.mkdirSync(dir, { recursive: true });
  // Match the core connection (connection.ts): int64 so INTEGER columns
  // round-trip values past 2^31 (e.g. Date.now() ms timestamps a extension
  // stores), plus the same per-connection pragmas so a extension's private db
  // behaves like the rest of core.
  const db = openDenoDb(join(dir, "data.sqlite"));
  db.exec("PRAGMA journal_mode = WAL;");
  db.exec("PRAGMA foreign_keys = ON;");
  // Shorter than core's 5s: extension queries run synchronously on core's event
  // loop, so a contended lock blocks the whole loop for up to this long. A
  // private per-extension db is rarely contended, so 1s fails a stuck write fast
  // rather than freezing core for 5s.
  db.exec("PRAGMA busy_timeout = 1000;");
  extensionDbs.set(extensionId, db);
  return db;
}

// Close a cached handle and drop it from the map. Tolerates an already-closed
// handle (close throws then). Used by both LRU eviction and uninstall cleanup.
function closeExtensionDb(extensionId: string): void {
  const db = extensionDbs.get(extensionId);
  if (!db) return;
  try {
    db.close();
  } catch {
    /* already closed */
  }
  extensionDbs.delete(extensionId);
}

/** Close the cached handle and remove a extension's private data dir. Called
 *  by the installer on uninstall. */
export function deleteExtensionData(extensionId: string): void {
  closeExtensionDb(extensionId);
  try {
    Deno.removeSync(extensionDataDir(extensionId), { recursive: true });
  } catch (err) {
    if (!(err instanceof Deno.errors.NotFound)) throw err;
  }
}

function dbParams(args: unknown): DbBindValue[] {
  const params = (args as Record<string, unknown> | null)?.params ?? [];
  if (!Array.isArray(params)) {
    throw new AppError("validation_error", `db op "params" must be an array`);
  }
  return params.map((p) => {
    if (p === null || typeof p === "string" || typeof p === "number" || typeof p === "boolean") {
      return p;
    }
    throw new AppError("validation_error", "db params must be strings, numbers, booleans, or null");
  });
}

// Extension SQL runs synchronously in the core process, so it must stay inside
// the extension's own database file: ATTACH/DETACH would open arbitrary database
// files under core's privileges (core.sqlite included) and VACUUM INTO writes
// a database to an arbitrary path. All three are rejected lexically; string
// literals, quoted identifiers, and comments are stripped first so a
// VALUES ('attach') row never false-positives.
const SQL_QUERY_MAX_ROWS = 10_000;
const SQL_SLOW_WARN_MS = 1_000;

function assertSqlAllowed(extensionId: string, toolName: string, sql: string): void {
  const screened = stripSqlLiterals(sql).toUpperCase();
  const blocked =
    /\bATTACH\b|\bDETACH\b/.test(screened) || /\bVACUUM\b[^;]*\bINTO\b/.test(screened);
  if (blocked) {
    log.warn(`blocked db statement from ${extensionId}/${toolName}: ${sqlSnippet(sql)}`);
    throw new AppError(
      "forbidden",
      "ATTACH, DETACH, and VACUUM INTO are not allowed in extension databases",
    );
  }
}

// Replace string literals ('..' with '' escapes), quoted identifiers ("..",
// `..`, [..]), and comments (-- line, /* block */) with a space so the
// keyword screen only sees real SQL tokens.
function stripSqlLiterals(sql: string): string {
  let out = "";
  let i = 0;
  while (i < sql.length) {
    const ch = sql[i];
    if (ch === "'" || ch === '"' || ch === "`") {
      i++;
      while (i < sql.length) {
        if (sql[i] === ch) {
          if (sql[i + 1] === ch) {
            i += 2; // doubled quote escapes itself
            continue;
          }
          i++;
          break;
        }
        i++;
      }
      out += " ";
      continue;
    }
    if (ch === "[") {
      const end = sql.indexOf("]", i + 1);
      i = end === -1 ? sql.length : end + 1;
      out += " ";
      continue;
    }
    if (ch === "-" && sql[i + 1] === "-") {
      const end = sql.indexOf("\n", i);
      i = end === -1 ? sql.length : end + 1;
      out += " ";
      continue;
    }
    if (ch === "/" && sql[i + 1] === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end === -1 ? sql.length : end + 2;
      out += " ";
      continue;
    }
    out += ch;
    i++;
  }
  return out;
}

function sqlSnippet(sql: string): string {
  return sql.length > 200 ? sql.slice(0, 200) + "…" : sql;
}

// The db opens with int64, so INTEGER values past 2^53 come back as BigInt,
// which JSON.stringify rejects; the stdio protocol is JSON, so map safe
// magnitudes to number and the rest to decimal strings.
function jsonSafe(value: unknown): unknown {
  if (typeof value === "bigint") {
    return value >= BigInt(Number.MIN_SAFE_INTEGER) && value <= BigInt(Number.MAX_SAFE_INTEGER)
      ? Number(value)
      : value.toString();
  }
  if (Array.isArray(value)) return value.map(jsonSafe);
  if (value !== null && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) out[k] = jsonSafe(v);
    return out;
  }
  return value;
}

function dispatchDb(extensionId: string, toolName: string, op: string, args: unknown): unknown {
  const sql = argString(args, "sql");
  const params = dbParams(args);
  assertSqlAllowed(extensionId, toolName, sql);
  const db = extensionDb(extensionId);
  // Execution is synchronous on core's event loop; attribute failures and
  // slow statements to the extension/tool so a faulty query is identifiable.
  const startedAt = Date.now();
  try {
    switch (op) {
      case "query": {
        const rows = db.prepare(sql).all(...params);
        if (rows.length > SQL_QUERY_MAX_ROWS) {
          log.warn(
            `db query from ${extensionId}/${toolName} returned ${rows.length} rows (cap ${SQL_QUERY_MAX_ROWS}): ${sqlSnippet(
              sql,
            )}`,
          );
          throw new AppError(
            "validation_error",
            `query returned more than ${SQL_QUERY_MAX_ROWS} rows; narrow it with LIMIT`,
          );
        }
        return jsonSafe(rows);
      }
      case "execute": {
        const changes = db.prepare(sql).run(...params);
        // Read the rowid via SQL against the int64 connection so a value past
        // 2^53 comes back as BigInt (jsonSafe handles it) rather than a
        // precision-lost JS number from the db.lastInsertRowId accessor.
        const idRow = db.prepare("SELECT last_insert_rowid() AS id").get() as {
          id: number | bigint;
        };
        return jsonSafe({ changes, lastInsertRowId: idRow.id });
      }
      default:
        throw new AppError("validation_error", `unknown db op "${op}"`);
    }
  } catch (err) {
    if (!(err instanceof AppError)) {
      log.warn(
        `db ${op} from ${extensionId}/${toolName} failed: ${errMessage(err)}; sql: ${sqlSnippet(
          sql,
        )}`,
      );
    }
    throw err;
  } finally {
    const elapsed = Date.now() - startedAt;
    if (elapsed >= SQL_SLOW_WARN_MS) {
      log.warn(`slow db ${op} (${elapsed}ms) from ${extensionId}/${toolName}: ${sqlSnippet(sql)}`);
    }
  }
}

// --- llm ---------------------------------------------------------------

// Per-call output cap: tools get short completions (summaries, labels),
// not long-form generation; the chat path owns that.
const LLM_COMPLETE_MAX_TOKENS = 1024;
const LLM_COMPLETE_DEFAULT_TOKENS = 512;

async function dispatchLlm(op: string, args: unknown): Promise<unknown> {
  if (op !== "complete") {
    throw new AppError("validation_error", `unknown llm op "${op}"`);
  }
  const prompt = argString(args, "prompt");
  const systemPrompt = (args as Record<string, unknown>)?.systemPrompt;
  const maxTokens = (args as Record<string, unknown>)?.maxTokens;
  const settings = await loadCoreSettings();
  await llmIdle().ensureLoaded(settings);
  const endpoint = await resolveEndpoint(settings, "default");
  const text = await singleShot({
    systemPrompt: typeof systemPrompt === "string" ? systemPrompt : "",
    userMessage: prompt,
    endpoint,
    overrides: {
      maxTokens: Math.min(
        typeof maxTokens === "number" && maxTokens > 0
          ? Math.floor(maxTokens)
          : LLM_COMPLETE_DEFAULT_TOKENS,
        LLM_COMPLETE_MAX_TOKENS,
      ),
      // Extension completions are short and expect a clean answer, so thinking is
      // off (the model's `<think>` text would otherwise land in the returned
      // string). The tool author bounds length via maxTokens.
      reasoningBudget: 0,
    },
  });
  return { text };
}

// --- tts / stt -----------------------------------------------------------

// The user's tts.voice lives on the client and isn't visible to the broker, so
// extension synthesis uses the schema-default voice (matching the old fallback).
const BROKER_TTS_VOICE = "af_bella";

async function dispatchTts(op: string, args: unknown): Promise<unknown> {
  if (op !== "speak") {
    throw new AppError("validation_error", `unknown tts op "${op}"`);
  }
  const settings = await loadCoreSettings();
  if (settings["tts.enabled"] !== true) {
    throw new AppError("server_unavailable", "Text-to-Speech is turned off in settings");
  }
  const text = argString(args, "text");
  if (text.length > TTS_MAX_TEXT_CHARS) {
    throw new AppError("validation_error", `tts text exceeds ${TTS_MAX_TEXT_CHARS} characters`);
  }
  const wav = await synthesizeSpeech(text, BROKER_TTS_VOICE);
  return {
    dataB64: encodeBase64(wav),
    mime: "audio/wav",
    sampleRate: wavSampleRate(wav),
  };
}

// WAV sample rate is a little-endian uint32 at byte offset 24; Kokoro is 24 kHz.
function wavSampleRate(wav: Uint8Array): number {
  if (wav.length < 28) return 24_000;
  return new DataView(wav.buffer, wav.byteOffset, wav.byteLength).getUint32(24, true);
}

async function dispatchStt(op: string, args: unknown): Promise<unknown> {
  if (op !== "transcribe") {
    throw new AppError("validation_error", `unknown stt op "${op}"`);
  }
  const settings = await loadCoreSettings();
  if (settings["stt.enabled"] !== true) {
    throw new AppError("server_unavailable", "Speech-to-Text is turned off in settings");
  }
  const dataB64 = argString(args, "dataB64");
  const mimeRaw = (args as Record<string, unknown>)?.mime;
  const mime = typeof mimeRaw === "string" && mimeRaw.length > 0 ? mimeRaw : "audio/wav";
  let bytes: Uint8Array;
  try {
    bytes = Uint8Array.from(atob(dataB64), (ch) => ch.charCodeAt(0));
  } catch {
    throw new AppError("validation_error", "stt dataB64 is not valid base64");
  }
  if (bytes.byteLength > STT_MAX_AUDIO_BYTES) {
    throw new AppError("validation_error", "stt audio exceeds the 25 MB limit");
  }
  const file = new File([bytes.buffer as ArrayBuffer], "audio", { type: mime });
  const language = (args as Record<string, unknown>)?.language;
  const text = await transcribeAudio(
    settings,
    file,
    typeof language === "string" && language.length > 0 ? language : undefined,
  );
  return { text };
}

import { Hono, type MiddlewareHandler } from "hono";
import { join } from "@std/path";
import type OpenAI from "openai";
import type { Message } from "@loca/shared";
import {
  contentToText,
  errMessage,
  messageInputSchema,
  messagePatchSchemaByRole,
  type MessageRoleForPatch,
} from "@loca/shared";
import { sessionsRepo } from "../../services/sessions-store.ts";
import { numSetting, strSetting } from "../../services/settings-access.ts";
import { sessionAttachmentsDir } from "../../platform/paths.ts";
import { host } from "../../platform/runtime.ts";
import { AppError } from "../../platform/errors.ts";
import { newAttachmentId, newMessageId } from "../../platform/ids.ts";
import { parseBody, readJson } from "../body.ts";
import { requireClient } from "../middleware/auth.ts";
import { loadCoreSettings } from "../../services/core-settings.ts";
import { regenerateTitle } from "../../services/title-gen.ts";
import { resolveEndpoint } from "../../services/endpoint-resolver.ts";
import { llmScheduler } from "../../services/llm-scheduler.ts";
import { type LlmRequest } from "../../services/llm-provider.ts";
import { getLogger, scrubSecrets } from "../../platform/log.ts";

const log = getLogger("http.sessions");

// Sessions with an in-flight SSE chat stream. The SSE /:id/chat path persists
// the user message and runs a completion independently, so without this guard
// two concurrent POSTs to one session would append two user messages and
// interleave two completions, corrupting the persisted history order.
const sseActiveSessions = new Set<string>();

export function sessionsRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  r.get("/", async (c) => {
    const me = requireClient(c);
    return c.json(await sessionsRepo().list(me.id));
  });

  r.post("/", async (c) => {
    const me = requireClient(c);
    const body = (await readJson(c)) as { title?: string; temporary?: boolean };
    const session = await sessionsRepo().create({
      ownerClientId: me.id,
      title: body.title,
      temporary: body.temporary === true,
    });
    // Creating any session means the client moved on from a prior temporary
    // one; reclaim stragglers it didn't (or couldn't) delete itself.
    await sessionsRepo().sweepClientTemporary(me.id, session.id);
    return c.json(session);
  });

  r.get("/:id", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    // Navigating to a session orphans any other temporary one this client
    // owns (they are never listed, so there is no way back). Sweep them.
    await sessionsRepo().sweepClientTemporary(me.id, session.id);
    const messages = await sessionsRepo().listMessages(session.id);
    return c.json({ ...session, messages });
  });

  r.patch("/:id", async (c) => {
    const me = requireClient(c);
    const body = (await readJson(c)) as { title?: string };
    if (typeof body.title !== "string") {
      throw new AppError("validation_error", "title is required");
    }
    await sessionsRepo().patchTitle(me.id, c.req.param("id"), body.title);
    return c.json({ id: c.req.param("id"), title: body.title });
  });

  // Regenerate the session title on demand. Generation streams in the
  // background; the client learns the start, the new title, and the end via
  // `session.updated` frames, so this just kicks it off and returns.
  r.post("/:id/regenerate-title", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    void regenerateTitle(session.id, me.id);
    return c.body(null, 204);
  });

  r.delete("/:id", async (c) => {
    const me = requireClient(c);
    const { attachmentPaths } = await sessionsRepo().delete(me.id, c.req.param("id"));
    for (const p of attachmentPaths) {
      // host.fs.remove is idempotent (a missing file is a no-op); a real error
      // (permissions, a directory in the way) is worth a warning but never
      // fails the delete.
      try {
        await host().fs.remove(p);
      } catch (err) {
        log.warn(`session delete: failed to remove attachment ${p}: ${errMessage(err)}`);
      }
    }
    return c.body(null, 204);
  });

  r.post("/:id/messages", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    const body = parseBody(messageInputSchema, await readJson(c));
    return c.json(await sessionsRepo().appendMessage(session.id, body as Message));
  });

  r.patch("/:id/messages/:msgId", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    // Look up the row first so we can validate the patch against the
    // schema that matches its role; this is what blocks a UserMessage from
    // being patched with assistant-only fields like `streaming` / `toolCalls`.
    const existing = await sessionsRepo().getMessage(session.id, c.req.param("msgId"));
    const schema = messagePatchSchemaByRole[existing.role as MessageRoleForPatch];
    if (!schema) {
      throw new AppError(
        "validation_error",
        `cannot patch message with unknown role "${existing.role}"`,
      );
    }
    // `schema` is selected by role, so it is a union of per-role ZodTypes that
    // `parseBody`'s single-type generic can't unify; validate it inline.
    const parsed = schema.safeParse(await readJson(c));
    if (!parsed.success) {
      throw new AppError("validation_error", parsed.error.message);
    }
    return c.json(
      await sessionsRepo().patchMessage(
        session.id,
        c.req.param("msgId"),
        parsed.data as Partial<Message>,
      ),
    );
  });

  r.delete("/:id/messages/:msgId", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    await sessionsRepo().deleteMessage(session.id, c.req.param("msgId"));
    return c.body(null, 204);
  });

  r.post("/:id/attachments", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    const form = await c.req.formData();
    const file = form.get("file");
    const messageId = form.get("messageId");
    if (!(file instanceof File) || typeof messageId !== "string") {
      throw new AppError("validation_error", "file + messageId required");
    }
    const id = newAttachmentId();
    const dir = sessionAttachmentsDir(session.id);
    await host().fs.mkdir(dir, { recursive: true });
    const filename = sanitizeFilename(file.name);
    const absPath = join(dir, `${id}_${filename}`);
    const bytes = new Uint8Array(await file.arrayBuffer());
    await host().fs.writeFileAtomic(absPath, bytes);
    const rec = await sessionsRepo().recordAttachment(
      session.id,
      messageId,
      filename,
      file.type || undefined,
      bytes.byteLength,
      absPath,
    );
    return c.json({ id: rec.id, absPath: rec.absPath, filename: rec.filename });
  });

  r.get("/:id/attachments/:attId", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    const rec = await sessionsRepo().getAttachment(session.id, c.req.param("attId"));
    // Read the whole file through the host (no host streaming primitive; the
    // upload path already buffers, so this is symmetric). Attachments are
    // bounded by the request body limit.
    const bytes = await host().fs.readFile(rec.absPath);
    // Copy into an ArrayBuffer-backed view so the Response body type is concrete
    // (host.fs returns Uint8Array<ArrayBufferLike>, which Response rejects).
    return new Response(new Uint8Array(bytes), {
      status: 200,
      headers: { "Content-Type": rec.mime ?? "application/octet-stream" },
    });
  });

  // SSE chat fallback for non-WS clients (plan §4). One-shot completion.
  // No tool support, no reasoning trace, no multi-stream multiplexing. The
  // WS path (chat.start) covers all of those.
  //
  // Body: { content: string }
  // Response: text/event-stream with data lines:
  //   data: {"contentDelta":"hello"}
  //   data: {"contentDelta":" world"}
  //   data: {"done":true,"usage":{...}}
  //   data: {"error":"..."}                                  (terminal)
  r.post("/:id/chat", async (c) => {
    const me = requireClient(c);
    const session = await sessionsRepo().getOrThrow(me.id, c.req.param("id"));
    const body = (await readJson(c)) as { content?: unknown };
    if (typeof body.content !== "string" || body.content.length === 0) {
      throw new AppError("validation_error", "body must be { content: string }");
    }

    // Refuse a second concurrent stream on this session BEFORE persisting the
    // user message, so two overlapping requests can't interleave their turns.
    if (sseActiveSessions.has(session.id)) {
      throw new AppError("conflict", "this session already has an in-flight chat stream");
    }
    sseActiveSessions.add(session.id);

    // Persist the user message so the SSE response can be replayed from
    // session history later if the client refreshes.
    const userMsg: Message = {
      id: newMessageId(),
      role: "user",
      content: body.content,
      createdAtMs: Date.now(),
    } as Message;
    await sessionsRepo().appendMessage(session.id, userMsg);

    const history = await sessionsRepo().listMessages(session.id);
    const settings = await loadCoreSettings();
    const endpoint = await resolveEndpoint(settings, "default");
    const isLocal = strSetting(settings, "llm.provider", "local") === "local";
    const messages = historyToOpenAI(history, strSetting(settings, "llm.systemPrompt", ""));

    const req: LlmRequest = { endpoint, messages };

    const encoder = new TextEncoder();
    const stream = new ReadableStream({
      async start(controller) {
        let assistantText = "";
        let usage: { prompt: number; completion: number; total: number } | undefined;
        try {
          for await (const delta of llmScheduler().schedule(req, {
            clientId: me.id,
            isLocal,
            parallelSlots: numSetting(settings, "llm.local.parallelSlots", 1),
          })) {
            if (delta.contentDelta) {
              assistantText += delta.contentDelta;
              controller.enqueue(
                encoder.encode(`data: ${JSON.stringify({ contentDelta: delta.contentDelta })}\n\n`),
              );
            }
            if (delta.usage) usage = delta.usage;
          }
          // Persist the assistant turn.
          if (assistantText) {
            await sessionsRepo().appendMessage(session.id, {
              id: newMessageId(),
              role: "assistant",
              content: assistantText,
              createdAtMs: Date.now(),
            } as Message);
          }
          controller.enqueue(encoder.encode(`data: ${JSON.stringify({ done: true, usage })}\n\n`));
        } catch (err) {
          controller.enqueue(
            encoder.encode(
              `data: ${JSON.stringify({
                error: scrubSecrets(errMessage(err)),
              })}\n\n`,
            ),
          );
        } finally {
          sseActiveSessions.delete(session.id);
          controller.close();
        }
      },
      cancel() {
        // Client disconnected mid-stream: release the session guard.
        sseActiveSessions.delete(session.id);
      },
    });

    return new Response(stream, {
      status: 200,
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      },
    });
  });

  return r;
}

// --- helpers for the SSE fallback ----------------------------------------

function historyToOpenAI(
  history: Message[],
  systemPrompt: string,
): OpenAI.Chat.Completions.ChatCompletionMessageParam[] {
  const out: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [];
  if (systemPrompt) out.push({ role: "system", content: systemPrompt });
  for (const m of history) {
    // Only carry user/assistant/system through the fallback. Tool messages
    // and reasoning are WS-path concerns. SSE doesn't transport images, so
    // multipart content collapses to its text parts (attachments inlined as
    // text are still useful; image bytes are dropped without warning).
    if (m.role === "user" || m.role === "assistant" || m.role === "system") {
      const text = contentToText(m.content);
      if (text.length > 0) out.push({ role: m.role, content: text });
    }
  }
  return out;
}

// Filenames are attacker-controlled (uploaded via /sessions/:id/attachments)
// and end up on disk as `${id}_${filename}`. The id prefix already prevents
// directory traversal (`/`, `\` are still stripped below), but Windows has
// extra rules (reserved basenames, trailing dots/spaces, length limits)
// that aren't covered by a simple replace, and control chars / pipe-shell
// glyphs are best stripped on every host. Returns a non-empty string that
// is safe to join into a single path segment.
const WIN_RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i;
const MAX_LEN = 200;

// Exported for unit tests; not used outside this module.
export function sanitizeFilename(name: string): string {
  // 1. Strip control chars + replace path / shell / Windows-forbidden glyphs.
  // oxlint-disable-next-line no-control-regex
  let out = name.replace(/[\x00-\x1f/\\:*?"<>|]/g, "_");
  // 2. Strip leading dots (hidden files) and trailing dots/spaces (Windows
  //    silently drops them, which then collides with neighboring names).
  out = out.replace(/^\.+/, "").replace(/[. ]+$/, "");
  // 3. Cap length, preserving the extension if any.
  if (out.length > MAX_LEN) {
    const dot = out.lastIndexOf(".");
    if (dot > 0 && out.length - dot <= 16) {
      const ext = out.slice(dot);
      out = out.slice(0, MAX_LEN - ext.length) + ext;
    } else {
      out = out.slice(0, MAX_LEN);
    }
  }
  // 4. Refuse Windows reserved basenames even with an extension; prefix
  //    with `_` so the file is still recognizable.
  if (WIN_RESERVED.test(out)) out = `_${out}`;
  // 5. Fall back to a fixed placeholder if everything was stripped. The
  //    caller still prepends an id, so this is just informational.
  if (out.length === 0) out = "file";
  return out;
}

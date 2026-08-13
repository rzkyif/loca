// Builds the provider request's message array from the session history,
// resolving attachments (images inlined as data URIs, documents inlined as
// text) and expanding `@token` memory references at request-build time.

import type OpenAI from "openai";
import type { AssistantMessage, Message, MessageContent } from "@loca/shared";
import { contentToText, errMessage } from "@loca/shared";
import { encodeBase64 } from "@std/encoding/base64";
import { sessionsRepo } from "./sessions-store.ts";
import { memoryTokenBlocks, newMemoryTokenBudget } from "./memory-injection.ts";
import { getLogger } from "../platform/log.ts";
import { host } from "../platform/runtime.ts";

const log = getLogger("chat.attachments");

export async function toOpenAiMessages(
  history: Message[],
  systemPrompt: string,
  sessionId: string,
  attachmentCache?: Map<string, string | null>,
  mcpClaimedTokens?: Set<string>,
): Promise<OpenAI.Chat.Completions.ChatCompletionMessageParam[]> {
  const out: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [];
  if (systemPrompt) out.push({ role: "system", content: systemPrompt });
  const memoryTokenBudget = newMemoryTokenBudget();
  // Stems already resolved as MCP resources/prompts are pre-marked expanded so
  // the memory expander skips them (avoids a double injection on a slug that
  // names both).
  for (const t of mcpClaimedTokens ?? []) memoryTokenBudget.claimed.add(t);
  for (const m of history) {
    if (m.role === "user") {
      let content = await userContentToOpenAi(m.content, sessionId, attachmentCache);
      // `@token` memory references: the stored message keeps only the
      // token; the CURRENT content is appended at request-build time, so a
      // later edit of the memory flows into the next turn automatically.
      // The budget dedupes repeat mentions across the whole request.
      const memoryBlocks = await memoryTokenBlocks(contentToText(m.content), memoryTokenBudget);
      if (memoryBlocks) {
        if (typeof content === "string") {
          content = `${content}\n\n${memoryBlocks}`;
        } else content.push({ type: "text", text: memoryBlocks });
      }
      out.push({ role: "user", content });
    } else if (m.role === "assistant") {
      // Assistant content is always a plain string in this codebase (no
      // multipart support needed at the model boundary). Tool calls the
      // model emitted MUST be replayed on the message: the `role: "tool"`
      // results that follow reference them by id, and an undeclared
      // tool_call_id renders as an orphaned tool response in the chat
      // template, garbling every hop after a call.
      const text = typeof m.content === "string" ? m.content : contentToText(m.content);
      const calls = (m as AssistantMessage).toolCalls;
      const param: OpenAI.Chat.Completions.ChatCompletionAssistantMessageParam = {
        role: "assistant",
        content: text,
      };
      if (calls && calls.length > 0) {
        param.tool_calls = calls.map((tc) => ({
          id: tc.callId,
          type: "function" as const,
          function: { name: tc.toolName, arguments: tc.arguments },
        }));
      }
      out.push(param);
    } else if (m.role === "system") {
      const text = typeof m.content === "string" ? m.content : contentToText(m.content);
      out.push({ role: "system", content: text });
    } else if (m.role === "tool") {
      out.push({
        role: "tool",
        tool_call_id: m.callId,
        content:
          m.status === "completed" ? JSON.stringify(m.result) : JSON.stringify({ error: m.error }),
      });
    } else if (m.role === "reasoning") {
      // Reasoning trace is not part of the OpenAI message protocol; omit.
    }
    // tool_filter / memory_filter / display / error messages are not sent to the LLM.
  }
  return out;
}

// Multipart user content → OpenAI ChatCompletionContentPart[]. `image_file`
// parts load the bytes from disk and inline them as a data URI so the model
// receives the actual image; `document_file` reads the file (the client
// always converts non-image attachments to markdown before upload) and
// inlines the text with an "[Attached document: filename]" header so the
// model knows the boundary. Falls back to a string when the content is just
// text, which keeps the wire payload small for the common no-attachment case.
async function userContentToOpenAi(
  content: MessageContent,
  sessionId: string,
  attachmentCache?: Map<string, string | null>,
): Promise<string | OpenAI.Chat.Completions.ChatCompletionContentPart[]> {
  if (typeof content === "string") return content;
  const parts: OpenAI.Chat.Completions.ChatCompletionContentPart[] = [];
  for (const p of content) {
    if (p.type === "text") {
      parts.push({ type: "text", text: p.text });
    } else if (p.type === "image_url") {
      parts.push({ type: "image_url", image_url: { url: p.image_url.url } });
    } else if (p.type === "image_file") {
      const dataUrl = await readAttachmentAsDataUrl(
        sessionId,
        p.path,
        p.mime || "image/png",
        attachmentCache,
      );
      if (dataUrl) {
        parts.push({ type: "image_url", image_url: { url: dataUrl } });
      }
    } else if (p.type === "document") {
      parts.push({
        type: "text",
        text: `[Attached document: ${p.filename}]\n\n${p.markdown}`,
      });
    } else if (p.type === "document_file") {
      const text = await readAttachmentAsText(sessionId, p.path, attachmentCache);
      if (text !== null) {
        parts.push({
          type: "text",
          text: `[Attached document: ${p.filename}]\n\n${text}`,
        });
      }
    }
  }
  // Collapse to a plain string when every part is text, because some providers
  // reject a `content: [{type: "text", ...}]` shape that has no image_url siblings.
  if (parts.every((p) => p.type === "text")) {
    return parts
      .filter((p): p is OpenAI.Chat.Completions.ChatCompletionContentPartText => p.type === "text")
      .map((p) => p.text)
      .join("\n");
  }
  return parts;
}

// Pull the trailing attachment id out of the URL the client stored on the
// MessagePart (the path field is `<baseUrl>/api/v1/sessions/<sid>/attachments/<aid>`).
// Returns null if the URL doesn't match. We just skip the attachment in that
// case rather than failing the whole turn.
function attachmentIdFromPath(path: string): string | null {
  const m = path.match(/\/attachments\/([^/?#]+)$/);
  return m ? m[1] : null;
}

async function readAttachmentAsDataUrl(
  sessionId: string,
  path: string,
  mime: string,
  cache?: Map<string, string | null>,
): Promise<string | null> {
  const id = attachmentIdFromPath(path);
  if (!id) return null;
  const cacheKey = `img:${id}`;
  if (cache?.has(cacheKey)) return cache.get(cacheKey) ?? null;
  let result: string | null = null;
  try {
    const rec = await sessionsRepo().getAttachment(sessionId, id);
    const bytes = await host().fs.readFile(rec.absPath);
    // encodeBase64 over the Uint8Array directly, instead of an O(n) per-byte
    // String.fromCharCode loop that builds a giant intermediate binary string
    // on the event loop for every multi-MB image.
    result = `data:${rec.mime ?? mime};base64,${encodeBase64(bytes)}`;
  } catch (err) {
    log.warn(`image attachment load failed (${path}): ${errMessage(err)}`);
    result = null;
  }
  cache?.set(cacheKey, result);
  return result;
}

async function readAttachmentAsText(
  sessionId: string,
  path: string,
  cache?: Map<string, string | null>,
): Promise<string | null> {
  const id = attachmentIdFromPath(path);
  if (!id) return null;
  const cacheKey = `doc:${id}`;
  if (cache?.has(cacheKey)) return cache.get(cacheKey) ?? null;
  let result: string | null = null;
  try {
    const rec = await sessionsRepo().getAttachment(sessionId, id);
    result = await host().fs.readTextFile(rec.absPath);
  } catch (err) {
    log.warn(`document attachment load failed (${path}): ${errMessage(err)}`);
    result = null;
  }
  cache?.set(cacheKey, result);
  return result;
}

/**
 * Helpers for working with message attachments: base64 encode/decode,
 * upload/download attachment files through the core REST API, and figure
 * out which files belong to a given message.
 *
 * After the rework, attachments are owned by the currently-selected core
 * (not the local filesystem). The persisted MessagePart still uses `path`
 * as its key. That key is now a `/api/v1/sessions/:id/attachments/:attId`
 * URL on the core, not a local file path.
 */

import { cores } from "$lib/core";
import { platform } from "$lib/platform";
import { blobToBase64 } from "$lib/util/audio";
import type { Attachment, MessageContent, MessagePart } from "$lib/util/types";

export type WrittenAttachment = { path: string; filename: string };

/** Base64-encode a UTF-8 string (used for document markdown payloads). */
export function utf8ToBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

/** Decode base64 → UTF-8 string. */
export function base64ToUtf8(b64: string): string {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

/** Upload an attachment to the paired core for the given message. The
 *  returned `path` is the authed URL the AttachmentList renders from. */
export async function writeSessionAttachment(
  sessionId: string,
  messageId: string,
  filename: string,
  dataBase64: string,
  mime?: string,
): Promise<WrittenAttachment> {
  const blob = base64ToBlob(dataBase64, mime ?? "application/octet-stream");
  const api = cores().api().sessions;
  const res = await api.uploadAttachment(sessionId, messageId, blob, filename);
  return {
    path: api.attachmentUrl(sessionId, res.id),
    filename: res.filename,
  };
}

/** Fetch an attachment by its core URL (the `path` stored on MessagePart)
 *  and return its base64 contents. Bearer auth is applied by CoreClient. */
export async function readSessionAttachment(path: string): Promise<string> {
  const client = cores().currentClient();
  if (!client) throw new Error("no paired core selected");
  // The stored `path` is `<baseUrl>/api/v1/sessions/.../attachments/...`, so we
  // fetch it through the pinned net layer with the same bearer header CoreClient
  // would. `endpoint` is a public readonly field on CoreClient, so no cast is needed.
  const res = await platform().net.fetch({
    url: path,
    headers: { Authorization: `Bearer ${client.endpoint.token}` },
    ...client.netTrust(),
  });
  if (res.status < 200 || res.status >= 300) {
    throw new Error(`fetch ${path}: ${res.status}`);
  }
  const bytes = res.body;
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

/** Best-effort delete of attachment files. New core garbage-collects them
 *  on session-delete, so per-attachment delete from the client is a no-op
 *  for now (the old Tauri command is gone). Kept as a stub so callers
 *  don't have to be conditional. */
export function deleteSessionAttachments(_paths: string[]): Promise<void> {
  return Promise.resolve();
}

/** Collect the file paths referenced by any `_file` parts inside a message. */
export function collectAttachmentPaths(content: MessageContent): Set<string> {
  const out = new Set<string>();
  if (typeof content === "string") return out;
  for (const part of content) {
    if (part.type === "image_file" || part.type === "document_file") {
      out.add(part.path);
    }
  }
  return out;
}

/** Paths present in `prev` but not in `next`. */
export function diffRemovedAttachmentPaths(prev: MessageContent, next: MessageContent): string[] {
  const prevPaths = collectAttachmentPaths(prev);
  const nextPaths = collectAttachmentPaths(next);
  const removed: string[] = [];
  for (const p of prevPaths) {
    if (!nextPaths.has(p)) removed.push(p);
  }
  return removed;
}

const IMAGE_EXT_BY_MIME: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/jpg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
  "image/svg+xml": "svg",
};

export function imageExtFromMime(mime: string): string {
  return IMAGE_EXT_BY_MIME[mime.toLowerCase()] || "png";
}

/** Ensure a document filename has a `.md` extension so on-disk content matches the payload. */
export function ensureMarkdownExtension(filename: string): string {
  const lower = filename.toLowerCase();
  if (lower.endsWith(".md")) return filename;
  return `${filename}.md`;
}

/** Preview display list: maps persisted `_file` parts to the legacy
 *  display shape the AttachmentList expects. */
export function toPreviewParts(content: MessageContent): MessagePart[] {
  if (typeof content === "string") return [];
  return content.filter(
    (p): p is MessagePart =>
      p.type === "document" ||
      p.type === "image_url" ||
      p.type === "document_file" ||
      p.type === "image_file",
  );
}

// Ingest: classifying picked/pasted files and turning their bytes into the
// in-memory Attachment shape the input area appends. The file-type tables and
// `classifyAttachment` are pure (unit-tested); the `ingest*` helpers touch the
// platform() seam (FileConvert / base64) and are exercised by the running app.

/** Document file types we accept for markdown conversion. */
export const DOC_EXTENSIONS = [
  "pdf",
  "docx",
  "pptx",
  "xlsx",
  "xls",
  "csv",
  "html",
  "htm",
  "txt",
  "md",
  "json",
  "xml",
  "rst",
  "log",
  "toml",
  "yaml",
  "ini",
  "py",
  "rs",
  "js",
  "ts",
  "c",
  "cpp",
  "go",
  "java",
];

/** The subset of DOC_EXTENSIONS that is plain UTF-8 text, so it needs no binary
 *  document converter (anytomd/pdf-extract) and can be ingested by reading the
 *  bytes directly. Used on mobile, where the converter is unavailable. The
 *  remainder (pdf, docx, pptx, xlsx, xls) needs the converter and is desktop-only. */
export const TEXT_DOC_EXTENSIONS = [
  "csv",
  "html",
  "htm",
  "txt",
  "md",
  "json",
  "xml",
  "rst",
  "log",
  "toml",
  "yaml",
  "ini",
  "py",
  "rs",
  "js",
  "ts",
  "c",
  "cpp",
  "go",
  "java",
];

/** Image file types we accept (only when the model supports images). */
export const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];

/** Extension -> MIME used when a picked/pasted image carries no own type. */
export const MIME_BY_EXT: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  svg: "image/svg+xml",
};

/**
 * Decide how a file should be ingested from its name + reported type. Images
 * are only eligible when the model supports them; otherwise a known document
 * extension wins, and anything else is rejected (`null`). The returned `ext`
 * for images is normalized to a known image extension (falling back to `png`)
 * so a synthesized filename gets a sensible suffix.
 */
export function classifyAttachment(
  filename: string,
  fileType: string,
  supportImages: boolean,
): { kind: "image"; ext: string } | { kind: "document" } | null {
  const ext = filename.split(".").pop()?.toLowerCase() || "";
  const isImage =
    supportImages && (IMAGE_EXTENSIONS.includes(ext) || fileType.startsWith("image/"));
  if (isImage) {
    return { kind: "image", ext: IMAGE_EXTENSIONS.includes(ext) ? ext : "png" };
  }
  if (DOC_EXTENSIONS.includes(ext)) return { kind: "document" };
  return null;
}

/** Encode an image blob into the in-memory image Attachment. */
export async function ingestImageBlob(
  blob: Blob,
  filename: string,
  ext: string,
): Promise<Attachment> {
  const mime = blob.type || MIME_BY_EXT[ext] || "image/png";
  const pendingData = await blobToBase64(new Blob([blob], { type: mime }));
  return { type: "image", filename, pendingData, mime };
}

/** Convert a document blob to markdown and wrap it as a document Attachment. */
export async function ingestDocumentBlob(blob: Blob, filename: string): Promise<Attachment> {
  // platform().fileConvert.toMarkdown handles the temp-file dance (write,
  // convert, cleanup) internally for any File. Construct a File so the
  // filename survives the round-trip.
  const file = new File([blob], filename, { type: blob.type });
  const pendingData = await platform().fileConvert.toMarkdown(file);
  return { type: "document", filename, pendingData };
}

/** Wrap already-decoded UTF-8 document text as a document Attachment, skipping
 *  the binary converter. The text IS the content (a text/markdown/code/csv file
 *  needs no conversion), so this is the mobile path where the converter is
 *  unavailable. */
export function ingestTextDocument(text: string, filename: string): Attachment {
  return { type: "document", filename, pendingData: text };
}

/** Convert an on-disk document straight to markdown (file-picker path). */
export async function ingestDocumentFromPath(
  filePath: string,
  filename: string,
): Promise<Attachment> {
  const pendingData = await platform().fileConvert.toMarkdownFromPath(filePath);
  return { type: "document", filename, pendingData };
}

function base64ToBlob(b64: string, mime: string): Blob {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}

// Download a file from an http(s) URL into the user's Downloads folder,
// reporting progress via ctx.setProgress as bytes land.

import * as path from "node:path";
import * as fs from "node:fs";
import mime from "mime-types";
import { assertSafePublicUrl, safeFetch } from "./net.ts";
import type { ToolContext } from "./types.ts";

export async function download(
  args: { url?: string; filename?: string },
  ctx: ToolContext,
): Promise<{ path: string; bytes: number }> {
  const url = typeof args?.url === "string" ? args.url.trim() : "";
  // Validates scheme and blocks loopback/private targets (safeFetch re-checks
  // each redirect hop below).
  assertSafePublicUrl(url);

  ctx.setProgress(0, "Starting download", url);
  const res = await safeFetch(url, { signal: ctx.signal });
  if (!res.ok) throw new Error(`server returned ${res.status}`);

  const total = Number(res.headers.get("content-length")) || 0;
  const downloadsDir = pickDownloadsDir();
  fs.mkdirSync(downloadsDir, { recursive: true });

  const baseName = deriveFilename(url, args?.filename);
  const contentType = res.headers.get("content-type")?.split(";")[0]?.trim() ?? "";
  const withExt = ensureExtension(baseName, contentType);
  const destPath = uniquePath(downloadsDir, withExt);

  ctx.log("info", `writing ${destPath}`);

  if (!res.body) throw new Error("response has no body");
  const reader = res.body.getReader();
  const tmp = destPath + ".part";
  // Open the temp file once, write each chunk as it lands. On error or
  // abort we rm the .part so we don't leave half-files in Downloads.
  const file = await Deno.open(tmp, {
    write: true,
    create: true,
    truncate: true,
  });
  let written = 0;
  try {
    while (true) {
      if (ctx.signal.aborted) throw new Error("cancelled by user");
      const { done, value } = await reader.read();
      if (done) break;
      await file.write(value);
      written += value.byteLength;
      if (total > 0) {
        ctx.setProgress(
          written / total,
          "Downloading",
          `${(written / 1024 / 1024).toFixed(1)} / ${(total / 1024 / 1024).toFixed(1)} MB`,
        );
      } else {
        ctx.setProgress(0, "Downloading", `${(written / 1024 / 1024).toFixed(1)} MB`);
      }
    }
    file.close();
    fs.renameSync(tmp, destPath);
  } catch (err) {
    try {
      file.close();
    } catch {
      /* already closed */
    }
    try {
      fs.rmSync(tmp, { force: true });
    } catch {
      /* ignore */
    }
    throw err;
  }

  ctx.setProgress(1, "Downloaded", path.basename(destPath));
  return { path: destPath, bytes: written };
}

export function pickDownloadsDir(): string {
  // Cross-platform heuristic: honor XDG_DOWNLOAD_DIR when set, otherwise
  // fall back to ~/Downloads. Home comes from the declared HOME / USERPROFILE
  // env grants, not os.homedir(), which would need an undeclared
  // --allow-sys=homedir and throw on the default path. Each env read is
  // guarded: Deno.env.get throws (not returns undefined) when a key isn't
  // granted, so an ungranted XDG var must not abort the whole tool.
  const xdg = tryEnv("XDG_DOWNLOAD_DIR");
  if (xdg && fs.existsSync(xdg)) return xdg;
  const home = tryEnv("HOME") ?? tryEnv("USERPROFILE");
  if (!home) {
    throw new Error("cannot locate the home directory (HOME / USERPROFILE not set or granted)");
  }
  return path.join(home, "Downloads");
}

function tryEnv(key: string): string | undefined {
  try {
    const v = Deno.env.get(key);
    return v && v.length > 0 ? v : undefined;
  } catch {
    return undefined; // permission not granted for this key
  }
}

function deriveFilename(url: string, override: string | undefined): string {
  if (override) return sanitizeFilename(override);
  try {
    const u = new URL(url);
    const last = u.pathname.split("/").filter(Boolean).pop();
    if (last) return sanitizeFilename(decodeURIComponent(last));
  } catch {
    /* fall through */
  }
  return "download";
}

// Reduce an arbitrary string (URL path segment or model-supplied override) to
// a safe basename: no directory separators, no traversal, no control chars,
// bounded length. Without this a name like "../../etc/passwd" or "a/b" would
// escape the Downloads folder. Filtered by codepoint rather than a regex so
// no control characters appear in source (the linter rejects those).
function sanitizeFilename(name: string): string {
  let cleaned = "";
  for (const ch of name) {
    const code = ch.codePointAt(0) ?? 0;
    if (code < 0x20 || code === 0x7f) continue; // C0 controls + DEL
    if (ch === "/" || ch === "\\") {
      cleaned += "_";
      continue;
    }
    cleaned += ch;
  }
  const base = cleaned.replace(/^\.+/, "").trim().slice(0, 200);
  return base.length > 0 ? base : "download";
}

function ensureExtension(name: string, contentType: string): string {
  if (path.extname(name)) return name;
  if (!contentType) return name;
  const ext = mime.extension(contentType);
  return ext ? `${name}.${ext}` : name;
}

export function uniquePath(dir: string, filename: string): string {
  let candidate = path.join(dir, filename);
  if (!fs.existsSync(candidate)) return candidate;
  const ext = path.extname(filename);
  const stem = filename.slice(0, filename.length - ext.length);
  for (let i = 1; i < 1000; i++) {
    candidate = path.join(dir, `${stem}-${i}${ext}`);
    if (!fs.existsSync(candidate)) return candidate;
  }
  return candidate;
}

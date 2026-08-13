// Resolves an extension's entry point (the file the worker imports) and the
// default downloads directory used when building a worker's path templates.

import { isWithin } from "@loca/core-engine";
import { AppError } from "@loca/core-engine";

export function resolveEntryPath(extensionFolder: string): string {
  const entry = resolveEntryCandidate(extensionFolder);
  // The entry is imported and executed by the worker, so it MUST stay inside
  // the extension folder. A malicious deno.json "exports" / package.json "main"
  // like "../../evil.ts" would otherwise run code outside the install dir that
  // the content hash verifies. (The same isWithin guard protects tarball
  // extraction and the install path.)
  if (!isWithin(extensionFolder, entry)) {
    throw new AppError(
      "invalid_loca_json",
      `extension entry point ${entry} escapes its folder ${extensionFolder}`,
    );
  }
  return entry;
}

function resolveEntryCandidate(extensionFolder: string): string {
  // Resolution order: deno.json "exports" (string form) for deno-native
  // extensions (e.g. the built-in, which ships no package.json), then
  // package.json "main" for npm-extracted extensions, then the index.ts
  // convention. The worker imports this via file://, so we just need the right
  // entry file.
  try {
    const cfg = JSON.parse(Deno.readTextFileSync(`${extensionFolder}/deno.json`)) as {
      exports?: unknown;
    };
    if (typeof cfg.exports === "string" && cfg.exports.length > 0) {
      return `${extensionFolder}/${cfg.exports.replace(/^\.\//, "")}`;
    }
  } catch {
    /* no deno.json (or no string exports) */
  }
  try {
    const pkg = JSON.parse(Deno.readTextFileSync(`${extensionFolder}/package.json`)) as {
      main?: string;
    };
    if (typeof pkg.main === "string" && pkg.main.length > 0) {
      return `${extensionFolder}/${pkg.main.replace(/^\.\//, "")}`;
    }
  } catch {
    /* no package.json */
  }
  return `${extensionFolder}/index.ts`;
}

export function defaultDownloadsDir(): string {
  if (Deno.build.os === "windows") {
    const profile = Deno.env.get("USERPROFILE");
    if (profile) return `${profile}\\Downloads`;
    return "";
  }
  const home = Deno.env.get("HOME");
  return home ? `${home}/Downloads` : "";
}

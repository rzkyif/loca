// Shared types and tiny helpers for the extension installer's phases
// (installer-download.ts / installer-register.ts / installer-deps.ts). This is
// the dependency leaf: every phase module imports from here, so nothing here may
// import them back.

import type { BuiltinExtensionManifest } from "@tomat/shared";

export type InstallSource =
  | { source: "npm"; name: string; version?: string }
  | { source: "local"; path: string; slug: string }
  // A seeded extension Tomat ships (the built-in, or the dev-only samples), keyed
  // by its id. Bytes are resolved at install time:
  // - `planted` set (first-boot seeding ONLY): install offline from the
  //   install-script-planted tarball + already-verified signed manifest, with NO
  //   network request (a running core never fetches without a user action).
  // - `planted` unset: resolve from the codebase (dev) or the signed CDN tarball.
  //   That branch fetches, so it runs only from a user-triggered download/update.
  | {
      source: "seeded";
      id: string;
      planted?: { tarballPath: string; manifest: BuiltinExtensionManifest };
    };

export interface InstallEventSink {
  log(jobId: string, id: string, stream: "stdout" | "stderr", line: string): void;
  done(jobId: string, id: string, ok: boolean, code: number): void;
}

export function flattenNpmName(name: string): string {
  // @scope/name -> @scope__name to land cleanly under extensions/<id>/.
  return name.replace("/", "__");
}

export async function readOptional(path: string): Promise<string | null> {
  try {
    return await Deno.readTextFile(path);
  } catch {
    return null;
  }
}

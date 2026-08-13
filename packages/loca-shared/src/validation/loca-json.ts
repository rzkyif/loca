// Canonical loca.json schema: the manifest every Loca extension ships.
// Parsed by core's installer to validate an extension at install time and
// re-validated on hash drift. An npm package is discovered as an extension by
// the `loca-extension` keyword and described by this file: a human-readable
// `displayName`, the tools it provides (optional - an extension may ship only
// memories), and the knowledge/skills it bundles.

import { z } from "zod";

const reasonField = z.string().min(1).max(500);

const portSchema = z.union([z.number().int().min(1).max(65535), z.literal("*")]);

const netPermission = z.object({
  host: z.string().min(1),
  ports: z.array(portSchema).min(1),
  reason: reasonField,
  optional: z.boolean().optional(),
});

const pathPermission = z.object({
  path: z.string().min(1),
  reason: reasonField,
  optional: z.boolean().optional(),
});

const runPermission = z.object({
  binary: z.string().min(1),
  reason: reasonField,
  optional: z.boolean().optional(),
});

const envPermission = z.object({
  key: z
    .string()
    .min(1)
    .regex(/^[A-Z_][A-Z0-9_]*$/, {
      message: "env key must be SCREAMING_SNAKE_CASE",
    }),
  reason: reasonField,
  optional: z.boolean().optional(),
});

const ffiPermission = z.object({
  reason: reasonField,
  optional: z.boolean().optional(),
});

const sysPermission = z.object({
  flag: z.string().min(1),
  reason: reasonField,
  optional: z.boolean().optional(),
});

// Host-module permissions (memory store, LLM, TTS, STT) enforced by the
// host's module broker rather than the sandbox runtime. All-or-nothing keys
// except memories, which splits into read/write access.
const memoriesPermission = z.object({
  access: z.enum(["read", "write"]),
  reason: reasonField,
  optional: z.boolean().optional(),
});

const modulePermission = z.object({
  reason: reasonField,
  optional: z.boolean().optional(),
});

export const toolPermissionsSchema = z
  .object({
    net: z.array(netPermission).default([]),
    read: z.array(pathPermission).default([]),
    write: z.array(pathPermission).default([]),
    run: z.array(runPermission).default([]),
    env: z.array(envPermission).default([]),
    ffi: z.array(ffiPermission).default([]),
    sys: z.array(sysPermission).default([]),
    memories: z.array(memoriesPermission).default([]),
    llm: z.array(modulePermission).default([]),
    tts: z.array(modulePermission).default([]),
    stt: z.array(modulePermission).default([]),
  })
  .strict();

export type ToolPermissionsDecl = z.infer<typeof toolPermissionsSchema>;

const toolNamePattern = /^[a-zA-Z0-9_-]{1,64}$/;

export const toolSchema = z
  .object({
    name: z.string().regex(toolNamePattern, {
      message: "tool name must match ^[a-zA-Z0-9_-]{1,64}$",
    }),
    description: z.string().min(1),
    parameters: z.record(z.string(), z.unknown()),
    triggers: z.array(z.string()).default([]),
    function: z.string().min(1),
    alwaysAvailable: z.boolean().default(false),
    // OS gating. Empty (or omitted) = every platform. `linux` matches any Linux
    // session; `linux_x11` / `linux_wayland` narrow to a display server, which
    // Loca detects centrally. A tool is hidden everywhere on a host its list
    // doesn't cover.
    platforms: z
      .array(z.enum(["darwin", "windows", "linux", "linux_x11", "linux_wayland"]))
      .default([]),
    permissions: toolPermissionsSchema.default({
      net: [],
      read: [],
      write: [],
      run: [],
      env: [],
      ffi: [],
      sys: [],
      memories: [],
      llm: [],
      tts: [],
      stt: [],
    }),
  })
  .strict();

export type ToolDecl = z.infer<typeof toolSchema>;

// A memory the extension ships: reference `knowledge` or a procedural `skill`.
// `path` is relative to the extension root - a `.md` file for knowledge, or a
// folder holding SKILL.md (plus optional bundled files) for a skill. The path
// must stay inside the extension: no absolute paths and no `..` segments, so a
// manifest can't make core read files outside the install dir.
const relativeMemoryPath = z
  .string()
  .min(1)
  .refine(
    (p) => !/^([/\\]|[A-Za-z]:)/.test(p) && !p.split(/[/\\]/).includes(".."),
    "path must be relative and may not contain `..` segments",
  );

export const memoryDeclSchema = z
  .object({
    kind: z.enum(["knowledge", "skill"]),
    path: relativeMemoryPath,
  })
  .strict();

export type MemoryDecl = z.infer<typeof memoryDeclSchema>;

export const extensionManifestSchema = z
  .object({
    $schema: z.string().url().optional(),
    name: z.string().min(1),
    // Human-readable name shown in the Extensions UI (the built-in's is
    // "Built-In Functionalities").
    displayName: z.string().min(1),
    description: z.string().min(1),
    license: z.string().optional(),
    homepage: z.string().url().optional(),
    // Whether the host should provision a private database for this extension
    // (exposed to tools as `ctx.db`). Storage is host-defined.
    database: z.boolean().default(false),
    // An extension may provide tools, memories, or both; either list may be
    // empty (but one with neither does nothing useful).
    tools: z.array(toolSchema).default([]),
    memories: z.array(memoryDeclSchema).default([]),
  })
  .strict()
  .superRefine((value, ctx) => {
    const names = new Set<string>();
    for (let i = 0; i < value.tools.length; i++) {
      const name = value.tools[i].name;
      if (names.has(name)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          path: ["tools", i, "name"],
          message: `duplicate tool name "${name}" within extension`,
        });
      }
      names.add(name);
    }
  });

export type ExtensionManifest = z.infer<typeof extensionManifestSchema>;

export interface ManifestParseError {
  ok: false;
  message: string;
  issues: Array<{ path: (string | number)[]; message: string }>;
}

export interface ManifestParseOk {
  ok: true;
  value: ExtensionManifest;
}

export type ManifestParseResult = ManifestParseOk | ManifestParseError;

export function parseExtensionManifest(input: unknown): ManifestParseResult {
  const result = extensionManifestSchema.safeParse(input);
  if (result.success) {
    return { ok: true, value: result.data };
  }
  return {
    ok: false,
    message: result.error.message,
    issues: result.error.issues.map((i) => ({
      path: i.path.filter((p): p is string | number => typeof p !== "symbol"),
      message: i.message,
    })),
  };
}

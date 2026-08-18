// Zod schemas for the model routes' request bodies.
//
// These live here rather than beside the routes that read them so the contract
// bridge can see them: `scripts/contract/emit-zod.ts` walks this directory and
// nothing else, and a schema declared inside a route file is generated into no
// Rust type at all. The core then falls back to the constraint-free shape from
// the TypeScript half, and the port silently accepts what the oracle refuses.
// See D-093.
//
// Strict objects with enums and bounded arrays, so a malformed or oversized
// body is rejected before it reaches the managers.

import { z } from "zod";

/** Not exported, so it is inlined at its one use site rather than generating a
 *  Rust type named after nothing in particular. */
const groupEnum = z.enum(["llm", "stt", "tts", "embed"]);

/** The `POST /api/v1/models/download` body.
 *
 *  `.min(1)` on `source` is the load-bearing part: an empty source names no
 *  file, and the `.max(256)` caps a body that would otherwise enqueue that many
 *  downloads in one call. */
export const modelDownloadBodySchema = z
  .object({
    items: z
      .array(z.object({ source: z.string().min(1), group: groupEnum.optional() }).strict())
      .max(256),
  })
  .strict();

/** The `POST /api/v1/models/probe` body. Same bounds as the download body,
 *  which is the point: probe is the dry run of the same batch. */
export const modelProbeBodySchema = z
  .object({
    sources: z.array(z.string().min(1)).max(256),
  })
  .strict();

/** The `POST /api/v1/models/select` body. Three discriminators, tried in order;
 *  the handler branches on JavaScript truthiness, so `""` falls through to the
 *  next one rather than selecting nothing. */
export const modelSelectBodySchema = z
  .object({
    bucket: z.string().optional(),
    modelId: z.string().optional(),
    modelSpec: z.string().optional(),
  })
  .strict();

/** The `POST /api/v1/models/{stt,tts}/select` body. Both speech routes take the
 *  same three discriminators. */
export const speechSelectBodySchema = z
  .object({
    presetId: z.string().optional(),
    modelId: z.string().optional(),
    modelSpec: z.string().optional(),
  })
  .strict();

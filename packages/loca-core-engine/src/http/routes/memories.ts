// Memory store CRUD + rescan. Backs the Memories settings manager and
// the client's @-autocomplete; tools reach the same store through the
// module broker instead of these routes.

import { Hono, type MiddlewareHandler } from "hono";
import { z } from "zod";
import { memoriesStore } from "../../services/memories-store.ts";
import { host } from "../../platform/runtime.ts";
import { parseBody, readJson } from "../body.ts";

const createBodySchema = z
  .object({
    kind: z.enum(["knowledge", "skill"]).default("knowledge"),
    title: z.string().min(1),
    content: z.string().default(""),
  })
  .strict();

const fileBodySchema = z
  .object({
    content: z.string().default(""),
  })
  .strict();

const patchBodySchema = z
  .object({
    title: z.string().min(1).optional(),
    content: z.string().optional(),
    enabled: z.boolean().optional(),
  })
  .strict();

// Queue background (re)indexing through the host. Absent on a host without a
// background queue; there the summary/embedding refresh is that host's concern.
function scheduleIndexing(memoryId?: string): void {
  host().status?.scheduleMemoryIndexing(memoryId);
}

export function memoriesRoutes(authed: MiddlewareHandler): Hono {
  const r = new Hono();
  r.use("*", authed);

  r.get("/", (c) => c.json({ memories: memoriesStore().list() }));

  r.post("/", async (c) => {
    const body = parseBody(createBodySchema, await readJson(c));
    const doc = await memoriesStore().create(body.kind, body.title, body.content);
    scheduleIndexing(doc.id);
    return c.json(doc, 201);
  });

  r.post("/rescan", async (c) => {
    const result = await memoriesStore().rescan();
    scheduleIndexing();
    return c.json(result);
  });

  // Re-run the background indexer for one memory (regenerate summary + embedding).
  // The store get below confirms the id exists before scheduling.
  r.post("/:id/reindex", async (c) => {
    const id = c.req.param("id");
    await memoriesStore().get(id);
    scheduleIndexing(id);
    return c.json({ ok: true });
  });

  r.get("/:id", async (c) => c.json(await memoriesStore().get(c.req.param("id"))));

  // One bundled reference file from a skill folder.
  r.get("/:id/files/:name", async (c) =>
    c.json({
      content: await memoriesStore().getFile(c.req.param("id"), c.req.param("name")),
    }),
  );

  // Create or replace a bundled file beside a user skill's SKILL.md.
  r.put("/:id/files/:name", async (c) => {
    const body = parseBody(fileBodySchema, await readJson(c));
    await memoriesStore().writeFile(c.req.param("id"), c.req.param("name"), body.content);
    return c.json({ ok: true });
  });

  r.delete("/:id/files/:name", async (c) => {
    await memoriesStore().deleteFile(c.req.param("id"), c.req.param("name"));
    return c.json({ ok: true });
  });

  r.patch("/:id", async (c) => {
    const id = c.req.param("id");
    const body = parseBody(patchBodySchema, await readJson(c));
    if (body.enabled !== undefined) {
      memoriesStore().setEnabled(id, body.enabled);
    }
    if (body.title !== undefined) {
      memoriesStore().rename(id, body.title);
    }
    if (body.content !== undefined) {
      await memoriesStore().replaceContent(id, body.content);
    }
    scheduleIndexing(id);
    return c.json(await memoriesStore().get(id));
  });

  r.delete("/:id", async (c) => {
    await memoriesStore().delete(c.req.param("id"));
    return c.json({ ok: true });
  });

  return r;
}

// Scheduled prompt CRUD + manual run. Backs the Scheduled Prompts settings
// manager; agent-created schedules arrive through the schedule confirm flow
// instead of these routes.

import { Hono } from "hono";
import { z } from "zod";
import { scheduledPromptDraftSchema, scheduleSpecSchema } from "@loca/shared";
import { promptScheduler } from "../../services/prompt-scheduler.ts";
import { parseBody, readJson } from "@loca/core-engine/http/body";
import { bearerMiddleware, requireClient } from "../middleware/auth.ts";

const patchBodySchema = z
  .object({
    title: z.string().min(1).optional(),
    instruction: z.string().min(1).optional(),
    schedule: scheduleSpecSchema.optional(),
    runMissed: z.boolean().optional(),
    enabled: z.boolean().optional(),
  })
  .strict();

export function scheduledPromptsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.get("/", (c) => {
    const me = requireClient(c);
    return c.json({ scheduledPrompts: promptScheduler().list(me.id) });
  });

  r.post("/", async (c) => {
    const me = requireClient(c);
    const body = parseBody(scheduledPromptDraftSchema, await readJson(c));
    return c.json(promptScheduler().create(me.id, body), 201);
  });

  r.patch("/:id", async (c) => {
    const me = requireClient(c);
    const body = parseBody(patchBodySchema, await readJson(c));
    return c.json(promptScheduler().update(me.id, c.req.param("id"), body));
  });

  r.delete("/:id", (c) => {
    const me = requireClient(c);
    promptScheduler().delete(me.id, c.req.param("id"));
    return c.json({ ok: true });
  });

  r.post("/:id/run", async (c) => {
    const me = requireClient(c);
    const session = await promptScheduler().runNow(me.id, c.req.param("id"));
    return c.json({ sessionId: session.id });
  });

  return r;
}

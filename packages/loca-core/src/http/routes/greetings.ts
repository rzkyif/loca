// Greeting trigger. The greetings.* settings are now client-on-client (stored
// in the client's local file), so the CLIENT decides whether a given app start
// earns a greeting (it gates on greetings.enabled / greetings.runOn locally)
// and POSTs /run only when it should, carrying the session title + instruction
// it wants. The core just mints the session: it keeps a per-client dedup guard
// (so a crash-looping client can't spawn a session per retry) and defers the
// turn until the local model finishes loading (the connected edge that triggers
// this POST is exactly when llama begins loading on boot, so an immediate turn
// would race the load and error). The dedup window is the client-supplied
// cooldown (greetings.showCooldown), floored server-side, so a rapid re-show or
// crash loop still can't spam sessions. The client navigates via the
// session.created frame, not this response.

import { Hono } from "hono";
import { z } from "zod";
import { DEFAULT_GREETING_INSTRUCTION, errMessage } from "@loca/shared";
import { runAutomatedSession } from "../../services/automated-session.ts";
import { llmStillLoading } from "../../services/prompt-scheduler.ts";
import { parseBody, readJson } from "@loca/core-engine/http/body";
import { getLogger } from "../../shared/log.ts";
import { bearerMiddleware, requireClient } from "../middleware/auth.ts";

const log = getLogger("greetings");

// One greeting per client per window: a crash-looping client (or a buggy
// caller) must not mint a session per retry. Suppressed runs answer ran:false
// so an autostarted launch still reveals its window. The window is the client's
// cooldown when supplied; this is the fallback when it isn't.
const GREETING_MIN_INTERVAL_MS = 60_000;
// Absolute floor for the dedup window, so a buggy/rogue client can't drive the
// cooldown to zero and spam sessions.
const GREETING_INTERVAL_FLOOR_MS = 1_000;
// How long to wait for the local model before starting the turn anyway
// (an error then surfaces in the session, which still reveals the window).
const GREETING_READY_TIMEOUT_MS = 180_000;
const GREETING_READY_POLL_MS = 2_000;

const lastGreetingAtByClient = new Map<string, number>();

const runBodySchema = z
  .object({
    sessionTitle: z.string().optional(),
    instruction: z.string().optional(),
    cooldownMs: z.number().optional(),
  })
  .strict();

export function greetingsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.post("/run", async (c) => {
    const me = requireClient(c);
    const body = parseBody(runBodySchema, await readJson(c));
    const interval = Math.max(
      body.cooldownMs ?? GREETING_MIN_INTERVAL_MS,
      GREETING_INTERVAL_FLOOR_MS,
    );
    const last = lastGreetingAtByClient.get(me.id);
    if (last !== undefined && Date.now() - last < interval) {
      return c.json({ ran: false, reason: "recent" });
    }
    lastGreetingAtByClient.set(me.id, Date.now());
    // Prune entries past the dedup window so the map can't grow unbounded across
    // distinct client ids (each entry only matters for `interval`).
    const cutoff = Date.now() - interval;
    for (const [id, t] of lastGreetingAtByClient) {
      if (t < cutoff) lastGreetingAtByClient.delete(id);
    }
    const instruction = (body.instruction ?? "").trim() || DEFAULT_GREETING_INSTRUCTION;
    const sessionTitle = (body.sessionTitle ?? "").trim() || "Greeting {datetime}";
    void runGreetingWhenReady(me.id, sessionTitle, instruction);
    return c.json({ ran: true });
  });

  return r;
}

// Mirror of the scheduler's defer-while-loading: hold the greeting until the
// local model is up so the turn doesn't race the boot-time load.
async function runGreetingWhenReady(
  ownerClientId: string,
  title: string,
  instruction: string,
): Promise<void> {
  try {
    const deadline = Date.now() + GREETING_READY_TIMEOUT_MS;
    while (await llmStillLoading()) {
      if (Date.now() > deadline) {
        log.warn("local model still loading after wait budget; starting the greeting anyway");
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, GREETING_READY_POLL_MS));
    }
    await runAutomatedSession({
      ownerClientId,
      title,
      instruction,
      reason: "greeting",
      focus: "show_when_done",
    });
  } catch (err) {
    log.error(`greeting failed to start: ${errMessage(err)}`);
  }
}

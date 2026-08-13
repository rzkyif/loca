// Sidecar lifecycle routes. Status + manual stop/restart per kind.
//
// Start args are NOT user-supplied; they're computed from settings by
// services/sidecar-boot.ts. POST /restart re-applies the boot decision
// (effectively: stop + start with fresh args from the current settings),
// and POST /stop just terminates without re-spawning. /start exists as a
// convenience that's equivalent to /restart in this design.

import { Hono } from "hono";
import type { SidecarKind, SidecarSnapshot, SidecarsStatusResponse } from "@loca/shared";
import { sidecarManager } from "../../sidecars/manager.ts";
import { loadCoreSettings } from "@loca/core-engine/services/core-settings";
import {
  buildLlamaStartOptionsScaled,
  llamaMissingPrereq,
  llamaStartArgsFromSettings,
} from "../../sidecars/llama.ts";
import { buildSpeechStartOptions, speechDesiredState } from "../../sidecars/speech.ts";
import { sampleProcessMetrics } from "../../sidecars/process-metrics.ts";
import { AppError } from "@loca/core-engine";
import { bearerMiddleware } from "../middleware/auth.ts";

const SUPERVISED: ReadonlySet<SidecarKind> = new Set(["llama", "speech"]);

export function sidecarsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  // Live status + resource usage for the core process and its sidecars. The
  // set of sampled PIDs is fixed by this route (manager-tracked sidecars + the
  // core process); no PID is ever taken from the request.
  r.get("/status", async (c) => {
    const sidecars: SidecarSnapshot[] = sidecarManager().getStatuses();
    const pids = [Deno.pid, ...sidecars.flatMap((s) => (s.pid !== undefined ? [s.pid] : []))];
    const samples = await sampleProcessMetrics(pids);

    const withMetrics = sidecars.map((s): SidecarSnapshot => {
      const m = s.pid !== undefined ? samples.get(s.pid) : undefined;
      return m ? { ...s, rssMb: m.rssMb, cpuPct: m.cpuPct } : s;
    });
    const coreSample = samples.get(Deno.pid);
    const body: SidecarsStatusResponse = {
      sidecars: withMetrics,
      core: {
        pid: Deno.pid,
        rssMb: coreSample?.rssMb ?? 0,
        cpuPct: coreSample?.cpuPct ?? 0,
      },
    };
    return c.json(body);
  });

  r.post("/:kind/stop", async (c) => {
    const kind = c.req.param("kind") as SidecarKind;
    assertSupervised(kind);
    await sidecarManager().stop(kind);
    return c.body(null, 204);
  });

  // POST /:kind/start and POST /:kind/restart are equivalent: both
  // recompute the start args from current settings and (re-)start.
  for (const op of ["start", "restart"] as const) {
    r.post(`/:kind/${op}`, async (c) => {
      const kind = c.req.param("kind") as SidecarKind;
      assertSupervised(kind);
      const settings = await loadCoreSettings();
      if (kind === "llama") {
        const args = llamaStartArgsFromSettings(settings);
        if (!args) {
          throw new AppError(
            "validation_error",
            "llama-server cannot start: llm.provider is not local",
          );
        }
        const missing = await llamaMissingPrereq(args);
        if (missing) {
          throw new AppError("validation_error", `llama-server cannot start: ${missing}`);
        }
        await sidecarManager().restart("llama", await buildLlamaStartOptionsScaled(args));
      } else if (kind === "speech") {
        const state = await speechDesiredState(settings);
        if (!state.stt && !state.tts) {
          throw new AppError(
            "validation_error",
            "speech sidecar cannot start: STT is disabled/external and TTS is disabled (or its models are not on disk)",
          );
        }
        await sidecarManager().restart("speech", buildSpeechStartOptions(state));
      }
      return c.body(null, 204);
    });
  }

  return r;
}

function assertSupervised(kind: SidecarKind): void {
  if (!SUPERVISED.has(kind)) {
    throw new AppError("validation_error", `sidecar "${kind}" is not externally controllable`);
  }
}

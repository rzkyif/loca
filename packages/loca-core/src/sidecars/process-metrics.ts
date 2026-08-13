// Per-process CPU% + RSS sampling for the Services settings field. Backed by
// `pidusage` (cross-platform: /proc on Linux, `ps` on macOS, wmic/PowerShell on
// Windows). pidusage keeps per-PID history internally, so the ~2s on-demand
// polling the field does yields a meaningful CPU delta without us sampling on a
// background timer.
//
// Security: this helper is internal-only. The /sidecars/status route decides
// which PIDs to pass (its own tracked sidecars + the core process); it never
// forwards a PID from the client, so there is no arbitrary-process probe.

import pidusage from "pidusage";
import { getLogger } from "../shared/log.ts";

const log = getLogger("sidecars");

export interface ProcessSample {
  rssMb: number;
  cpuPct: number;
}

/** Sample CPU% + RSS for each PID. Returns a map keyed by PID; a PID that has
 *  already exited (or can't be read) is simply absent rather than failing the
 *  batch, so one dead sidecar never blanks the rest. */
export async function sampleProcessMetrics(pids: number[]): Promise<Map<number, ProcessSample>> {
  const out = new Map<number, ProcessSample>();
  const results = await Promise.allSettled(
    pids.map(async (pid): Promise<[number, ProcessSample]> => {
      const s = await pidusage(pid);
      return [pid, { rssMb: s.memory / 1024 / 1024, cpuPct: s.cpu }];
    }),
  );
  for (const r of results) {
    if (r.status === "fulfilled") {
      out.set(r.value[0], r.value[1]);
    } else {
      // Expected when a sidecar exits between PID collection and sampling.
      log.debug(`pidusage sample failed: ${r.reason}`);
    }
  }
  return out;
}

import type { SidecarsStatusResponse } from "@loca/shared";
import type { CoreClient } from "./client";

/** Sidecar status + live resource usage (Settings → Usage → Services). The
 *  core samples its own process + sidecar PIDs on each request; this is a thin
 *  wrapper polled while the Services field is visible. */
export class SidecarsApi {
  constructor(private readonly client: CoreClient) {}

  /** Sidecar snapshots (with rssMb/cpuPct) + the core process metrics. */
  status(): Promise<SidecarsStatusResponse> {
    return this.client.get<SidecarsStatusResponse>("/api/v1/sidecars/status");
  }

  /** Recompute start args from the current settings and (re)start a supervised
   *  sidecar. The recovery path for one stuck in Error: a fresh restart also
   *  clears the crash flap-guard, so a sidecar that gave up after repeated
   *  crashes gets a clean budget. */
  restart(kind: "llama" | "speech"): Promise<void> {
    return this.client.post<void>(`/api/v1/sidecars/${kind}/restart`, {});
  }
}

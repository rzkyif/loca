// Aggregate, core-reported lifecycle status, broadcast as a `core.status` WS
// frame (and on the health response). These are BACKEND states only: the client
// merges them with its own transport states (connecting / reconnecting /
// disconnected / unauthorized), which can never ride the wire.

import type { SidecarKind, SidecarStatus } from "./model.ts";

export const CORE_STATUSES = [
  "starting_up",
  "downloading",
  "idle",
  "busy",
  "updating",
  "error",
] as const;

export type CoreStatus = (typeof CORE_STATUSES)[number];

/** One subsystem (sidecar) the core has started. STT and TTS share the one
 *  `speech` process, so they appear as a single subsystem. */
export interface SubsystemStatus {
  kind: SidecarKind;
  status: SidecarStatus;
  /** Diagnostic text for a failed subsystem (log tail / exit code). */
  message?: string;
}

/** In-flight + waiting counts behind the `busy` state. */
export interface CoreQueues {
  llmActive: number;
  llmQueued: number;
  speechActive: number;
  speechQueued: number;
  activeStreams: number;
}

export interface CoreStatusSnapshot {
  status: CoreStatus;
  /** Short human detail for a tooltip (e.g. "loading whisper", "3 queued"). */
  detail?: string;
  /** Coarse 0..1 boot / load / download progress when known. */
  progress?: number;
  subsystems: SubsystemStatus[];
  /** Present while busy. */
  queues?: CoreQueues;
}

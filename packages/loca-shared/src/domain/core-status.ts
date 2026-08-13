// Aggregate, core-reported lifecycle status. The core derives one of these
// from its sidecar readiness, LLM/speech queue depth, active chat turns, and
// boot progress, and broadcasts it as a `core.status` WS frame (and on the
// health response). These are BACKEND states only: the client merges them with
// its own transport states (connecting / reconnecting / disconnected /
// unauthorized) when it paints the CoreBar, because a transport state can never
// ride the wire (you cannot receive a frame while disconnected).
//
// This is the SINGLE status frame: alongside the aggregate it carries the
// per-subsystem breakdown (`subsystems`) and, when busy, the queue counts
// (`queues`), so the CoreBar can fold every sidecar failure into the one `error`
// state and expand to show which subsystems broke and why. The client's
// per-sidecar facade (`serversState`) is rebuilt from `subsystems`.

import type { SidecarKind, SidecarStatus } from "./model.ts";

export const CORE_STATUSES = [
  // Boot / init: the core is up enough to answer health, but a required local
  // sidecar is still loading, so it cannot serve chat / speech yet.
  "starting_up",
  // A download (model file or sidecar binary) is active; the sidecars that
  // need the missing files sit at Disabled until they land.
  "downloading",
  // Ready, awaiting requests.
  "idle",
  // Serving a request (this client's or another's). New requests queue behind
  // the in-flight one rather than failing.
  "busy",
  // A self-update is staged or applying; a restart is imminent.
  "updating",
  // Unrecoverable: a required local sidecar is in a terminal Error state.
  "error",
] as const;

export type CoreStatus = (typeof CORE_STATUSES)[number];

/** One subsystem (sidecar) the core has started, with its current status and,
 *  when broken, the diagnostic message (its log tail or exit code). The CoreBar
 *  lists the errored ones in its expanded card; the client's `serversState`
 *  facade is rebuilt from the full set. STT and TTS share the one `speech`
 *  process, so they appear as a single subsystem. */
export interface SubsystemStatus {
  kind: SidecarKind;
  status: SidecarStatus;
  /** Diagnostic text for a failed subsystem (log tail / exit code). */
  message?: string;
}

/** In-flight + waiting counts behind the `busy` state, surfaced in the CoreBar's
 *  expanded card. "active" is processing now; "queued" is waiting for a slot. */
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
  /** Coarse 0..1 boot / load / download progress when known; drives a progress
   *  hint. */
  progress?: number;
  /** Every sidecar the core has started, with its status (and error message
   *  when failed). Always present; drives the per-subsystem fold + the client
   *  per-sidecar facade. */
  subsystems: SubsystemStatus[];
  /** Queue / in-flight counts, present while busy; drives the expanded card. */
  queues?: CoreQueues;
}

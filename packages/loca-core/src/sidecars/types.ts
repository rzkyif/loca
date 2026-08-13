// Shared types for the sidecar supervisor.

import type { SidecarKind, SidecarStatus } from "@loca/shared";

export type { SidecarKind, SidecarStatus };

// How the supervisor decides a freshly-spawned sidecar is "ready".
export type ReadinessCheck =
  | {
      kind: "http";
      // Must be loopback-only (127.0.0.1 / localhost). Validated at start().
      url: string;
      expectStatus?: number; // default: any 2xx
    }
  | {
      kind: "stdout";
      // Substring that the subprocess writes to stdout/stderr to signal ready.
      // Default: "READY\n".
      marker?: string;
    }
  | {
      kind: "warmup";
      // No active check; sleep this long after spawn, then declare Running.
      ms: number;
    };

// Restart-after-crash policy. unexpectedly-exited sidecars retry with
// exponential backoff, capped, up to maxAttempts.
export interface RestartPolicy {
  maxAttempts: number; // default: 5
  initialDelayMs: number; // default: 1_000
  maxDelayMs: number; // default: 30_000
}

export const DEFAULT_RESTART_POLICY: RestartPolicy = {
  maxAttempts: 5,
  initialDelayMs: 1_000,
  maxDelayMs: 30_000,
};

export interface StartOptions {
  // Absolute path to the binary. Caller is responsible for resolving the
  // platform-correct path (e.g. via binaries/manager.ts).
  binary: string;
  args: string[];
  env?: Record<string, string>;
  cwd?: string;
  readiness?: ReadinessCheck;
  startupTimeoutMs?: number; // default: 30_000
  restartPolicy?: RestartPolicy | "none";
  // If true, prepend the appropriate platform library-path env var with this
  // directory (DYLD_LIBRARY_PATH on macOS, LD_LIBRARY_PATH on Linux,
  // PATH on Windows). Also sets cwd to this dir on Windows so ggml backend
  // DLLs are found by scan-by-current-path.
  libraryDir?: string;
}

// Wire-shape consumed by routes/ws and any other subscribers. Matches the
// shared `SidecarSnapshot` from @loca/shared/domain/model.ts; defined
// locally here too so the manager doesn't import the wider domain.
export interface SidecarSnapshot {
  kind: SidecarKind;
  status: SidecarStatus;
  pid?: number;
  message?: string;
  // 0..1 progress hint during the loading phase. Absent when running.
  progress?: number;
}

export type StatusListener = (snapshot: SidecarSnapshot) => void;

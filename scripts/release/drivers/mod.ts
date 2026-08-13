// Build-environment driver interface for the all-targets release.
//
// Each non-host OS is built natively in its own environment, started on demand
// from the M4 host: Linux in a Podman container (podman.ts), Windows in a UTM VM
// (windows.ts). A driver is a dumb artifact producer: it builds its triples into
// the host's dist/ and returns an ArtifactBundle. Signing + upload stay on the
// host.
//
// SCAFFOLD STATUS: the drivers exist but are not yet wired into the live
// resolution in all-targets.ts, because they need machine-specific config (the
// LOCA_WIN_* / LOCA_PODMAN_* / LOCA_LINUX_BUILD_* keys in .env, read via
// cfg() in podman.ts / windows.ts; see .env.example). Once those are filled,
// activation is: register the drivers here and have all-targets.ts route each
// non-host triple to its driver via withEnvironment().

import type { Triple } from "../../../packages/loca-shared/src/domain/model.ts";
import type { ArtifactBundle, ClientDescriptor } from "../artifacts.ts";
import type { ReleaseChannel } from "../lib.ts";

/** Power state of a managed environment, so a run only stops what it started. */
export type EnvState = "RUNNING" | "STOPPED" | "ABSENT";

export interface CoreBuildRequest {
  /** Triples this environment should build (all same-OS as the environment). */
  triples: Triple[];
  channel: ReleaseChannel;
  /** Channel suffix on our binary names (channelBinSuffix). */
  suffix: string;
}

export interface ClientBuildRequest {
  /** Triples this environment should build the desktop client installer for. */
  triples: Triple[];
  channel: ReleaseChannel;
  /** Build-time values the in-environment build (build-release-bundle.ts
   *  --kind=client -> envFromProcess) needs. Every desktop installer (Windows
   *  MSI/NSIS, Linux AppImage) carries a Tauri updater `.sig`, so each signing
   *  environment is injected the Tauri PRIVATE key; the Ed25519 manifest key + R2
   *  creds never leave the host (signing/upload of the manifests stays there). */
  secrets: {
    /** Ed25519 PUBLIC key (base64); envFromProcess requires it. Never the private key. */
    signingPublicKeyB64: string;
    /** Tauri updater PUBLIC key; reconciled against tauri.conf.json. */
    tauriPublicKey: string;
    /** Tauri updater PRIVATE key + password; injected only by signing environments. */
    tauriPrivateKey: string;
    tauriPassword: string;
  };
}

export interface BuildEnvironment {
  /** Stable id used in logs + staging paths (e.g. "linux-podman"). */
  id: string;
  /** Triples this environment is responsible for building. */
  triples: Triple[];
  /** Usable on this machine right now: the tool is installed AND configured.
   *  Returns false while the .env driver config is unset, so an un-configured
   *  driver is skipped (never silently builds nothing). */
  available(): Promise<boolean>;
  /** Current power state, so withEnvironment only starts/stops what it started. */
  detectState(): Promise<EnvState>;
  /** Start the environment (no-op if already running). */
  ensureUp(): Promise<void>;
  /** Stop the environment if this run started it. Async (normal path). */
  teardown(): Promise<void>;
  /** Synchronous teardown for the signal-handler / process-exit backstop, so an
   *  interrupt can never leave a VM/container running. Best-effort. */
  teardownSync(): void;
  /** Build core + helpers + speech for the request's triples. Artifacts land
   *  under the host DIST_DIR (in the dist/<triple>/ layout compose* expects);
   *  returns the bundle describing them. */
  buildCore(req: CoreBuildRequest): Promise<ArtifactBundle>;
  /** Build the desktop client installer(s) for the request's triples in this
   *  environment (Tauri can't cross-OS compile), copy them under the host
   *  DIST_DIR, and return one descriptor per triple. Optional: an environment
   *  that doesn't build the client (or isn't wired yet) omits it and its triples
   *  carry forward in client.json. */
  buildClient?(req: ClientBuildRequest): Promise<ClientDescriptor[]>;
}

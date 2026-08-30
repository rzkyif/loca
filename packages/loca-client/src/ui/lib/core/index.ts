// Barrel for the core-API client. Components/state import from here so we
// have one stable surface: `import { cores, ApiError } from "$lib/core";`

export { ApiError, CoreClient } from "./client";
export type { CoreEndpoint, WsListener } from "./client";

export {
  cores,
  dropTokenlessCores,
  dropUnauthorizedCore,
  ensureLocalCoreUpIfNeeded,
  isLocalCoreEntry,
  isLoopbackUrl,
  isThisDevicesCore,
  LOCAL_CORE_ID,
  pruneUninstalledLocalCores,
} from "./cores";
export type { PairedCoreEntry } from "./cores";

export type {
  McpServerInput,
  ScheduledPromptPatch,
  SttCatalogResponse,
  TtsCatalogResponse,
} from "./api";
export {
  mintCodeWithAdminToken,
  PairingApi,
  pairWithCode,
  probeCore,
  setAdminPasswordWithToken,
} from "./pairing";
export type { PairResult } from "./pairing";

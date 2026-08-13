// Barrel for the core-API client. Components/state import from here so we
// have one stable surface: `import { cores, ApiError } from "$lib/core";`

export { ApiError, CoreClient } from "./client";
export type { CoreEndpoint, WsListener } from "./client";

export {
  cores,
  ensureLocalCoreUpIfNeeded,
  isLoopbackUrl,
  isThisDevicesCore,
  pruneUninstalledLocalCores,
} from "./cores";
export type { PairedCoreEntry } from "./cores";

export { SessionsApi } from "./sessions";
export { ChatApi } from "./chat";
export { ModelsApi } from "./models";
export { BinariesApi } from "./binaries";
export { RequirementsApi } from "./requirements";
export { StorageApi } from "./storage";
export { ExtensionsApi } from "./extensions";
export { MemoriesApi } from "./memories";
export { ScheduledPromptsApi } from "./scheduled-prompts";
export { GreetingsApi } from "./greetings";
export { SttApi } from "./stt";
export { TtsApi } from "./tts";
export { CoreSettingsApi } from "./settings";
export {
  mintCodeWithAdminToken,
  PairingApi,
  pairWithCode,
  probeCore,
  setAdminPasswordWithToken,
} from "./pairing";
export type { PairResult } from "./pairing";
export { UpdateApi } from "./update";

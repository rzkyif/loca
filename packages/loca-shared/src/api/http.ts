// REST request / response shapes for the /api/v1/* surface.
// Many bodies are inferred from validation schemas under ./../validation/;
// types unique to specific endpoints live here.

import type { Message, Session, SessionListEntry, TokenUsage } from "../domain/session.ts";
import type {
  Extension,
  ExtensionSearchResult,
  ExtensionSource,
  Grant,
  OpenAIToolDef,
  PermissionDecl,
  Tool,
  ToolDescriptor,
} from "../domain/extension.ts";
import type {
  BinaryKind,
  BinaryProbeResult,
  BinaryStatus,
  DownloadEntry,
  DownloadPlan,
  ModelEntry,
  RequirementsSnapshot,
  SidecarSnapshot,
} from "../domain/model.ts";
import type { StorageTree } from "../domain/storage.ts";
import type { CoreStatusSnapshot } from "../domain/core-status.ts";

export const API_BASE = "/api/v1";

// --- Public ----------------------------------------------------------------

/** What a Core can do, as opposed to what the device rendering its UI can do.
 *  Those are different questions, and confusing them gets the interesting case
 *  backwards: a phone paired to a desktop Core must still offer extensions,
 *  scheduled prompts and stdio MCP servers, because all three are properties of
 *  the Core, not of the screen. Device concerns (touch affordances, sheets,
 *  soft-keyboard behaviour, window geometry) stay on the platform flag and the
 *  settings schema's `desktopOnly`; these do not.
 *
 *  A client caches the block on connect. Absent means "a Core that predates
 *  negotiation", which is treated as fully capable rather than fully incapable:
 *  a false negative hides a working feature with no affordance to discover it,
 *  where a false positive shows a control that fails with a worded reason. */
export interface CoreCapabilities {
  /** Extension tools can actually RUN. Installing is not the question: an
   *  extension installs fine with no worker spawner and then every one of its
   *  tools fails at call time, which is the state worth hiding. */
  extensions: boolean;
  /** A language model can run inside Core, so a local model selection is
   *  servable rather than a download with nothing to load it. */
  localInference: boolean;
  /** Speech recognition and synthesis can run inside Core. */
  localSpeech: boolean;
  /** A saved schedule will actually fire. The routes serve either way, so a
   *  prompt already saved stays readable and deletable rather than invisible;
   *  what is false here is the timer that arms it. */
  scheduledPrompts: boolean;
  /** A local (spawned) MCP server can be started. Remote MCP servers are
   *  unaffected, and they are the half that works everywhere. */
  stdioMcp: boolean;
  /** **Inbound only: this Core can accept NEW paired clients.**
   *
   *  It is not "this device can pair". A client pairing OUT to a remote Core is
   *  unaffected by this flag; what is false on an embedded Core is that
   *  anything can pair TO it, because it has no listener by design. So this
   *  gates minting a pairing code and revoking a device on THIS Core, never the
   *  wizard that adds a remote one. */
  pairing: boolean;
  /** This Core can replace its own binary. Named `coreUpdate` rather than
   *  `update` because on mobile the CLIENT self-updates while Core cannot,
   *  being inside the app bundle, and a bare `update` invites hiding the wrong
   *  affordance. */
  coreUpdate: boolean;
}

export interface HealthResponse {
  status: "ok";
  version: string;
  uptimeMs: number;
  /** What this Core can do. Read on connect so the client negotiates rather
   *  than inferring capability from the platform it renders on. */
  capabilities: CoreCapabilities;
  /** Aggregate lifecycle status, so a client can seed the CoreBar on select
   *  without waiting for the first `core.status` WS frame. */
  core: CoreStatusSnapshot;
  /** Whether an admin password has been set. When true, an already-paired
   *  client can mint pairing codes / revoke other devices with the password
   *  (no need to read the on-disk admin token). Always true after a normal
   *  install, which sets one. */
  adminPasswordSet: boolean;
  /** Whether Core is served over HTTPS by a terminating reverse proxy. When
   *  true, the Client validates the proxy's real certificate (standard WebPKI)
   *  instead of pinning Core's self-signed one, and pairing drops the cert
   *  binding. An unauthenticated hint the pairing handshake verifies: a forged
   *  value cannot complete pairing (see the cert fold in services/auth.ts). */
  behindProxy: boolean;
}

// --- Sessions --------------------------------------------------------------

export type ListSessionsResponse = SessionListEntry[];

export interface CreateSessionRequest {
  title?: string;
  /** A scratch session the client discards when it moves on: creating any other
   *  session, or navigating away, orphans it. Absent means a normal session. */
  temporary?: boolean;
}
export type CreateSessionResponse = Session;

export interface GetSessionResponse extends Session {
  messages: Message[];
}

export interface PatchSessionRequest {
  title?: string;
}
export type PatchSessionResponse = Pick<Session, "id" | "title">;

export type AppendMessageRequest = Message;
export interface AppendMessageResponse {
  id: string;
  ord: number;
}

export type PatchMessageRequest = Partial<Message>;
export interface PatchMessageResponse {
  id: string;
  ord: number;
}

export interface UploadAttachmentResponse {
  id: string;
  absPath: string;
  filename: string;
}

// --- STT -------------------------------------------------------------------

export interface SttTranscribeResponse {
  text: string;
}

export interface SttStatusResponse {
  provider: "local" | "external";
  running: boolean;
}

// --- TTS -------------------------------------------------------------------

export interface TtsSynthesizeRequest {
  text: string;
  voice?: string;
  speed?: number;
}

export interface TtsVoiceEntry {
  id: string;
  label: string;
  lang?: string;
}
export type TtsVoicesResponse = TtsVoiceEntry[];

export interface TtsStatusResponse {
  loaded: boolean;
  loading: boolean;
}

// --- Models ----------------------------------------------------------------

export type ListModelsResponse = ModelEntry[];

export interface DownloadModelsRequest {
  items: Array<{ source: string; group?: "llm" | "stt" | "tts" | "embed" }>;
}
export interface DownloadModelsResponse {
  jobIds: string[];
}

export interface ProbeModelsRequest {
  sources: string[];
}
export type ProbeModelsResponse = DownloadPlan[];

export type ListDownloadsResponse = DownloadEntry[];

// --- Binaries --------------------------------------------------------------

export type ListBinariesResponse = BinaryStatus[];

export interface InstallBinariesRequest {
  kinds?: BinaryKind[];
}
export interface InstallBinariesResponse {
  jobIds: string[];
}

export interface UpdateBinaryRequest {
  kind: BinaryKind;
}
export interface UpdateBinaryResponse {
  jobId: string;
}

export interface BinaryUpdateCheck {
  kind: BinaryKind;
  installed: boolean;
  installedVersion: string | null;
  latestVersion: string;
  available: boolean;
}
export type CheckBinariesResponse = BinaryUpdateCheck[];

export interface ProbeBinariesRequest {
  kinds: BinaryKind[];
}
export type ProbeBinariesResponse = BinaryProbeResult[];

// --- Requirements ----------------------------------------------------------

export type GetRequirementsResponse = RequirementsSnapshot;
export interface DownloadRequirementsResponse {
  modelJobIds: string[];
  binaryJobIds: string[];
}

// --- Storage ---------------------------------------------------------------

export type GetStorageResponse = StorageTree;
export interface DeleteStoragePathsRequest {
  paths: string[];
}

export interface ClearStorageCategoryRequest {
  /** Category to empty of its non-locked items. For `settings` this is a
   *  factory reset: defaults restored and secrets wiped. */
  categoryId: string;
}

// --- Core self-update ------------------------------------------------------

export interface UpdateCheckResponse {
  currentVersion: string;
  latestVersion: string;
  available: boolean;
  manifestUrl: string;
}

export interface UpdateApplyRequest {
  version?: string;
}

// --- Extensions --------------------------------------------------------------

export type ListExtensionsResponse = Extension[];

export interface SearchExtensionsResponse {
  results: ExtensionSearchResult[];
}

// Acquire a extension's files (POST /download): fetch + extract an npm tarball,
// copy a seeded extension (built-in / samples), or register a locally dropped-in
// folder. Deps are NOT installed here; that is the separate POST /:id/install step.
export type DownloadExtensionRequest =
  | { source: "npm"; name: string; version?: string }
  | { source: "local"; path: string; slug?: string }
  | { source: "seeded"; id: string };

// Returned by every endpoint that starts a streamed background job (download,
// install-deps, update). Progress + completion arrive over the
// extension.install_log / extension.install_done WS frames, keyed by `jobId`.
export interface ExtensionJobResponse {
  jobId: string;
  extensionId: string;
}

// Returned by the synchronous extension actions: enable-all / disable-all /
// confirm-reenable.
export interface ExtensionActionResponse {
  id: string;
}

export interface UpdateExtensionRequest {
  version?: string;
}

export interface ListExtensionToolsResponse {
  tools: Tool[];
}

// Check installed extensions for newer versions. With no `ids`, every installed
// extension is checked. npm extensions resolve `dist-tags.latest`; the built-in
// resolves its signed manifest version; local extensions have no upstream and
// report `latestVersion: null`.
export interface CheckUpdatesRequest {
  ids?: string[];
}
export interface ExtensionUpdateStatus {
  id: string;
  installedVersion: string;
  latestVersion: string | null;
  updateAvailable: boolean;
  error?: string;
}
export interface CheckUpdatesResponse {
  results: ExtensionUpdateStatus[];
}

export interface SetGrantsRequest {
  grants: Array<{ key: string; state: Grant["state"] }>;
}
export interface SetGrantsResponse {
  grants: Grant[];
}

export interface ToolFilterRequest {
  vector: number[];
  topK?: number;
}
export interface ToolFilterResponse {
  candidates: ToolDescriptor[];
  /** The always-available set, returned alongside the cosine candidates rather
   *  than merged into them: the caller force-includes these ahead of the phase-2
   *  filter, so they must stay distinguishable. */
  alwaysAvailable: ToolDescriptor[];
  /** Tools the query names directly. Force-included ahead of the candidates and
   *  the phase-2 filter, so a tool the user names by hand always reaches the
   *  model even with no fresh embedding or a rank past topK. */
  nameMatched: ToolDescriptor[];
}

export interface ToolSchemasRequest {
  ids: string[];
}
export interface ToolSchemasResponse {
  tools: OpenAIToolDef[];
}

export interface EmbedRequest {
  texts: string[];
}
export interface EmbedResponse {
  vectors: number[][];
}

// --- LLM utility (single-shot) --------------------------------------------

export interface AutocorrectRequest {
  text: string;
}
export interface AutocorrectResponse {
  text: string;
}

export interface MergeTranscriptionRequest {
  existing: string;
  next: string;
}
export interface MergeTranscriptionResponse {
  text: string;
}

// --- Settings --------------------------------------------------------------

// CoreSettings shape is sparse (only non-defaults persisted).
// Concrete field definitions live in domain/settings.ts (added in a later
// milestone). The wire format is just a record.
export type GetCoreSettingsResponse = Record<string, unknown>;
export type PatchCoreSettingsRequest = Record<string, unknown>;
export type PatchCoreSettingsResponse = Record<string, unknown>;

// --- Sidecars --------------------------------------------------------------

/** Live resource sample for a single process. */
export interface ProcessMetricsLite {
  pid: number;
  rssMb: number;
  cpuPct: number;
}

/** Response of GET /api/v1/sidecars/status. The route samples only its own
 *  tracked processes (sidecar PIDs + the core process); it never accepts a PID
 *  from the caller. `sidecars` carries llama/whisper (and tts when loaded) with
 *  `rssMb`/`cpuPct` populated; `core` is the core process itself. */
export interface SidecarsStatusResponse {
  sidecars: SidecarSnapshot[];
  core: ProcessMetricsLite;
}

// --- Helpers shared by clients --------------------------------------------

// Re-export commonly needed enums so client code can import everything from
// one entry without crossing into `domain/*`.
export type { ExtensionSource, PermissionDecl, TokenUsage };

// Every per-domain wrapper over one CoreClient, in one place. Each is a thin
// map from a call to a REST path or a WS frame; `cores().select()` rebuilds the
// whole object when the current core changes.

import type {
  AppliedModelSettings,
  AppliedSttSettings,
  AppliedTtsSettings,
  AskUserAnswer,
  AutocorrectRequest,
  AutocorrectResponse,
  BinaryKind,
  CatalogModelView,
  CheckBinariesResponse,
  CheckUpdatesResponse,
  DeleteStoragePathsRequest,
  DownloadExtensionRequest,
  DownloadModelsRequest,
  DownloadModelsResponse,
  DownloadRequirementsResponse,
  Extension,
  ExtensionActionResponse,
  ExtensionJobResponse,
  GetRequirementsResponse,
  GetSessionResponse,
  GetStorageResponse,
  Grant,
  ListDownloadsResponse,
  ListExtensionsResponse,
  ListExtensionToolsResponse,
  McpPrompt,
  McpResource,
  McpServer,
  Memory,
  MemoryMeta,
  MergeTranscriptionRequest,
  MergeTranscriptionResponse,
  Message,
  PresetBucket,
  RecommendationSet,
  ScheduledPrompt,
  ScheduledPromptDraft,
  ScheduleSpec,
  SearchExtensionsResponse,
  Session,
  SessionListEntry,
  SetGrantsResponse,
  SidecarsStatusResponse,
  SttCatalogModel,
  SttPresetView,
  SttTranscribeResponse,
  TtsCatalogModel,
  TtsPresetView,
  TtsStatusResponse,
  TtsSynthesizeRequest,
  TtsVoicesResponse,
  UndeclaredPolicy,
  UpdateBinaryResponse,
  UpdateCheckResponse,
} from "@loca/shared";
import type { CoreClient } from "./client";
import { PairingApi } from "./pairing";

export interface CatalogResponse {
  generatedAt: string;
  models: CatalogModelView[];
}

export interface SelectModelResponse {
  applied: { preset: string; settings: AppliedModelSettings };
}

export interface SttCatalogResponse {
  generatedAt: string;
  models: SttCatalogModel[];
  presets: SttPresetView[];
}

export interface SelectSttModelResponse {
  applied: { preset: string; settings: AppliedSttSettings };
}

export interface TtsCatalogResponse {
  generatedAt: string;
  models: TtsCatalogModel[];
  presets: TtsPresetView[];
}

export interface SelectTtsModelResponse {
  applied: { preset: string; settings: AppliedTtsSettings };
}

export interface ScheduledPromptPatch {
  title?: string;
  instruction?: string;
  schedule?: ScheduleSpec;
  runMissed?: boolean;
  enabled?: boolean;
}

export interface McpServerInput {
  name: string;
  kind: "stdio" | "remote";
  command?: string;
  args?: string[];
  runtime?: "custom" | "deno";
  denoAllowAll?: boolean;
  denoPermissions?: string[];
  url?: string;
  remoteAuth?: "none" | "bearer" | "oauth";
  enabled?: boolean;
  /** Write-only: omit to leave unchanged, "" to clear. The core only ever
   *  echoes back `hasAuth`. */
  authToken?: string;
}

export interface GreetingRunInput {
  sessionTitle: string;
  instruction: string;
  /** Least time between greetings for this client, in ms. The core dedups on
   *  this value (floored server-side), so a rapid re-show can't spam sessions. */
  cooldownMs: number;
}

type ModelSelection = { modelId: string } | { modelSpec: string };

const enc = encodeURIComponent;

export function buildApi(c: CoreClient) {
  return {
    sessions: {
      list: () => c.get<SessionListEntry[]>("/api/v1/sessions"),
      create: (title?: string, opts?: { temporary?: boolean }) =>
        c.post<Session>("/api/v1/sessions", { title, temporary: opts?.temporary }),
      get: (id: string) => c.get<GetSessionResponse>(`/api/v1/sessions/${enc(id)}`),
      patchTitle: (id: string, title: string) =>
        c.patch<{ id: string; title: string }>(`/api/v1/sessions/${enc(id)}`, { title }),
      regenerateTitle: (id: string) =>
        c.post<void>(`/api/v1/sessions/${enc(id)}/regenerate-title`, {}),
      delete: (id: string) => c.del<void>(`/api/v1/sessions/${enc(id)}`),
      appendMessage: (sessionId: string, message: Message) =>
        c.post<{ id: string; ord: number }>(`/api/v1/sessions/${enc(sessionId)}/messages`, message),
      patchMessage: (sessionId: string, messageId: string, patch: Partial<Message>) =>
        c.patch<{ id: string; ord: number }>(
          `/api/v1/sessions/${enc(sessionId)}/messages/${enc(messageId)}`,
          patch,
        ),
      deleteMessage: (sessionId: string, messageId: string) =>
        c.del<void>(`/api/v1/sessions/${enc(sessionId)}/messages/${enc(messageId)}`),
      uploadAttachment: (sessionId: string, messageId: string, file: Blob, filename: string) => {
        const form = new FormData();
        form.set("file", file, filename);
        form.set("messageId", messageId);
        return c.postForm<{ id: string; absPath: string; filename: string }>(
          `/api/v1/sessions/${enc(sessionId)}/attachments`,
          form,
        );
      },
      attachmentUrl: (sessionId: string, attachmentId: string): string =>
        `${c.endpoint.baseUrl}/api/v1/sessions/${enc(sessionId)}/attachments/${enc(attachmentId)}`,
    },

    // Streaming is server-driven; these just send control frames.
    chat: {
      start: (
        streamId: string,
        sessionId: string,
        // Omitted, not defaulted, and the frame leaves it out when it is absent.
        // Core reads an absent `route` as "the client did not pin one" and only
        // then consults `dualModel.enabled`; sending "default" pins the default
        // route and suppresses auto-routing entirely.
        route?: "default" | "secondary",
        opts?: { systemPrompt?: string; toolsHint?: string; anchorMessageId?: string },
      ) =>
        c.sendWs({
          kind: "chat.start",
          streamId,
          sessionId,
          ...(route !== undefined ? { route } : {}),
          ...(opts?.systemPrompt !== undefined ? { systemPrompt: opts.systemPrompt } : {}),
          ...(opts?.toolsHint ? { toolsHint: opts.toolsHint } : {}),
          ...(opts?.anchorMessageId ? { anchorMessageId: opts.anchorMessageId } : {}),
        }),
      interrupt: (streamId: string) => c.sendWs({ kind: "chat.interrupt", streamId }),
      /** (Re)attach this client to a session: a turn still generating on it
       *  re-emits its messages so far and any open tool prompt, then live
       *  deltas resume. A no-op server-side when nothing is in flight. */
      subscribe: (sessionId: string) => c.sendWs({ kind: "chat.subscribe", sessionId }),
      respondAskUser: (callId: string, requestId: string, answers: AskUserAnswer[]) =>
        c.sendWs({ kind: "tool.askuser_response", callId, requestId, answers }),
      respondPermission: (callId: string, requestId: string, allow: boolean) =>
        c.sendWs({ kind: "tool.permission_response", callId, requestId, allow }),
      /** `draft` carries the user's (possibly edited) version and must be
       *  present when accepted. */
      respondScheduleConfirm: (
        callId: string,
        requestId: string,
        accepted: boolean,
        draft?: ScheduledPromptDraft,
      ) =>
        c.sendWs({
          kind: "schedule.confirm_response",
          callId,
          requestId,
          accepted,
          ...(draft ? { draft } : {}),
        }),
      cancelTool: (callId: string) => c.sendWs({ kind: "tool.cancel", callId }),
    },

    models: {
      download: (req: DownloadModelsRequest) =>
        c.post<DownloadModelsResponse>("/api/v1/models/download", req),
      downloads: () => c.get<ListDownloadsResponse>("/api/v1/models/downloads"),
      cancel: (id: string) => c.post<void>(`/api/v1/models/downloads/${enc(id)}/cancel`, {}),
      retry: (id: string) => c.post<void>(`/api/v1/models/downloads/${enc(id)}/retry`, {}),
      remove: (id: string) => c.del<void>(`/api/v1/models/downloads/${enc(id)}`),
      /** The three computed presets for this device + what's currently applied. */
      recommend: () => c.get<RecommendationSet>("/api/v1/models/recommend"),
      /** Force-refresh the catalog + re-probe hardware, then recompute. */
      recheck: () => c.post<RecommendationSet>("/api/v1/models/recommend/recheck", {}),
      /** Full model list annotated with fit-on-this-device (the browser). */
      catalog: () => c.get<CatalogResponse>("/api/v1/models/catalog"),
      /** Apply a preset bucket, a catalog model (its recommended quant), or a
       *  specific quant by its modelSpec: writes llm.* settings. */
      select: (sel: { bucket: PresetBucket } | ModelSelection) =>
        c.post<SelectModelResponse>("/api/v1/models/select", sel),
      sttCatalog: () => c.get<SttCatalogResponse>("/api/v1/models/stt/catalog"),
      /** Apply a curated card, a catalog model (its default quant), or a
       *  specific quant by its modelSpec: writes stt.* settings. */
      sttSelect: (sel: { presetId: string } | ModelSelection) =>
        c.post<SelectSttModelResponse>("/api/v1/models/stt/select", sel),
      ttsCatalog: () => c.get<TtsCatalogResponse>("/api/v1/models/tts/catalog"),
      /** The tts.* counterpart of `sttSelect`. */
      ttsSelect: (sel: { presetId: string } | ModelSelection) =>
        c.post<SelectTtsModelResponse>("/api/v1/models/tts/select", sel),
    },

    binaries: {
      update: (kind: BinaryKind) =>
        c.post<UpdateBinaryResponse>("/api/v1/binaries/update", { kind }),
      check: () => c.get<CheckBinariesResponse>("/api/v1/binaries/check"),
    },

    requirements: {
      /** `probe:true` fills size/version hints via outbound HEADs and must be
       *  sent ONLY from an explicit user action (opening the Pending Downloads
       *  modal), never on the always-on connect/settings refetch. */
      get: (opts?: { probe?: boolean }) =>
        c.get<GetRequirementsResponse>(`/api/v1/requirements${opts?.probe ? "?probe=1" : ""}`),
      download: () => c.post<DownloadRequirementsResponse>("/api/v1/requirements/download", {}),
    },

    sidecars: {
      status: () => c.get<SidecarsStatusResponse>("/api/v1/sidecars/status"),
      /** Recompute start args from the current settings and (re)start a
       *  supervised sidecar. Also clears the crash flap-guard, so one that gave
       *  up after repeated crashes gets a clean budget. */
      restart: (kind: "llama" | "speech") => c.post<void>(`/api/v1/sidecars/${kind}/restart`, {}),
    },

    storage: {
      get: () => c.get<GetStorageResponse>("/api/v1/storage"),
      /** The core refuses anything in use. */
      deletePaths: (paths: string[]) =>
        c.post<void>("/api/v1/storage/delete", { paths } satisfies DeleteStoragePathsRequest),
      /** Clear a category's non-locked items. For "settings" this is a factory
       *  reset (defaults + wiped secrets). */
      clearCategory: (categoryId: string) => c.post<void>("/api/v1/storage/clear", { categoryId }),
    },

    extensions: {
      list: () => c.get<ListExtensionsResponse>("/api/v1/extensions"),
      search: (q: string, opts: { limit?: number; offset?: number } = {}) => {
        const params = new URLSearchParams({ q });
        if (opts.limit !== undefined) params.set("limit", String(opts.limit));
        if (opts.offset !== undefined) params.set("offset", String(opts.offset));
        return c.get<SearchExtensionsResponse>(`/api/v1/extensions/search?${params}`);
      },
      /** Phase 1: acquire an extension's files (npm/local/builtin). Deps are
       *  installed separately via installDeps. */
      download: (req: DownloadExtensionRequest) =>
        c.post<ExtensionJobResponse>("/api/v1/extensions/download", req),
      /** Phase 2: install a downloaded extension's dependencies + pin its hash. */
      installDeps: (id: string) =>
        c.post<ExtensionJobResponse>(`/api/v1/extensions/${enc(id)}/install`, {}),
      /** Installs now if the worker runtime is present, else `{ queued: true }`. */
      installBuiltin: () => c.post<{ queued: boolean }>("/api/v1/extensions/builtin-install", {}),
      delete: (id: string) => c.del<void>(`/api/v1/extensions/${enc(id)}`),
      /** Revert to 'downloaded' (drop installed deps). Source files stay. */
      uninstall: (id: string) =>
        c.post<ExtensionActionResponse>(`/api/v1/extensions/${enc(id)}/uninstall`, {}),
      update: (id: string, version?: string) =>
        c.post<ExtensionJobResponse>(`/api/v1/extensions/${enc(id)}/update`, { version }),
      /** Omit `ids` to check every installed extension. */
      checkUpdates: (ids?: string[]) =>
        c.post<CheckUpdatesResponse>("/api/v1/extensions/check-updates", { ids }),
      listTools: (id: string) =>
        c.get<ListExtensionToolsResponse>(`/api/v1/extensions/${enc(id)}/tools`),
      /** Flat list of every tool from every provider (backs the Tools manager). */
      listAllTools: () => c.get<ListExtensionToolsResponse>("/api/v1/tools"),
      /** Re-pin the current on-disk content + clear the drift warning. */
      confirmReenable: (id: string) =>
        c.post<ExtensionActionResponse>(`/api/v1/extensions/${enc(id)}/confirm-reenable`, {}),
      enableTool: (id: string, tool: string) =>
        c.post<{ ok: boolean }>(`/api/v1/extensions/${enc(id)}/tools/${enc(tool)}/enable`, {}),
      disableTool: (id: string, tool: string) =>
        c.post<{ ok: boolean }>(`/api/v1/extensions/${enc(id)}/tools/${enc(tool)}/disable`, {}),
      setToolAlwaysAvailable: (id: string, tool: string, value: boolean) =>
        c.post<{ ok: boolean }>(
          `/api/v1/extensions/${enc(id)}/tools/${enc(tool)}/always-available`,
          { value },
        ),
      setGrants: (
        id: string,
        tool: string,
        grants: Array<{ key: string; state: Grant["state"] }>,
      ) =>
        c.post<SetGrantsResponse>(`/api/v1/extensions/${enc(id)}/tools/${enc(tool)}/grants`, {
          grants,
        }),
      setUndeclaredPolicy: (id: string, policy: UndeclaredPolicy) =>
        c.post<Extension>(`/api/v1/extensions/${enc(id)}/undeclared-policy`, { policy }),
      reindex: () => c.post<{ embedded: number }>("/api/v1/extensions/reindex", {}),
      rescan: () =>
        c.post<{ added: number; updated: number; removed: number }>(
          "/api/v1/extensions/rescan",
          {},
        ),
    },

    // Memories live on the core (markdown files + index rows); the client only
    // mirrors the metadata list for the settings manager and @-autocomplete.
    memories: {
      list: async (): Promise<MemoryMeta[]> =>
        (await c.get<{ memories: MemoryMeta[] }>("/api/v1/memories")).memories,
      create: (kind: "knowledge" | "skill", title: string, content = "") =>
        c.post<Memory>("/api/v1/memories", { kind, title, content }),
      get: (id: string) => c.get<Memory>(`/api/v1/memories/${enc(id)}`),
      update: (id: string, patch: { title?: string; content?: string; enabled?: boolean }) =>
        c.patch<Memory>(`/api/v1/memories/${enc(id)}`, patch),
      delete: (id: string) => c.del<void>(`/api/v1/memories/${enc(id)}`),
      getFile: async (id: string, name: string): Promise<string> =>
        (await c.get<{ content: string }>(`/api/v1/memories/${enc(id)}/files/${enc(name)}`))
          .content,
      putFile: (id: string, name: string, content: string) =>
        c.put<{ ok: true }>(`/api/v1/memories/${enc(id)}/files/${enc(name)}`, { content }),
      deleteFile: (id: string, name: string) =>
        c.del<void>(`/api/v1/memories/${enc(id)}/files/${enc(name)}`),
      rescan: () =>
        c.post<{ added: number; removed: number; changed: number }>("/api/v1/memories/rescan", {}),
    },

    // MCP servers live on the core (it owns the connections); the client
    // mirrors the projected state for settings and the "/" + "@" autocomplete.
    mcp: {
      list: async (): Promise<McpServer[]> =>
        (await c.get<{ servers: McpServer[] }>("/api/v1/mcp")).servers,
      listPrompts: async (): Promise<McpPrompt[]> =>
        (await c.get<{ prompts: McpPrompt[] }>("/api/v1/mcp/prompts")).prompts,
      listResources: async (): Promise<McpResource[]> =>
        (await c.get<{ resources: McpResource[] }>("/api/v1/mcp/resources")).resources,
      create: (input: McpServerInput) => c.post<McpServer>("/api/v1/mcp", input),
      update: (id: string, input: Partial<McpServerInput>) =>
        c.patch<McpServer>(`/api/v1/mcp/${enc(id)}`, input),
      delete: (id: string) => c.del<void>(`/api/v1/mcp/${enc(id)}`),
      reconnect: (id: string) => c.post<McpServer>(`/api/v1/mcp/${enc(id)}/reconnect`, {}),
      /** Begin OAuth sign-in: the URL to open in a browser, or null if stored
       *  tokens already authorized. Completion arrives later as an mcp.snapshot
       *  once the redirect lands on core's loopback listener. */
      startOAuth: (id: string) =>
        c.post<{ authorizationUrl: string | null }>(`/api/v1/mcp/${enc(id)}/oauth/start`, {}),
      setToolEnabled: (id: string, tool: string, enabled: boolean) =>
        c.post<McpServer>(
          `/api/v1/mcp/${enc(id)}/tools/${enc(tool)}/${enabled ? "enable" : "disable"}`,
          {},
        ),
      /** Resolve a prompt's messages into one instruction string (a live server
       *  round-trip), called at send time to fold a `/prompt` into the turn. */
      resolvePrompt: async (
        id: string,
        prompt: string,
        args: Record<string, string>,
      ): Promise<string> =>
        (
          await c.post<{ text: string }>(`/api/v1/mcp/${enc(id)}/prompts/${enc(prompt)}/resolve`, {
            arguments: args,
          })
        ).text,
      setToolAlwaysAvailable: (id: string, tool: string, alwaysAvailable: boolean) =>
        c.post<McpServer>(
          `/api/v1/mcp/${enc(id)}/tools/${enc(tool)}/always-available/${
            alwaysAvailable ? "enable" : "disable"
          }`,
          {},
        ),
      setPromptEnabled: (id: string, prompt: string, enabled: boolean) =>
        c.post<McpServer>(
          `/api/v1/mcp/${enc(id)}/prompts/${enc(prompt)}/${enabled ? "enable" : "disable"}`,
          {},
        ),
    },

    scheduledPrompts: {
      list: async (): Promise<ScheduledPrompt[]> =>
        (await c.get<{ scheduledPrompts: ScheduledPrompt[] }>("/api/v1/scheduled-prompts"))
          .scheduledPrompts,
      create: (draft: ScheduledPromptDraft) =>
        c.post<ScheduledPrompt>("/api/v1/scheduled-prompts", draft),
      update: (id: string, patch: ScheduledPromptPatch) =>
        c.patch<ScheduledPrompt>(`/api/v1/scheduled-prompts/${enc(id)}`, patch),
      delete: (id: string) => c.del<void>(`/api/v1/scheduled-prompts/${enc(id)}`),
      run: (id: string) =>
        c.post<{ sessionId: string }>(`/api/v1/scheduled-prompts/${enc(id)}/run`, {}),
    },

    // The greetings.* settings are client-local: the caller gates on them and
    // only calls this when a greeting should run. The core mints the session
    // (after a per-client dedup + model-ready wait) and the client navigates on
    // the session.created frame.
    greetings: {
      run: (input: GreetingRunInput) =>
        c.post<{ ran: boolean; sessionId?: string }>("/api/v1/greetings/run", input),
    },

    stt: {
      transcribe: (audio: Blob, language?: string) => {
        const form = new FormData();
        form.set("audio", audio, "audio.wav");
        if (language) form.set("language", language);
        return c.postForm<SttTranscribeResponse>("/api/v1/stt/transcribe", form);
      },
    },

    tts: {
      load: async (): Promise<void> => {
        await c.post("/api/v1/tts/load", {});
      },
      unload: async (): Promise<void> => {
        await c.post("/api/v1/tts/unload", {});
      },
      /** A WAV blob: wrap with `new Audio(URL.createObjectURL(...))` or feed to
       *  AudioContext.decodeAudioData. */
      synthesize: (req: TtsSynthesizeRequest) => c.postBlob("/api/v1/tts/synthesize", req),
      voices: () => c.get<TtsVoicesResponse>("/api/v1/tts/voices"),
      status: () => c.get<TtsStatusResponse>("/api/v1/tts/status"),
    },

    llm: {
      autocorrect: async (text: string): Promise<string> =>
        (
          await c.post<AutocorrectResponse>("/api/v1/llm/autocorrect", {
            text,
          } satisfies AutocorrectRequest)
        ).text,
      merge: async (existing: string, next: string): Promise<string> =>
        (
          await c.post<MergeTranscriptionResponse>("/api/v1/llm/merge", {
            existing,
            next,
          } satisfies MergeTranscriptionRequest)
        ).text,
    },

    settings: {
      /** Sparse record of non-default values. */
      load: () => c.get<Record<string, unknown>>("/api/v1/settings"),
      /** PATCH the merged delta, returning the full merged record. A null or
       *  undefined value deletes the key. */
      patch: (partial: Record<string, unknown>) =>
        c.patch<Record<string, unknown>>("/api/v1/settings", partial),
      // Secrets are stored encrypted on disk (secrets.enc, sealed via the OS
      // keychain); the client only ever sees the NAME list, never the values.
      listSecrets: async (): Promise<string[]> =>
        (await c.get<{ names: string[] }>("/api/v1/settings/secrets")).names,
      /** Empty `value` deletes. */
      setSecret: async (name: string, value: string): Promise<void> => {
        if (value === "") await c.del<void>(`/api/v1/settings/secrets/${enc(name)}`);
        else await c.put(`/api/v1/settings/secrets/${enc(name)}`, { value });
      },
      deleteSecret: (name: string) => c.del<void>(`/api/v1/settings/secrets/${enc(name)}`),
    },

    pairing: new PairingApi(c),

    update: {
      /** Fetches and verifies the signed core manifest against the running
       *  core. Cheap, so it is safe on a "Check for Updates" click. */
      check: () => c.get<UpdateCheckResponse>("/api/v1/update/check"),
      /** Stages the binary, hands off to loca-core-updater, and exits the core,
       *  so the request does NOT return cleanly on success: callers swallow
       *  request-aborted errors and watch the `update.staged` WS frame. */
      apply: (version?: string) => c.post<void>("/api/v1/update/apply", { version }),
    },
  };
}

export type CoreApi = ReturnType<typeof buildApi>;

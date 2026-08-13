import type {
  CheckUpdatesResponse,
  DownloadExtensionRequest,
  EmbedRequest,
  EmbedResponse,
  Extension,
  ExtensionActionResponse,
  ExtensionJobResponse,
  Grant,
  ListExtensionsResponse,
  ListExtensionToolsResponse,
  SearchExtensionsResponse,
  SetGrantsResponse,
  ToolFilterRequest,
  ToolFilterResponse,
  ToolSchemasResponse,
  UndeclaredPolicy,
} from "@loca/shared";
import type { CoreClient } from "./client";

export class ExtensionsApi {
  constructor(private readonly client: CoreClient) {}

  list(): Promise<ListExtensionsResponse> {
    return this.client.get("/api/v1/extensions");
  }

  search(
    q: string,
    opts: { limit?: number; offset?: number } = {},
  ): Promise<SearchExtensionsResponse> {
    const params = new URLSearchParams({ q });
    if (opts.limit !== undefined) params.set("limit", String(opts.limit));
    if (opts.offset !== undefined) params.set("offset", String(opts.offset));
    return this.client.get(`/api/v1/extensions/search?${params}`);
  }

  // Phase 1: acquire a extension's files (npm/local/builtin). Returns a streamed
  // job; deps are installed separately via installDeps.
  download(req: DownloadExtensionRequest): Promise<ExtensionJobResponse> {
    return this.client.post("/api/v1/extensions/download", req);
  }

  // Phase 2: install a downloaded extension's dependencies + pin its hash.
  installDeps(id: string): Promise<ExtensionJobResponse> {
    return this.client.post(`/api/v1/extensions/${encodeURIComponent(id)}/install`, {});
  }

  // Install the built-in's tools at the user's request. Installs now if the
  // worker runtime is present, else `{ queued: true }` (runs once it lands).
  installBuiltin(): Promise<{ queued: boolean }> {
    return this.client.post("/api/v1/extensions/builtin-install", {});
  }

  delete(id: string): Promise<void> {
    return this.client.del(`/api/v1/extensions/${encodeURIComponent(id)}`) as Promise<void>;
  }

  // Revert an installed, deps-bearing extension to 'downloaded' (drop its installed
  // deps). The source files stay so it can be re-installed.
  uninstall(id: string): Promise<ExtensionActionResponse> {
    return this.client.post(`/api/v1/extensions/${encodeURIComponent(id)}/uninstall`, {});
  }

  update(id: string, version?: string): Promise<ExtensionJobResponse> {
    return this.client.post(`/api/v1/extensions/${encodeURIComponent(id)}/update`, { version });
  }

  /** Check installed extensions for newer versions. Omit `ids` to check all. */
  checkUpdates(ids?: string[]): Promise<CheckUpdatesResponse> {
    return this.client.post("/api/v1/extensions/check-updates", { ids });
  }

  listTools(id: string): Promise<ListExtensionToolsResponse> {
    return this.client.get(`/api/v1/extensions/${encodeURIComponent(id)}/tools`);
  }

  // Flat list of every tool from every provider (backs the Tools manager).
  listAllTools(): Promise<ListExtensionToolsResponse> {
    return this.client.get("/api/v1/tools");
  }

  // Re-pin the current on-disk content + clear the drift warning.
  confirmReenable(id: string): Promise<ExtensionActionResponse> {
    return this.client.post(`/api/v1/extensions/${encodeURIComponent(id)}/confirm-reenable`, {});
  }

  enableTool(id: string, tool: string): Promise<{ ok: boolean }> {
    return this.client.post(
      `/api/v1/extensions/${encodeURIComponent(id)}/tools/${encodeURIComponent(tool)}/enable`,
      {},
    );
  }

  disableTool(id: string, tool: string): Promise<{ ok: boolean }> {
    return this.client.post(
      `/api/v1/extensions/${encodeURIComponent(id)}/tools/${encodeURIComponent(tool)}/disable`,
      {},
    );
  }

  setToolAlwaysAvailable(id: string, tool: string, value: boolean): Promise<{ ok: boolean }> {
    return this.client.post(
      `/api/v1/extensions/${encodeURIComponent(id)}/tools/${encodeURIComponent(tool)}/always-available`,
      { value },
    );
  }

  setGrants(
    id: string,
    tool: string,
    grants: Array<{ key: string; state: Grant["state"] }>,
  ): Promise<SetGrantsResponse> {
    return this.client.post(
      `/api/v1/extensions/${encodeURIComponent(id)}/tools/${encodeURIComponent(tool)}/grants`,
      { grants },
    );
  }

  setUndeclaredPolicy(id: string, policy: UndeclaredPolicy): Promise<Extension> {
    return this.client.post(`/api/v1/extensions/${encodeURIComponent(id)}/undeclared-policy`, {
      policy,
    });
  }

  reindex(): Promise<{ embedded: number }> {
    return this.client.post("/api/v1/extensions/reindex", {});
  }

  rescan(): Promise<{ added: number; updated: number; removed: number }> {
    return this.client.post("/api/v1/extensions/rescan", {});
  }

  filter(req: ToolFilterRequest): Promise<ToolFilterResponse> {
    return this.client.post("/api/v1/extensions/filter", req);
  }

  toolSchemas(ids: string[]): Promise<ToolSchemasResponse> {
    return this.client.post("/api/v1/extensions/tool-schemas", { ids });
  }

  embed(req: EmbedRequest): Promise<EmbedResponse> {
    return this.client.post("/api/v1/extensions/embed", req);
  }
}

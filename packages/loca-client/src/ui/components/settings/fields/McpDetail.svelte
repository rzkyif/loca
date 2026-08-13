<script lang="ts">
  import { untrack } from "svelte";
  import { errMessage, type McpServer } from "@loca/shared";
  import { confirmState, mcpState } from "$stores";
  import { getLogger } from "$lib/util/log";
  import { createDebouncedSave } from "$lib/util/debounced-save";
  import McpDetailView from "@loca/shared/ui/components/settings/McpDetailView.svelte";

  const log = getLogger("mcp");

  let {
    server,
    horizontal = false,
    reload,
  }: {
    server: McpServer;
    horizontal?: boolean;
    reload: () => void;
  } = $props();

  let draftName = $state(untrack(() => server.name));
  let draftKind = $state<"stdio" | "remote">(untrack(() => server.kind));
  let draftCommand = $state(untrack(() => server.command ?? ""));
  let draftArgs = $state(untrack(() => (server.args ?? []).join(" ")));
  // stdio runtime: "custom" runs the command verbatim, "deno" runs it through
  // the bundled deno binary (so npm-based servers need no Node.js install).
  let draftRuntime = $state<"custom" | "deno">(untrack(() => server.runtime));
  let draftAllowAll = $state(untrack(() => server.denoAllowAll));
  let draftPermissions = $state(untrack(() => (server.denoPermissions ?? []).join(" ")));
  let draftUrl = $state(untrack(() => server.url ?? ""));
  let draftRemoteAuth = $state<"none" | "bearer" | "oauth">(untrack(() => server.remoteAuth));
  // The bearer token is write-only: core never sends it back, so the field
  // starts blank and is persisted only once the user edits it (so an untouched
  // save doesn't wipe an existing token).
  let draftAuthToken = $state("");
  let authTouched = $state(false);

  // Optimistic override for the enable toggle while an enable/disable call is in
  // flight. The knob and status reflect the user's intent immediately (enabling
  // shows "connecting" for the seconds the connect handshake takes) instead of
  // waiting for the PATCH to return with the settled state. Cleared once the call
  // resolves, at which point `server` already carries the real status.
  let pending = $state<boolean | null>(null);

  const effectiveEnabled = $derived(pending ?? server.enabled);
  const effectiveStatus = $derived<McpServer["status"]>(
    pending === true ? "connecting" : server.status,
  );

  // The command actually run for a stdio server, shown in the enable
  // confirmation so consent reflects what launches.
  function effectiveCommand(): string {
    if (draftRuntime === "deno") {
      const perms = draftAllowAll ? "--allow-all" : draftPermissions.trim();
      return `deno run ${perms} ${draftCommand.trim()} ${draftArgs.trim()}`
        .replace(/\s+/g, " ")
        .trim();
    }
    return `${draftCommand.trim()} ${draftArgs.trim()}`.trim();
  }

  const { scheduleSave, flushSave } = createDebouncedSave(async () => {
    try {
      await mcpState.update(server.id, {
        name: draftName.trim() || "Untitled",
        kind: draftKind,
        command: draftKind === "stdio" ? draftCommand.trim() : undefined,
        args: draftArgs.trim() ? draftArgs.trim().split(/\s+/) : [],
        runtime: draftRuntime,
        denoAllowAll: draftAllowAll,
        denoPermissions: draftPermissions.trim() ? draftPermissions.trim().split(/\s+/) : [],
        url: draftKind === "remote" ? draftUrl.trim() : undefined,
        remoteAuth: draftKind === "remote" ? draftRemoteAuth : undefined,
        // Only send the token field once the user has touched it.
        ...(authTouched ? { authToken: draftAuthToken } : {}),
      });
      reload();
    } catch (e) {
      log.error("save MCP server failed:", e);
    }
  }, 600);

  async function applyEnabled(enabled: boolean) {
    pending = enabled;
    try {
      await mcpState.update(server.id, { enabled });
      reload();
    } catch (e) {
      confirmState.alert({ title: "Action failed", message: errMessage(e) });
    } finally {
      pending = null;
    }
  }

  function toggleEnabled(enabled: boolean) {
    // Disabling is harmless; enabling spawns a local subprocess (stdio) or
    // opens a network connection (remote), so confirm first. A stdio server
    // runs an arbitrary command OUTSIDE the tool sandbox, so this is the
    // consent gate for executing it.
    if (!enabled) {
      void applyEnabled(false);
      return;
    }
    const cmd = effectiveCommand();
    const message =
      draftKind === "stdio"
        ? `Enabling this server runs the command "${cmd}" on your machine. It ` +
          `runs OUTSIDE the tool sandbox with your full access. Only enable MCP ` +
          `servers you trust.`
        : `Enabling this server connects to ${draftUrl.trim()} and sends it your requests${
            server.hasAuth ? " with your stored token" : ""
          }. Only enable MCP servers you trust.`;
    confirmState.request({
      title: "Enable MCP server?",
      message,
      confirmLabel: "Enable",
      onConfirm: () => applyEnabled(true),
    });
  }
</script>

<McpDetailView
  enabled={effectiveEnabled}
  status={effectiveStatus}
  statusError={server.statusError}
  remoteAuth={draftRemoteAuth}
  hasAuth={server.hasAuth}
  oauthAuthorized={server.oauthAuthorized}
  {draftName}
  {draftKind}
  {draftCommand}
  {draftArgs}
  {draftRuntime}
  {draftAllowAll}
  {draftPermissions}
  {draftUrl}
  {draftAuthToken}
  {horizontal}
  onToggleEnabled={(v) => toggleEnabled(v)}
  onNameInput={(v) => {
    draftName = v;
    scheduleSave();
  }}
  onKindChange={(v) => {
    draftKind = v;
    scheduleSave();
  }}
  onRuntimeChange={(v) => {
    draftRuntime = v;
    scheduleSave();
  }}
  onCommandInput={(v) => {
    draftCommand = v;
    scheduleSave();
  }}
  onArgsInput={(v) => {
    draftArgs = v;
    scheduleSave();
  }}
  onToggleAllowAll={(v) => {
    draftAllowAll = v;
    scheduleSave();
  }}
  onPermissionsInput={(v) => {
    draftPermissions = v;
    scheduleSave();
  }}
  onUrlInput={(v) => {
    draftUrl = v;
    scheduleSave();
  }}
  onRemoteAuthChange={(v) => {
    draftRemoteAuth = v;
    scheduleSave();
  }}
  onAuthTokenInput={(v) => {
    draftAuthToken = v;
    authTouched = true;
    scheduleSave();
  }}
  onSignIn={async () => {
    try {
      await mcpState.startOAuth(server.id);
    } catch (e) {
      confirmState.alert({ title: "Sign in failed", message: errMessage(e) });
    }
  }}
  onFlush={() => flushSave()}
/>

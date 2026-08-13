<script lang="ts">
  // Presentational body of an MCP server's detail editor: the enable toggle with
  // its status line, the name field, the transport select, and the transport-
  // specific fields (stdio command/args with an optional bundled-deno runtime and
  // its permissions, or a remote URL with a bearer token), plus the live prompts
  // list. The client owns the draft state, the debounced save, the secret-token
  // write-only handling, and the enable-confirmation flow; this View stays pure:
  // it takes the draft field values + status as data and emits on*-change
  // callbacks, with a pre-filtered prompts array toggled via onTogglePrompt.
  import type {
    McpConnectionStatus,
    McpRemoteAuth,
    McpStdioRuntime,
    McpTransportKind,
  } from "../../../domain/mcp.ts";
  import Button from "../primitives/Button.svelte";
  import FormField from "../primitives/FormField.svelte";
  import Input from "../primitives/Input.svelte";
  import Select from "../primitives/Select.svelte";
  import Toggle from "../primitives/Toggle.svelte";
  import IconText from "../primitives/IconText.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  // `horizontal` mirrors the settings-panel layout flag: controls sit to the
  // right of their label when there is room, and stack below when narrow.
  let {
    enabled = false,
    status,
    statusError = undefined,
    remoteAuth = "none",
    hasAuth = false,
    oauthAuthorized = false,
    draftName,
    draftKind,
    draftCommand,
    draftArgs,
    draftRuntime,
    draftAllowAll,
    draftPermissions,
    draftUrl,
    draftAuthToken,
    horizontal = false,
    onToggleEnabled = noop,
    onNameInput = noop,
    onKindChange = noop,
    onRuntimeChange = noop,
    onCommandInput = noop,
    onArgsInput = noop,
    onToggleAllowAll = noop,
    onPermissionsInput = noop,
    onUrlInput = noop,
    onRemoteAuthChange = noop,
    onAuthTokenInput = noop,
    onSignIn = noop,
    onFlush = noop,
  }: {
    enabled?: boolean;
    status: McpConnectionStatus;
    statusError?: string;
    remoteAuth?: McpRemoteAuth;
    hasAuth?: boolean;
    oauthAuthorized?: boolean;
    draftName: string;
    draftKind: McpTransportKind;
    draftCommand: string;
    draftArgs: string;
    draftRuntime: McpStdioRuntime;
    draftAllowAll: boolean;
    draftPermissions: string;
    draftUrl: string;
    draftAuthToken: string;
    horizontal?: boolean;
    onToggleEnabled?: (enabled: boolean) => void;
    onNameInput?: (v: string) => void;
    onKindChange?: (v: McpTransportKind) => void;
    onRuntimeChange?: (v: McpStdioRuntime) => void;
    onCommandInput?: (v: string) => void;
    onArgsInput?: (v: string) => void;
    onToggleAllowAll?: (v: boolean) => void;
    onPermissionsInput?: (v: string) => void;
    onUrlInput?: (v: string) => void;
    onRemoteAuthChange?: (v: McpRemoteAuth) => void;
    onAuthTokenInput?: (v: string) => void;
    onSignIn?: () => void;
    onFlush?: () => void;
  } = $props();

  function noop(): void {}

  // Connection status shown as a read-only two-line block above the toggle (the
  // same shape memories use for summarization status). It reflects the live
  // connection: "Off" until enabled, then "Connecting" while the handshake runs,
  // then "Connected" or "Couldn't connect". The client feeds an optimistic
  // "connecting" the moment the toggle is flipped so the line reacts instantly.
  const connection = $derived.by(() => {
    if (!enabled) {
      return {
        icon: "i-material-symbols-link-off-rounded",
        tone: "text-default-500",
        title: "Off",
        detail: "Enable to connect and use its tools and prompts.",
      };
    }
    switch (status) {
      case "connected":
        return {
          icon: "i-material-symbols-check-circle-rounded",
          tone: "text-accent-green-700",
          title: "Connected",
          detail: "Ready to use its tools and prompts.",
        };
      case "connecting":
        return {
          icon: "i-material-symbols-progress-activity",
          tone: "text-accent-yellow-700",
          title: "Connecting",
          detail: "Starting the server and fetching its capabilities.",
        };
      case "error":
        return {
          icon: "i-material-symbols-error-rounded",
          tone: "text-accent-red-700",
          title: "Couldn't connect",
          detail: "The server failed to start. See the details below.",
        };
      default:
        return {
          icon: "i-material-symbols-info-rounded",
          tone: "text-accent-yellow-700",
          title: "Disconnected",
          detail: "Not connected right now.",
        };
    }
  });

  const KIND_OPTIONS = [
    { value: "stdio", label: "Local (stdio)" },
    { value: "remote", label: "Remote (HTTP/SSE)" },
  ];

  const AUTH_OPTIONS = [
    { value: "none", label: "None" },
    { value: "bearer", label: "Bearer token" },
    { value: "oauth", label: "OAuth 2.1" },
  ];

  const RUNTIME_OPTIONS = [
    { value: "custom", label: "Custom command" },
    { value: "deno", label: "Bundled deno" },
  ];
</script>

<div class="flex flex-col gap-3">
  <div class="flex flex-col gap-0.5 text-xs">
    <IconText icon={connection.icon} color={connection.tone}>{connection.title}</IconText>
    <span class="text-default-500">{connection.detail}</span>
  </div>

  <FormField label="Connect" {horizontal}>
    <Toggle checked={enabled} ariaLabel="Enable server" onchange={(v) => onToggleEnabled(v)} />
  </FormField>

  {#if status === "error" && statusError}
    <ErrorDetailView message="Couldn't connect to this server" detail={statusError} />
  {/if}

  <FormField label="Name">
    <Input
      type="text"
      value={draftName}
      ariaLabel="Server name"
      oninput={(v) => onNameInput(v)}
      onblur={() => onFlush()}
    />
  </FormField>

  <FormField label="Transport">
    <Select
      value={draftKind}
      options={KIND_OPTIONS}
      ariaLabel="Transport"
      onchange={(v) => onKindChange(v as McpTransportKind)}
    />
  </FormField>

  {#if draftKind === "stdio"}
    <FormField label="Runtime">
      <Select
        value={draftRuntime}
        options={RUNTIME_OPTIONS}
        ariaLabel="Runtime"
        onchange={(v) => onRuntimeChange(v as McpStdioRuntime)}
      />
    </FormField>

    {#if draftRuntime === "deno"}
      <FormField label="Package or Script">
        <Input
          type="text"
          value={draftCommand}
          placeholder="npm:@scope/server"
          mono
          ariaLabel="Package or script"
          oninput={(v) => onCommandInput(v)}
          onblur={() => onFlush()}
        />
      </FormField>
      <FormField label="Arguments">
        <Input
          type="text"
          value={draftArgs}
          mono
          ariaLabel="Arguments"
          oninput={(v) => onArgsInput(v)}
          onblur={() => onFlush()}
        />
      </FormField>
      <div class="flex flex-col gap-1.5 pt-2 border-t border-surface">
        <FormField
          label="Full Access"
          description="Runs with full access for the widest compatibility. Turn off to choose permissions yourself."
          descriptionTier="always"
          {horizontal}
        >
          <Toggle
            checked={draftAllowAll}
            ariaLabel="Full access"
            onchange={(v) => onToggleAllowAll(v)}
          />
        </FormField>
        {#if !draftAllowAll}
          <FormField label="Permissions">
            <Input
              type="text"
              value={draftPermissions}
              placeholder="--allow-net --allow-read=/path"
              mono
              ariaLabel="Permissions"
              oninput={(v) => onPermissionsInput(v)}
              onblur={() => onFlush()}
            />
          </FormField>
        {/if}
      </div>
    {:else}
      <FormField label="Command">
        <Input
          type="text"
          value={draftCommand}
          placeholder="npx"
          mono
          ariaLabel="Command"
          oninput={(v) => onCommandInput(v)}
          onblur={() => onFlush()}
        />
      </FormField>
      <FormField label="Arguments">
        <Input
          type="text"
          value={draftArgs}
          placeholder="-y @some/mcp-server"
          mono
          ariaLabel="Arguments"
          oninput={(v) => onArgsInput(v)}
          onblur={() => onFlush()}
        />
      </FormField>
    {/if}
  {:else}
    <FormField label="URL">
      <Input
        type="text"
        value={draftUrl}
        placeholder="https://example.com/mcp"
        mono
        ariaLabel="URL"
        oninput={(v) => onUrlInput(v)}
        onblur={() => onFlush()}
      />
    </FormField>
    <FormField label="Authentication">
      <Select
        value={remoteAuth}
        options={AUTH_OPTIONS}
        ariaLabel="Authentication"
        onchange={(v) => onRemoteAuthChange(v as McpRemoteAuth)}
      />
    </FormField>
    {#if remoteAuth === "bearer"}
      <FormField label="Bearer token">
        <Input
          type="password"
          value={draftAuthToken}
          placeholder={hasAuth ? "(stored - leave blank to keep)" : "optional"}
          mono
          ariaLabel="Bearer token"
          oninput={(v) => onAuthTokenInput(v)}
          onblur={() => onFlush()}
        />
      </FormField>
    {:else if remoteAuth === "oauth"}
      <FormField
        label="Sign in"
        description={oauthAuthorized
          ? "Signed in. Sign in again only if access is revoked."
          : "Authorize in your browser to connect."}
        descriptionTier="always"
        {horizontal}
      >
        <Button
          variant={oauthAuthorized ? "secondary" : "primary"}
          size="sm"
          icon="i-material-symbols-login-rounded"
          onclick={() => onSignIn()}
        >
          {oauthAuthorized ? "Sign in again" : "Sign in"}
        </Button>
      </FormField>
    {/if}
  {/if}
</div>

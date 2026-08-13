<script lang="ts">
  import { errMessage, type Grant, type GrantState, permissionKey, type Tool } from "@loca/shared";
  import { confirmState, settingsState, extensionsState, mcpState } from "$stores";
  import ToolDetailView from "@loca/shared/ui/components/settings/ToolDetailView.svelte";

  // One tool's enable toggle and permission grants. Provider-agnostic: extension
  // tools carry sandbox permissions managed here; MCP tools run on their server
  // and only carry an enable toggle.
  // `reload` refreshes the list behind the detail so a card's "Enabled" chip
  // (and other badges) reflect a toggle made here, not just in the detail pane.
  let {
    tool,
    horizontal = false,
    reload,
  }: { tool: Tool; horizontal?: boolean; reload?: () => void } = $props();

  let busyId = $state<string | null>(null);

  // Extension tools toggle through the extensions API (with grant refresh); MCP
  // tools toggle on their server, then refresh the flat tool list.
  async function setEnabled(enabled: boolean): Promise<void> {
    if (tool.providerKind === "mcp") {
      await mcpState.setToolEnabled(tool.extensionId, tool.name, enabled);
      await extensionsState.loadAllTools();
    } else if (enabled) {
      await extensionsState.enableTool(tool.extensionId, tool.name);
    } else {
      await extensionsState.disableTool(tool.extensionId, tool.name);
    }
  }

  // Always-available applies to both providers. MCP tools default to it (so
  // they're usually offered); turning it off folds the tool into the relevance
  // filter (matched by name + description). It's hidden when the global "allow
  // always-available tools" setting is off (that setting then acts as the master
  // switch, so the per-tool override is disabled).
  const showAlwaysAvailable = $derived(
    settingsState.currentSettings["tools.alwaysAvailableEnabled"] !== false,
  );

  // Extension tools store the flag on their DB row; MCP tools store it on the
  // server row, then the flat tool list is refreshed to reflect the new value.
  async function setAlwaysAvailable(alwaysAvailable: boolean): Promise<void> {
    if (tool.providerKind === "mcp") {
      await mcpState.setToolAlwaysAvailable(tool.extensionId, tool.name, alwaysAvailable);
      await extensionsState.loadAllTools();
    } else {
      await extensionsState.setToolAlwaysAvailable(tool.extensionId, tool.name, alwaysAvailable);
    }
  }

  async function runToolAction(id: string, fn: () => Promise<void>) {
    busyId = id;
    try {
      await fn();
      // The mutation already refreshed the flat tool list; re-render the list
      // behind the detail so its card badges pick up the change.
      reload?.();
    } catch (e) {
      confirmState.alert({ title: "Action failed", message: errMessage(e) });
    } finally {
      busyId = null;
    }
  }

  function grantStateFor(t: Tool, key: string): GrantState {
    return t.grants.find((g) => g.permissionKey === key)?.state ?? "ask";
  }

  function riskyWarning(decl: Tool["requiredPermissions"][number]): string | null {
    switch (decl.kind) {
      case "run":
        return `The tool will be able to run the ${decl.binary} program at any time without asking you. Programs it runs are not sandboxed and can do anything you can do on this computer.`;
      case "ffi":
        return "The tool will be able to load native libraries at any time without asking you. Native code runs outside the sandbox and can do anything you can do on this computer.";
      case "read":
        return `The tool will be able to read files under ${decl.path} at any time without asking you, including sending their contents anywhere it can reach.`;
      case "write":
        return `The tool will be able to change or delete files under ${decl.path} at any time without asking you.`;
      case "net":
        return decl.host === "*"
          ? "The tool will be able to connect to any server on the internet at any time without asking you, including sending data it has access to."
          : null;
      default:
        return null;
    }
  }

  async function applyGrantChange(permKey: string, nextState: GrantState) {
    const merged: Array<{ key: string; state: Grant["state"] }> = [];
    for (const g of tool.grants) {
      if (g.permissionKey !== permKey) merged.push({ key: g.permissionKey, state: g.state });
    }
    merged.push({ key: permKey, state: nextState });
    await runToolAction(`${tool.name}::${permKey}`, () =>
      extensionsState.setGrants(tool.extensionId, tool.name, merged),
    );
  }

  function handleGrantChange(
    decl: Tool["requiredPermissions"][number],
    permKey: string,
    nextState: GrantState,
  ) {
    if (busyId !== null) return;
    const warning = nextState === "granted" ? riskyWarning(decl) : null;
    const suppressed = settingsState.currentSettings["extensions.skipRiskyGrantWarning"] === true;
    if (!warning || suppressed) {
      void applyGrantChange(permKey, nextState);
      return;
    }
    confirmState.request({
      title: "Always allow this permission?",
      message: warning,
      confirmLabel: "Always Allow",
      destructive: true,
      dontShowAgainLabel: "Do not warn me about risky permissions again",
      onConfirm: (dontShowAgain) => {
        if (dontShowAgain) {
          void settingsState.updateSetting("extensions.skipRiskyGrantWarning", true);
        }
        void applyGrantChange(permKey, nextState);
      },
    });
  }

  function sortedPermissions(t: Tool): Tool["requiredPermissions"] {
    return [...t.requiredPermissions].sort(
      (a, b) => Number(a.optional ?? false) - Number(b.optional ?? false),
    );
  }

  function permissionParts(decl: Tool["requiredPermissions"][number]): {
    before: string;
    code?: string;
    after: string;
  } {
    switch (decl.kind) {
      case "net":
        return {
          before: "Network access to ",
          code: `${decl.host}:${decl.ports.map(String).join(",")}`,
          after: "",
        };
      case "read":
        return { before: "Read files at ", code: decl.path, after: "" };
      case "write":
        return { before: "Write files at ", code: decl.path, after: "" };
      case "run":
        return { before: "Run the ", code: decl.binary, after: " command" };
      case "env":
        return { before: "Read the ", code: decl.key, after: " environment variable" };
      case "ffi":
        return { before: "Load native libraries (FFI)", after: "" };
      case "sys":
        return { before: "Read system info (", code: decl.flag, after: ")" };
      case "memories":
        return decl.access === "write"
          ? { before: "Create and edit your memories", after: "" }
          : { before: "Read your memories", after: "" };
      case "llm":
        return { before: "Generate text with the model", after: "" };
      case "tts":
        return { before: "Speak text aloud", after: "" };
      case "stt":
        return { before: "Transcribe audio to text", after: "" };
    }
  }

  const deniedRequired = $derived(
    tool.requiredPermissions.filter(
      (d) => !d.optional && grantStateFor(tool, permissionKey(d)) === "denied",
    ).length,
  );

  // Pre-format every permission row for the View: labels, the inline code chip,
  // the "(required)" marker, the current grant state, and the aria label. The
  // declaration is captured in a closure so the grant handler still sees the
  // typed decl (for its risk warning).
  const permissionRows = $derived(
    sortedPermissions(tool).map((decl) => {
      const key = permissionKey(decl);
      const parts = permissionParts(decl);
      return {
        key,
        before: parts.before,
        code: parts.code,
        after: parts.after,
        required: !decl.optional,
        reason: decl.reason,
        grantState: grantStateFor(tool, key),
        ariaLabel: `${parts.before}${parts.code ?? ""}${parts.after}`,
        decl,
      };
    }),
  );
</script>

<ToolDetailView
  enabled={tool.enabled}
  {horizontal}
  enableBusy={busyId === tool.name}
  enableAriaLabel={`Enable ${tool.name}`}
  {showAlwaysAvailable}
  alwaysAvailable={tool.alwaysAvailable}
  alwaysAvailableBusy={busyId === `${tool.name}::always-available`}
  alwaysAvailableAriaLabel={`Always offer ${tool.name}`}
  {deniedRequired}
  permissions={permissionRows}
  onToggleEnabled={(v) => runToolAction(tool.name, () => setEnabled(v))}
  onToggleAlwaysAvailable={(v) =>
    runToolAction(`${tool.name}::always-available`, () => setAlwaysAvailable(v))}
  onGrantChange={(key, nextState) => {
    const row = permissionRows.find((r) => r.key === key);
    if (row) handleGrantChange(row.decl, key, nextState);
  }}
/>

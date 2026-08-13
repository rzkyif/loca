<script lang="ts">
  // Presentational body of one tool's detail pane: the enable toggle (with the
  // "denied required permission" caveat) and the per-permission grant rows. All
  // values arrive pre-formatted (the client owns the tool, the busy state, the
  // grant lookups, and computes every permission label/aria string), so this
  // stays pure: props in, callbacks out. Provider-agnostic: a tool with no
  // declared permissions shows the empty note instead of the grant list.
  import Toggle from "../primitives/Toggle.svelte";
  import FormField from "../primitives/FormField.svelte";
  import SubsectionHeader from "../primitives/SubsectionHeader.svelte";
  import type { GrantState } from "../../../domain/extension.ts";

  // One permission row, fully pre-formatted by the client. `before`/`code`/
  // `after` are the three label spans (`code` renders in the inline code chip);
  // `required` toggles the "(required)" marker; `grantState` selects the toggle
  // cell; `ariaLabel` labels the grant toggle.
  interface PermissionRow {
    key: string;
    before: string;
    code?: string;
    after: string;
    required: boolean;
    reason: string;
    grantState: GrantState;
    ariaLabel: string;
  }

  let {
    enabled,
    horizontal = false,
    enableBusy = false,
    enableAriaLabel,
    showAlwaysAvailable = false,
    alwaysAvailable = false,
    alwaysAvailableBusy = false,
    alwaysAvailableAriaLabel = "Always available",
    deniedRequired = 0,
    permissions = [],
    onToggleEnabled,
    onToggleAlwaysAvailable,
    onGrantChange,
  }: {
    enabled: boolean;
    horizontal?: boolean;
    enableBusy?: boolean;
    enableAriaLabel: string;
    // The always-available row is shown only when the global "allow
    // always-available tools" setting is on; the client hides it otherwise
    // (and for MCP tools, which are always offered regardless).
    showAlwaysAvailable?: boolean;
    alwaysAvailable?: boolean;
    alwaysAvailableBusy?: boolean;
    alwaysAvailableAriaLabel?: string;
    deniedRequired?: number;
    permissions?: PermissionRow[];
    onToggleEnabled?: (enabled: boolean) => void;
    onToggleAlwaysAvailable?: (alwaysAvailable: boolean) => void;
    onGrantChange?: (key: string, nextState: GrantState) => void;
  } = $props();

  const noop = (): void => {};

  const GRANT_OPTIONS = [
    { value: "denied", label: "Deny" },
    { value: "ask", label: "Ask" },
    { value: "granted", label: "Allow" },
  ];

  const codeClass =
    "font-mono bg-surface-inset text-default-800 rounded-small px-1.5 py-0.5 break-all";
</script>

<div class="flex flex-col gap-3">
  <FormField
    label="Enabled"
    description={enabled && deniedRequired > 0
      ? "Enabled, but not offered to the agent while a required permission is denied."
      : undefined}
    descriptionTier="always"
    {horizontal}
  >
    <Toggle
      checked={enabled}
      disabled={enableBusy}
      ariaLabel={enableAriaLabel}
      onchange={(v) => (onToggleEnabled ?? noop)(v)}
    />
  </FormField>

  {#if showAlwaysAvailable}
    <FormField
      label="Always Available"
      description="Offer this tool on every message instead of only when it looks relevant."
      descriptionTier="ondemand"
      {horizontal}
    >
      <Toggle
        checked={alwaysAvailable}
        disabled={alwaysAvailableBusy}
        ariaLabel={alwaysAvailableAriaLabel}
        onchange={(v) => (onToggleAlwaysAvailable ?? noop)(v)}
      />
    </FormField>
  {/if}

  {#if permissions.length > 0}
    <div class="flex flex-col gap-1.5">
      <SubsectionHeader label="Permissions" />
      {#each permissions as perm (perm.key)}
        <FormField label={perm.ariaLabel} {horizontal}>
          {#snippet labelContent()}
            <div class="flex flex-col gap-0.5 min-w-0">
              <span class="text-xs text-default-800 break-words">
                {perm.before}{#if perm.code}<code class={codeClass}>{perm.code}</code
                  >{/if}{perm.after}{#if perm.required}<span class="text-default-500 ml-1.5"
                    >(required)</span
                  >{/if}
              </span>
              <span class="text-xs text-default-600 break-words">{perm.reason}</span>
            </div>
          {/snippet}
          <Toggle
            value={perm.grantState}
            options={GRANT_OPTIONS}
            ariaLabel={perm.ariaLabel}
            onselect={(v) => (onGrantChange ?? noop)(perm.key, v as GrantState)}
          />
        </FormField>
      {/each}
    </div>
  {:else}
    <span class="text-xs text-default-600 italic"> This tool needs no special permissions. </span>
  {/if}
</div>

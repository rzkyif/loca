<script lang="ts">
  // Read-only detail for an MCP server's prompt, shown in the Snippets manager
  // alongside user snippets: the server owns the prompt body, so the only
  // editable control is whether it appears in the "/" autocomplete. The rest
  // (server, description, arguments) is informational. Enabling flows straight
  // to the core registry via mcpState; `reload` refreshes the list card.
  import type { McpPrompt } from "@loca/shared";
  import { mcpState } from "$stores";
  import FormField from "@loca/shared/ui/components/primitives/FormField.svelte";
  import Toggle from "@loca/shared/ui/components/primitives/Toggle.svelte";

  let { prompt, reload }: { prompt: McpPrompt; reload: () => void } = $props();

  async function toggle(enabled: boolean) {
    await mcpState.setPromptEnabled(prompt.serverId, prompt.name, enabled);
    reload();
  }
</script>

<div class="flex flex-col gap-3">
  <FormField
    label="Enabled"
    description="Show this prompt in the / menu."
    descriptionTier="always"
    horizontal
  >
    <Toggle
      checked={prompt.enabled}
      ariaLabel={`Enable /${prompt.name}`}
      onchange={(v) => void toggle(v)}
    />
  </FormField>

  <div class="text-xs text-default-500">
    Provided by {prompt.serverName} (read-only). Trigger with
    <span class="font-mono">/{prompt.name}</span> in chat.
  </div>

  {#if prompt.description}
    <FormField label="Description">
      <div class="text-sm text-default-800">{prompt.description}</div>
    </FormField>
  {/if}

  {#if prompt.arguments.length > 0}
    <FormField label="Arguments">
      <div class="flex flex-col gap-1">
        {#each prompt.arguments as arg (arg.name)}
          <div class="flex flex-col">
            <span class="font-mono">
              {arg.name}{arg.required ? " (required)" : ""}
            </span>
            {#if arg.description}
              <span class="text-xs text-default-500">{arg.description}</span>
            {/if}
          </div>
        {/each}
      </div>
    </FormField>
  {/if}
</div>

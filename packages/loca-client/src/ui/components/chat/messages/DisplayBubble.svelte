<script lang="ts">
  // Bubble for `role: "display"` messages: content a tool pushed into the chat
  // via ctx.display.* or show_memory. Wraps the shared ExpandableMessageView
  // (expanded by default, since the tool explicitly asked to show this) and
  // injects the rich body (markdown / image / table / diff), which needs the
  // client markdown + diff renderers.
  import type { DisplayContent } from "@loca/shared";
  import ExpandableMessageView from "@loca/shared/ui/components/chat/messages/ExpandableMessageView.svelte";
  import MessageMarkdown from "./MessageMarkdown.svelte";
  import DiffView from "@loca/shared/ui/components/chat/messages/DiffView.svelte";

  let {
    id,
    content,
    neighborLeft = false,
    neighborRight = false,
  }: {
    id?: string;
    content: DisplayContent;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  } = $props();

  const bubbleTitle = $derived.by(() => {
    switch (content.type) {
      case "markdown":
        return "Content";
      case "image":
        return content.alt || "Image";
      case "table":
        return "Table";
      case "diff":
        return content.title || "Changes";
    }
  });
</script>

<ExpandableMessageView {id} title={bubbleTitle} defaultExpanded {neighborLeft} {neighborRight}>
  {#snippet body()}
    {#if content.type === "markdown"}
      <div class="bg-surface-inset text-default-800 text-sm px-4 py-2 rounded-large">
        <MessageMarkdown content={content.markdown} />
      </div>
    {:else if content.type === "image"}
      <img
        src={`data:${content.mime};base64,${content.dataB64}`}
        alt={content.alt ?? "Tool-provided image"}
        class="max-w-full rounded-large"
      />
    {:else if content.type === "table"}
      <div class="bg-surface-inset rounded-large px-2 py-2 overflow-x-auto">
        <table class="text-xs text-default-800 w-full">
          <thead>
            <tr>
              {#each content.columns as col}
                <th class="text-left font-semibold px-2 py-1">{col}</th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each content.rows as row}
              <tr>
                {#each row as cell}
                  <td class="px-2 py-1 align-top">{cell}</td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else if content.type === "diff"}
      <DiffView before={content.before} after={content.after} />
    {/if}
  {/snippet}
</ExpandableMessageView>

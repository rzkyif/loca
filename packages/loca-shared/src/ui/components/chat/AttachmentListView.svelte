<script lang="ts">
  import type { MessagePart } from "../../../domain/session.ts";
  import IconButton from "../primitives/IconButton.svelte";

  // Presentational attachment row (image thumbnails + document chips), shared so
  // the client and website render attachments identically. `image_file` parts
  // are loaded lazily by the client; the resolved data URLs are passed in via
  // `imageUrls` (keyed by part.path) so this View stays free of platform I/O.
  let {
    parts,
    editable = false,
    onRemove,
    onImageClick,
    imageUrls = {},
  }: {
    parts: MessagePart[];
    editable?: boolean;
    onRemove?: (index: number) => void;
    onImageClick?: (url: string) => void;
    imageUrls?: Record<string, string>;
  } = $props();
</script>

{#if parts.length > 0}
  <div class="flex flex-wrap gap-2">
    {#each parts as part, i (("path" in part && part.path) || ("filename" in part && part.filename) || i)}
      {#if part.type === "image_url"}
        <div
          class="relative group bg-surface-inset p-2 rounded-medium {editable && onRemove
            ? 'pr-7'
            : ''}"
        >
          {#if onImageClick}
            <button
              class="block p-0 border-0 bg-transparent hov:cursor-pointer"
              onclick={() => onImageClick(part.image_url.url)}
              title="View Full Image"
            >
              <img src={part.image_url.url} alt="Attached" class="h-16 w-16 object-cover" />
            </button>
          {:else}
            <img src={part.image_url.url} alt="Attached" class="h-16 w-16 object-cover" />
          {/if}
          {#if editable && onRemove}
            <IconButton
              icon="i-material-symbols-close-rounded"
              title="Remove Attachment"
              size="xs"
              variant="subtle"
              surface="circle"
              class="absolute top-1 right-1 bg-transparent"
              onclick={() => onRemove(i)}
            />
          {/if}
        </div>
      {:else if part.type === "image_file"}
        {@const url = imageUrls[part.path] || ""}
        <div
          class="relative group bg-surface-inset p-2 rounded-medium {editable && onRemove
            ? 'pr-7'
            : ''}"
        >
          {#if url}
            {#if onImageClick}
              <button
                class="block p-0 border-0 bg-transparent hov:cursor-pointer"
                onclick={() => onImageClick(url)}
                title="View Full Image"
              >
                <img src={url} alt="Attached" class="h-16 w-16 object-cover" />
              </button>
            {:else}
              <img src={url} alt="Attached" class="h-16 w-16 object-cover" />
            {/if}
          {:else}
            <div class="h-16 w-16 flex items-center justify-center text-default-400">
              <i class="flex i-material-symbols-image-outline-rounded text-2xl"></i>
            </div>
          {/if}
          {#if editable && onRemove}
            <IconButton
              icon="i-material-symbols-close-rounded"
              title="Remove Attachment"
              size="xs"
              variant="subtle"
              surface="circle"
              class="absolute top-1 right-1 bg-transparent"
              onclick={() => onRemove(i)}
            />
          {/if}
        </div>
      {:else if part.type === "document" || part.type === "document_file"}
        <div
          class="relative group flex items-center gap-1 bg-surface-inset px-3 py-1.5 rounded-medium text-sm text-default-600 h-fit"
        >
          <i class="flex i-material-symbols-description-outline-rounded text-base"></i>
          <span class="max-w-32 truncate">{part.filename}</span>
          {#if editable && onRemove}
            <IconButton
              icon="i-material-symbols-close-rounded"
              title="Remove Attachment"
              size="xs"
              variant="subtle"
              class="ml-1"
              onclick={() => onRemove(i)}
            />
          {/if}
        </div>
      {/if}
    {/each}
  </div>
{/if}

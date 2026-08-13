<script lang="ts">
  import type { MessagePart } from "$lib/util/types";
  import { readSessionAttachment } from "$lib/chat/attachments";
  import { getLogger } from "$lib/util/log";
  import AttachmentListView from "@loca/shared/ui/components/chat/AttachmentListView.svelte";

  const log = getLogger("attachments");

  let {
    parts,
    editable = false,
    onRemove,
    onImageClick,
  } = $props<{
    parts: MessagePart[];
    editable?: boolean;
    onRemove?: (index: number) => void;
    onImageClick?: (url: string) => void;
  }>();

  // Cache: attachment path -> data URL (resolved lazily for image_file parts)
  let imageUrlCache = $state<Record<string, string>>({});

  async function ensureImageUrl(path: string, mime: string): Promise<string> {
    if (imageUrlCache[path]) return imageUrlCache[path];
    try {
      const b64 = await readSessionAttachment(path);
      const url = `data:${mime};base64,${b64}`;
      imageUrlCache = { ...imageUrlCache, [path]: url };
      return url;
    } catch (e) {
      log.warn(`failed to read image: ${path}`, e);
      return "";
    }
  }

  // Kick off loads for any image_file parts we don't yet have
  $effect(() => {
    for (const p of parts) {
      if (p.type === "image_file" && !imageUrlCache[p.path]) {
        void ensureImageUrl(p.path, p.mime);
      }
    }
  });
</script>

<AttachmentListView {parts} {editable} {onRemove} {onImageClick} imageUrls={imageUrlCache} />

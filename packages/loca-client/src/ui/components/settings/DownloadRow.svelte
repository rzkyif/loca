<script lang="ts">
  import type { DownloadItem } from "$lib/util/types";
  import { SETTINGS_SCHEMA } from "@loca/shared";
  import { downloadsState } from "../../state";
  import { formatBytes } from "$lib/util/format";
  import { useBlink } from "$composables/use-blink.svelte";
  import DownloadRowView from "@loca/shared/ui/components/settings/DownloadRowView.svelte";

  let { item } = $props<{ item: DownloadItem }>();

  // The row icon is borrowed from the settings group that initiated the
  // download (e.g. the LLM brain icon for an llm.modelPath fetch). Falls
  // back to a generic download icon when the group_id is unknown.
  const groupIcon = $derived(
    SETTINGS_SCHEMA.find((g) => g.id === item.groupId)?.icon ??
      "i-material-symbols-download-rounded",
  );

  // Newly-finished rows draw attention with the shared "attention ping": a text
  // brightness pulse driven by a 500ms blink (matching the transition duration)
  // instead of a colored row fill. The flash clears itself after a short window
  // (downloadsState.openModal), so the pulse fades back to the resting tone.
  const isFlashing = $derived(downloadsState.flashingIds.has(item.id));
  const blink = useBlink();
  $effect(() => blink.run(isFlashing));

  const progressPct = $derived(() => {
    if (item.sizeBytes && item.sizeBytes > 0) {
      return Math.min(100, (item.downloadedBytes / item.sizeBytes) * 100);
    }
    return 0;
  });

  const sizeText = $derived(() => {
    if (item.sizeBytes != null && item.status === "Downloading") {
      return `${formatBytes(item.downloadedBytes)} / ${formatBytes(item.sizeBytes)}`;
    }
    if (item.sizeBytes != null) return formatBytes(item.sizeBytes);
    return "";
  });
</script>

<DownloadRowView
  status={item.status}
  icon={groupIcon}
  filename={item.filename}
  title={item.error ?? `${item.filename} (${item.relPath})`}
  sizeText={sizeText() || undefined}
  progress={progressPct()}
  flashing={isFlashing}
  blinkOn={blink.on}
  showReveal={downloadsState.localCore}
  onCancel={() => downloadsState.cancel(item.id)}
  onRemove={() => downloadsState.remove(item.id)}
  onRetry={() => downloadsState.retry(item.id)}
  onReveal={() => downloadsState.reveal(item.absPath)}
/>

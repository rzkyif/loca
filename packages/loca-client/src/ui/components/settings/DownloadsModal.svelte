<script lang="ts">
  import type { DownloadItem } from "$lib/util/types";
  import { confirmState, downloadsState } from "../../state";
  import { SETTINGS_SCHEMA } from "@loca/shared";
  import { formatBytes } from "$lib/util/format";
  import { useBlink } from "$composables/use-blink.svelte";
  import DownloadsModalView from "@loca/shared/ui/components/settings/DownloadsModalView.svelte";

  // Order: completed first (most recent at top), then active, then queued,
  // then cancelled, then errored. Within each bucket, oldest-added first.
  const sortedItems = $derived(() => {
    const order: Record<string, number> = {
      Completed: 0,
      Downloading: 1,
      Pending: 2,
      Cancelled: 3,
      Error: 4,
    };
    return [...downloadsState.items].sort((a, b) => {
      const oa = order[a.status] ?? 99;
      const ob = order[b.status] ?? 99;
      if (oa !== ob) return oa - ob;
      return a.addedAtMs - b.addedAtMs;
    });
  });

  const hasCompleted = $derived(downloadsState.completed.length > 0);

  // The "attention ping" pulse for newly-finished rows. A single blink drives
  // every flashing row at once (they share the 500ms phase), so the
  // per-row `blinkOn` is just the shared toggle gated by that row's flashing
  // flag. The pulse runs whenever any row is flashing.
  const anyFlashing = $derived(downloadsState.flashingIds.size > 0);
  const blink = useBlink();
  $effect(() => blink.run(anyFlashing));

  // The row icon is borrowed from the settings group that initiated the
  // download (e.g. the LLM brain icon for an llm.modelPath fetch). Falls
  // back to a generic download icon when the group_id is unknown.
  function groupIcon(item: DownloadItem): string {
    return (
      SETTINGS_SCHEMA.find((g) => g.id === item.groupId)?.icon ??
      "i-material-symbols-download-rounded"
    );
  }

  function progressPct(item: DownloadItem): number {
    if (item.sizeBytes && item.sizeBytes > 0) {
      return Math.min(100, (item.downloadedBytes / item.sizeBytes) * 100);
    }
    return 0;
  }

  function sizeText(item: DownloadItem): string | undefined {
    if (item.sizeBytes != null && item.status === "Downloading") {
      return `${formatBytes(item.downloadedBytes)} / ${formatBytes(item.sizeBytes)}`;
    }
    if (item.sizeBytes != null) return formatBytes(item.sizeBytes);
    return undefined;
  }

  const rows = $derived(
    sortedItems().map((item) => ({
      key: item.id,
      status: item.status,
      icon: groupIcon(item),
      filename: item.filename,
      title: item.error ?? `${item.filename} (${item.relPath})`,
      sizeText: sizeText(item),
      progress: progressPct(item),
      flashing: downloadsState.flashingIds.has(item.id),
      blinkOn: blink.on,
      showReveal: downloadsState.localCore,
      onCancel: () => downloadsState.cancel(item.id),
      onRemove: () => downloadsState.remove(item.id),
      onRetry: () => downloadsState.retry(item.id),
      onReveal: () => downloadsState.reveal(item.absPath),
    })),
  );
</script>

<DownloadsModalView
  open={downloadsState.modalOpen}
  items={rows}
  {hasCompleted}
  dismissOnEsc={!confirmState.pending}
  onClearCompleted={() => downloadsState.clearCompleted()}
  onClose={() => downloadsState.closeModal()}
/>

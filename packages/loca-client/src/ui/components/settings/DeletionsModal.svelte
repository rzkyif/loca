<script lang="ts">
  import { deletionsState } from "../../state";
  import { formatBytes } from "$lib/util/format";
  import DeletionsModalView from "@loca/shared/ui/components/settings/DeletionsModalView.svelte";
</script>

{#if deletionsState.pending}
  {@const p = deletionsState.pending}
  {@const total = p.items.reduce((s, i) => s + i.sizeBytes, 0)}
  <DeletionsModalView
    title={p.title}
    notice={p.notice}
    items={p.items.map((i) => ({ label: i.label, sizeText: formatBytes(i.sizeBytes) }))}
    skipped={p.skipped}
    totalText={total > 0 ? `Frees ${formatBytes(total)}` : undefined}
    confirmLabel={p.confirmLabel ?? "Delete"}
    confirmDisabled={p.items.length === 0 && !p.notice}
    onConfirm={() => deletionsState.confirm()}
    onCancel={() => deletionsState.cancel()}
    onClose={() => deletionsState.cancel()}
  />
{/if}

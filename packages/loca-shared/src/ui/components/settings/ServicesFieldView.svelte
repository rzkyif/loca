<script lang="ts">
  // Presentational metrics table for the services field: a column header, one row
  // per running service (name + optional status line + CPU/RAM), and a totals
  // row. Pure/presentational: the client polls sysinfo, formats every CPU/RAM
  // value, derives the rows, and injects a retry callback; this View only renders
  // the table. The retry control on an errored row calls `onRetry` with the row's
  // opaque `retryKind`, which the client maps back to a supervised sidecar.

  type Row = {
    label: string;
    sub?: string;
    cpuText: string;
    ramText: string;
    // Set on an errored, retryable row. `retryKind` is opaque to this View.
    retryKind?: string;
    retryLabel?: string;
    retryDisabled?: boolean;
  };

  const noop = (): void => {};

  let {
    rows,
    totalCpuText,
    totalRamText,
    cpuWidthClass = "w-12",
    ramWidthClass = "w-16",
    onRetry = noop,
  }: {
    rows: Row[];
    totalCpuText: string;
    totalRamText: string;
    /** Width utility for the CPU column (wider in horizontal layouts). */
    cpuWidthClass?: string;
    /** Width utility for the RAM column (wider in horizontal layouts). */
    ramWidthClass?: string;
    onRetry?: (retryKind: string) => void;
  } = $props();
</script>

<!-- Indented (pl-5) to match the Storage field's items, so the table sits
     apart from the field label. -->
<div class="flex flex-col gap-2 pb-1 pl-5">
  <!-- Column header -->
  <div
    class="flex items-baseline gap-3 text-default-400 text-[10px] uppercase tracking-wider select-none"
  >
    <div class="flex-1 min-w-0">Service Name</div>
    <div class="flex items-center gap-2 shrink-0">
      <div class="text-right {cpuWidthClass}">CPU</div>
      <div class="text-right {ramWidthClass}">RAM</div>
    </div>
  </div>

  {#each rows as row}
    <div class="flex items-baseline gap-3">
      <div class="flex flex-col flex-1 min-w-0">
        <div class="text-default-800 text-sm truncate">{row.label}</div>
        {#if row.sub}
          <div class="flex items-center gap-2">
            <div class="text-default-500 text-xs truncate">{row.sub}</div>
            {#if row.retryKind}
              <button
                type="button"
                class="shrink-0 text-xs text-accent-blue-400 hov:text-accent-blue-300 act:text-accent-blue-200 transition-interactive disabled:opacity-50"
                disabled={row.retryDisabled}
                onclick={() => onRetry(row.retryKind!)}
              >
                {row.retryLabel}
              </button>
            {/if}
          </div>
        {/if}
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <div class="text-default-500 text-xs tabular-nums text-right {cpuWidthClass}">
          {row.cpuText}
        </div>
        <div class="text-default-500 text-xs tabular-nums text-right {ramWidthClass}">
          {row.ramText}
        </div>
      </div>
    </div>
  {/each}

  <div class="flex items-baseline gap-3">
    <div class="text-default-800 text-sm flex-1 min-w-0 truncate">Total</div>
    <div class="flex items-center gap-2 shrink-0">
      <div class="text-default-500 text-xs font-bold tabular-nums text-right {cpuWidthClass}">
        {totalCpuText}
      </div>
      <div class="text-default-500 text-xs font-bold tabular-nums text-right {ramWidthClass}">
        {totalRamText}
      </div>
    </div>
  </div>
</div>

<script lang="ts">
  import type { AskUserTableQuestion } from "../../../../../domain/session.ts";
  import type { DraftAnswer } from "../../userinput/AskUserFormView.svelte";

  let {
    q,
    qi,
    draft,
    unselectedClasses,
    setCell,
    addRow,
    removeRow,
  }: {
    q: AskUserTableQuestion;
    qi: number;
    draft: DraftAnswer | undefined;
    unselectedClasses: string;
    setCell: (idx: number, row: number, col: number, value: string) => void;
    addRow: (idx: number, columns: number) => void;
    removeRow: (idx: number, row: number) => void;
  } = $props();
</script>

<!-- Cell padding, not `border-spacing`, carries the row/column gaps: it keeps the
     first column flush with the question above and the Add Row button below,
     and the trailing `pb-1` makes the gap under the last row match the row gap. -->
<div class="flex flex-col">
  <div class="overflow-x-auto">
    <table class="text-xs">
      <thead>
        <tr>
          {#each q.columns as col (col)}
            <th class="text-left font-semibold text-default-600 pl-2 pr-1 pb-1">
              {col}
            </th>
          {/each}
          <th class="pb-1"></th>
        </tr>
      </thead>
      <tbody>
        {#each draft?.rows ?? [] as row, ri (ri)}
          <tr>
            {#each q.columns as _col, ci (ci)}
              <td class="pr-1 pb-1">
                <input
                  type="text"
                  data-tc-nav
                  class="bg-surface-inset text-default-800 rounded block w-full min-w-20 h-7 px-2 outline-none text-xs"
                  value={row[ci] ?? ""}
                  oninput={(e) => setCell(qi, ri, ci, (e.target as HTMLInputElement).value)}
                />
              </td>
            {/each}
            <td class="pb-1">
              <button
                type="button"
                data-tc-nav
                data-tc-aux
                class="flex items-center justify-center h-7 w-7 rounded cursor-pointer outline-none transition-colors duration-100 {unselectedClasses}"
                title="Remove row"
                onclick={() => removeRow(qi, ri)}
              >
                <i class="i-material-symbols-close-rounded"></i>
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  <button
    type="button"
    data-tc-nav
    data-tc-aux
    class="flex items-center justify-center gap-1 w-full text-xs px-2 py-1 h-7 rounded cursor-pointer outline-none transition-colors duration-100 {unselectedClasses}"
    onclick={() => addRow(qi, q.columns.length)}
  >
    <i class="i-material-symbols-add-rounded"></i>
    Add Row
  </button>
</div>

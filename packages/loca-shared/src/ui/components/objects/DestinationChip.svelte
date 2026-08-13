<script lang="ts">
  import { destinationLabel, type SettingDestination } from "../../../domain/settings/types.ts";

  // A small uppercase chip telling the user whether a setting is shared by every
  // client of this core or specific to this client. Used on group headers (one
  // per visible destination) and on section headers in hybrid groups. The two
  // client storage locations (local file, per-client core overlay) both show as
  // "Client"; only the shared core store shows as "Core".
  let { dest }: { dest: SettingDestination } = $props();
</script>

<span
  class="text-[10px] font-medium uppercase tracking-wider px-1.5 inline-flex items-center h-4 leading-none rounded-medium bg-surface-inset text-default-700"
  title={dest === "core" ? "Shared by every client paired to this core" : "Specific to this client"}
>
  {destinationLabel(dest)}
</span>

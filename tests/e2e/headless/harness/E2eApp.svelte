<script lang="ts">
  // E2E mount wrapper. Mirrors the real +layout.svelte's shared-UI context
  // setup and persistent store attachments, but deliberately omits the platform
  // install (the harness installs the E2E platform before mount) and the mobile
  // branch, so this file imports nothing from @tauri-apps. It renders the REAL
  // +page.svelte app shell, so navigation, boot choreography, chat, settings,
  // etc. are all the production components under test.
  import "@loca/shared/ui/styles/base.css";
  import { onMount } from "svelte";
  import { connectionState } from "$stores/connection.svelte";
  import { coreStatusState } from "$stores/core-status.svelte";
  import { makeUiContext, setUiContext } from "@loca/shared/ui/context";
  import { getDefaultSettings } from "@loca/shared";
  import { settingsState } from "$stores";
  import { expansionState, isExpanded } from "$stores/expansion.svelte";
  import { getDuration } from "$lib/appearance/animations";
  import { viewState } from "$stores";
  import Page from "@client/routes/+page.svelte";

  const settingDefaults = getDefaultSettings();

  setUiContext(
    makeUiContext({
      getSetting: (key) => settingsState.currentSettings[key] ?? settingDefaults[key],
      animationDurationMs: (ms) => getDuration(ms),
      expansionGet: (id, fallback) => isExpanded(id, fallback),
      expansionSet: (id, value) => expansionState.set(id, value),
      expansionInit: (id, value) => {
        if (!expansionState.has(id)) expansionState.set(id, value);
      },
      platform: "desktop",
      density: "comfortable",
      pointer: "fine",
    }),
  );

  onMount(() => {
    connectionState.attach();
    coreStatusState.attach();
  });
</script>

<!-- Expose the real viewState.mode as a single reactive testid so specs can
     assert the active app mode without coupling to each mode component's
     (mobile/desktop-branched) markup. The real mode component still renders
     inside. The marker is a 1px element (visible to the locator, invisible to
     the user) so toBeVisible() resolves on the current mode. -->
<span
  data-testid={`mode-${viewState.mode}`}
  aria-hidden="true"
  style="position:absolute;top:0;left:0;width:1px;height:1px;overflow:hidden">m</span
>
<Page />

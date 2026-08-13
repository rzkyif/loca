<script lang="ts">
  // Quick settings mode: a single-open accordion of module sections. Each
  // header carries the module's on/off toggle (where one exists) and the open
  // body renders curated schema fields through the same SettingsField renderer
  // and field-change engine as the full Settings panel, so values round-trip
  // identically. Thin wrapper: QuickSettingsView owns the section loop and the
  // panel chrome; this owns the live accordion state, the settings mutations,
  // and feeds each field as the live SettingsField via the `field` snippet.
  // Auto-opened once after the first core is paired; also openable from Settings.

  import { onMount } from "svelte";
  import Bubble from "@loca/shared/ui/components/primitives/Bubble.svelte";
  import Modal from "@loca/shared/ui/components/primitives/Modal.svelte";
  import QuickSettingsView from "@loca/shared/ui/components/quick-settings/QuickSettingsView.svelte";
  import BuiltinToolkitModal from "../settings/BuiltinToolkitModal.svelte";
  import { QUICK_SETTINGS_SECTIONS } from "@loca/shared";
  import { useUiContext } from "@loca/shared/ui/context";
  import SettingsField from "../settings/SettingsField.svelte";
  import { downloadsState, settingsState, viewState } from "$stores";
  import { useSettingsForm } from "$composables/use-settings-form.svelte";
  import { useResponsiveLayout } from "$composables/use-responsive-layout.svelte";

  const form = useSettingsForm();
  const layout = useResponsiveLayout();
  const ui = useUiContext();
  // On mobile the panel mounts permanently in the chat shell and presents itself
  // as a draggable Modal bottom sheet that self-gates on the view mode (rising
  // over the live chat instead of replacing it). The Modal supplies the surface,
  // padding, rounding, and slide, so the content renders bare inside it. On
  // desktop the panel is rendered by the route only while in quickSettings mode,
  // so it draws its own Bubble with no gating.
  const mobile = $derived(ui.platform === "mobile");

  // Track the user's horizontal-mode threshold setting (same wiring as the
  // full Settings panel).
  $effect(() => {
    layout.threshold =
      (settingsState.currentSettings["appearance.settings.horizontalThreshold"] as number) ?? 680;
  });
  $effect(() => layout.observe());

  onMount(() => form.validateAllFields());

  // Single-open accordion. The general section starts open.
  let openId = $state<string | null>("general");

  function toggleOpen(id: string): void {
    openId = openId === id ? null : id;
  }

  // Optimistic: currentSettings flips synchronously, so the body reacts at
  // once; if the flush later fails and rolls back, the View's `selected &&
  // enabled` conjunction collapses the section again on its own.
  function setEnabled(id: string, value: boolean): void {
    const section = QUICK_SETTINGS_SECTIONS.find((s) => s.id === id);
    if (!section?.enabledField) return;
    void form.handleChange(section.enabledField, value);
    if (value) openId = id;
    else if (openId === id) openId = null;
  }

  // Both exits (close button and bottom button) go to Settings while required
  // downloads are missing, e.g. a model picked here still needs downloading;
  // Settings pops its pending-downloads modal on arrival. Otherwise back to
  // chat.
  const hasPending = $derived(downloadsState.hasPending);
  const exitLabel = $derived(hasPending ? "Review Pending Downloads" : "Continue to Chat");
  const exitTitle = $derived(hasPending ? "Review Pending Downloads" : "Back to Chat");
  const exitIcon = $derived(
    hasPending ? "i-material-symbols-download-rounded" : "i-material-symbols-arrow-forward-rounded",
  );

  function exit(): void {
    viewState.navigate(hasPending ? "settings" : "chat");
  }
</script>

{#if mobile}
  <Modal
    open={viewState.mode === "quickSettings"}
    onclose={exit}
    positioning="fixed"
    ariaLabel="Quick Settings"
  >
    {@render content()}
  </Modal>
  <BuiltinToolkitModal surface="quickSettings" />
{:else}
  <!-- Positioned, bubble-sized wrapper so the built-in-tools prompt's backdrop
       (Modal's default absolute positioning) stays clipped to this panel.
       pointer-events-none keeps the wrapper's empty margins click-through; the
       Modal re-enables pointer events on its own surface. -->
  <div class="relative w-fit pointer-events-none">
    <Bubble
      selectedAlignment={settingsState.getAlignment()}
      extraClass="flex flex-col w-[34rem] max-w-full max-h-[80vh] overflow-hidden"
    >
      {@render content()}
    </Bubble>
    <BuiltinToolkitModal surface="quickSettings" />
  </div>
{/if}

{#snippet content()}
  <QuickSettingsView
    values={settingsState.currentSettings}
    {openId}
    horizontal={layout.horizontal}
    {exitLabel}
    {exitTitle}
    {exitIcon}
    onExit={exit}
    onToggleSection={toggleOpen}
    onSetEnabled={setEnabled}
    containerRef={(el) => (layout.containerEl = el ?? undefined)}
  >
    {#snippet field(f)}
      <SettingsField
        field={f}
        monitors={[]}
        fonts={[]}
        error={form.validationErrors[f.id] ?? null}
        horizontal={layout.horizontal}
        onChange={form.handleChange}
        onReset={form.resetToDefault}
        onPresetSelect={form.handlePresetSelect}
      />
    {/snippet}
  </QuickSettingsView>
{/snippet}

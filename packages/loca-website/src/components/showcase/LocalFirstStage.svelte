<script lang="ts">
  import { onMount } from "svelte";
  import type { ComponentProps } from "svelte";
  import gsap from "gsap";
  import { getDefaultSettings } from "@loca/shared/domain/settings/engine";
  import type { SettingField } from "@loca/shared/domain/settings/types";
  import { modelPresetFieldSamples } from "@loca/shared/ui/samples";
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import SettingsFieldView from "@loca/shared/ui/components/settings/SettingsFieldView.svelte";
  import ModelPresetFieldView from "@loca/shared/ui/components/settings/ModelPresetFieldView.svelte";
  import SettingsDemoFooter from "../demos/SettingsDemoFooter.svelte";
  import Cursor from "./Cursor.svelte";
  import { Demo, settingsShowcaseGroups as groups, type StageProps } from "../../lib/showcase";

  let { register }: StageProps = $props();

  type Shell = {
    selectGroup: (id: string) => void;
    setSearch: (active: boolean) => void;
    getScrollEl: () => HTMLElement | undefined;
    reset: () => void;
  };
  let shell = $state<Shell>();
  let selectedGroupId = $state("llm");

  // A mutable settings map so flipping `*.provider` re-runs the section
  // visibility derives in SettingsContentView and the external sections slide in.
  // Speech-to-Text and Text-to-Speech ship disabled, which hides their preset +
  // provider fields entirely; the demo enables both so it can show their local
  // presets and the external switch.
  let values = $state<Record<string, unknown>>({
    ...getDefaultSettings(),
    "stt.enabled": true,
    "tts.enabled": true,
  });

  // The real adaptive preset picker (single-source component), fed from the
  // sample and driven by its own callbacks so the Custom dropdowns work. Typed
  // from the View's own props (not `typeof sample`, which would pin `selected`
  // to its literal `false` and reject the demo toggling it), with buckets +
  // custom required since the demo always supplies both.
  type PresetProps = ComponentProps<typeof ModelPresetFieldView>;
  type PresetState = PresetProps & {
    buckets: NonNullable<PresetProps["buckets"]>;
    custom: NonNullable<PresetProps["custom"]>;
  };
  let preset = $state<PresetState>(structuredClone(modelPresetFieldSamples.recommended));

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();

  function setProvider(key: string, value: string): void {
    values = { ...values, [key]: value };
  }

  // First interaction: pick the Custom card (a native <select> can't be
  // programmatically opened, so the demo selects Custom, then changes the model
  // on a second click, reading as the two-step pick the user described).
  function selectCustom(): void {
    preset = {
      ...preset,
      buckets: preset.buckets.map((b) => ({ ...b, selected: false })),
      custom: preset.custom ? { ...preset.custom, selected: true } : preset.custom,
    };
  }

  function pickModel(modelValue: string): void {
    preset = {
      ...preset,
      custom: preset.custom
        ? { ...preset.custom, selected: true, model: { ...preset.custom.model, value: modelValue } }
        : preset.custom,
    };
  }

  function reset(): void {
    values = { ...getDefaultSettings(), "stt.enabled": true, "tts.enabled": true };
    preset = structuredClone(modelPresetFieldSamples.recommended);
    shell?.reset();
    selectedGroupId = "llm";
  }

  onMount(() => {
    if (!stageEl || !cursorRef) return;
    const demo = new Demo(cursorRef, stageEl);
    demo.placeFrac(0.5, 0.5);

    const tl = gsap.timeline({ paused: true });
    const provider = 'select[aria-label="Provider"]';
    const modelSelect = 'select[aria-label="Choose a model"]';
    const scrollEl = () => shell?.getScrollEl();

    // Bring the Model section (Provider + the preset picker) into view first, so
    // the cursor never reaches for a field that is below the fold.
    demo.scroll(tl, scrollEl, () => demo.scrollSelectorTop(scrollEl, provider, 90), {
      duration: 0.6,
    });
    demo.hold(tl, 0.2);

    // 1. Inspect the local preset picker, then pick a custom model (two taps:
    //    select the Custom card, then choose a model from its dropdown).
    demo.move(tl, modelSelect, { duration: 1.0 });
    demo.hover(tl, modelSelect, true);
    demo.click(tl, modelSelect, () => selectCustom());
    demo.hold(tl, 0.7);
    demo.click(tl, modelSelect, () => pickModel("Qwen/Qwen3.5-9B"));
    demo.hover(tl, modelSelect, false);
    demo.hold(tl, 0.8);

    // 2. Switch the language model provider to external.
    demo.move(tl, provider, { duration: 0.8 });
    demo.hover(tl, provider, true);
    demo.click(tl, provider, () => setProvider("llm.provider", "external"));
    demo.hover(tl, provider, false);
    demo.hold(tl, 0.9);

    // 3. Same switch for Speech-to-Text.
    demo.move(tl, ".settings-group-stt", { duration: 0.8 });
    demo.hover(tl, ".settings-group-stt", true);
    demo.click(tl, ".settings-group-stt", () => shell?.selectGroup("stt"));
    demo.hover(tl, ".settings-group-stt", false);
    // Wait out the full group-swap slide (two BASE_MS phases) so the previous
    // group has unmounted and the new one has settled before the cursor moves;
    // otherwise the Provider query can resolve to the still-sliding old group.
    demo.hold(tl, 0.85);
    demo.scroll(tl, scrollEl, () => demo.scrollSelectorTop(scrollEl, provider, 90), {
      duration: 0.5,
    });
    demo.move(tl, provider, { duration: 0.8 });
    demo.click(tl, provider, () => setProvider("stt.provider", "external"));
    demo.hold(tl, 0.7);

    // 4. And for Text-to-Speech.
    demo.move(tl, ".settings-group-tts", { duration: 0.8 });
    demo.hover(tl, ".settings-group-tts", true);
    demo.click(tl, ".settings-group-tts", () => shell?.selectGroup("tts"));
    demo.hover(tl, ".settings-group-tts", false);
    demo.hold(tl, 0.85);
    demo.scroll(tl, scrollEl, () => demo.scrollSelectorTop(scrollEl, provider, 90), {
      duration: 0.5,
    });
    demo.move(tl, provider, { duration: 0.8 });
    demo.click(tl, provider, () => setProvider("tts.provider", "external"));
    register({
      timeline: tl,
      reset: () => {
        reset();
        tl.pause(0);
        demo.blur();
        demo.placeFrac(0.5, 0.5);
      },
    });
    return () => tl.kill();
  });
</script>

{#snippet fieldRenderer(f: SettingField)}
  {#if f.id === "llm.preset"}
    <ModelPresetFieldView
      checkLabel={preset.checkLabel}
      checkIcon={preset.checkIcon}
      buckets={preset.buckets}
      custom={preset.custom}
      onSelectBucket={(id) => {
        preset = {
          ...preset,
          buckets: preset.buckets.map((b) => ({ ...b, selected: b.id === id })),
          custom: preset.custom ? { ...preset.custom, selected: false } : preset.custom,
        };
      }}
      onSelectCustom={() => selectCustom()}
      onModelSelect={(v) => pickModel(v)}
    />
  {:else}
    <SettingsFieldView field={f} value={values[f.id]} horizontal />
  {/if}
{/snippet}

{#snippet sidebarFooter(collapsed: boolean)}
  <SettingsDemoFooter {collapsed} />
{/snippet}

<div
  bind:this={stageEl}
  class="relative w-full h-full overflow-hidden flex items-center justify-center"
>
  <div class="shrink-0">
    <SettingsShellView
      bind:this={shell}
      {groups}
      bind:selectedGroupId
      sizeClass="w-[620px] h-[960px]"
      {sidebarFooter}
    >
      {#snippet groupContent(gid)}
        <SettingsContentView groupId={gid} {values} field={fieldRenderer} />
      {/snippet}
    </SettingsShellView>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>

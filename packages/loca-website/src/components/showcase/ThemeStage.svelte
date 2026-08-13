<script lang="ts">
  import { onMount } from "svelte";
  import gsap from "gsap";
  import { getDefaultSettings, SETTINGS_SCHEMA } from "@loca/shared/domain/settings/engine";
  import { makeUiContext, setUiContext } from "@loca/shared/ui/context";
  import { BASE_MS } from "@loca/shared/ui/animations";
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import SettingsFieldView from "@loca/shared/ui/components/settings/SettingsFieldView.svelte";
  import type { SettingField } from "@loca/shared/domain/settings/types";
  import SettingsDemoFooter from "../demos/SettingsDemoFooter.svelte";
  import Cursor from "./Cursor.svelte";
  import { Demo, type Timeline } from "../../lib/showcase";

  let { register }: { register: (h: { timeline: Timeline; reset: () => void }) => void } = $props();

  // Reactive values so each control visibly moves AND the subtree UI context
  // re-derives from them: flipping `appearance.bubbleBlurEnabled` re-runs the
  // settings window's halo ring count (Bubble reads it from the context), so the
  // blur around the window actually turns off.
  const defaults = getDefaultSettings();
  let values = $state<Record<string, unknown>>({ ...defaults });

  const reduce =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  setUiContext(
    makeUiContext({
      getSetting: (key) => (key === "stt.enabled" ? true : (values[key] ?? defaults[key])),
      animationDurationMs: (ms = BASE_MS) => (reduce ? 0 : ms),
    }),
  );

  const groups = SETTINGS_SCHEMA.filter((g) => !g.hidden).map((g) => ({
    id: g.id,
    name: g.name,
    icon: g.icon,
    iconInactive: g.iconInactive ?? g.icon,
  }));

  type Shell = {
    selectGroup: (id: string) => void;
    setSearch: (active: boolean) => void;
    getScrollEl: () => HTMLElement | undefined;
    reset: () => void;
  };
  let shell = $state<Shell>();
  let selectedGroupId = $state("appearance");

  // The vibrant choices the demo applies. The default font becomes a serif so the
  // switch from the system sans-serif is unmistakable.
  const NEW_FONT = "Georgia";
  const NEW_MONO = "Courier New";
  const NEW_BASE_COLOR = "oklch(0.97 0.05 300)"; // a light lavender panel tint
  const NEW_SHADOW_COLOR = "#ff00ffff"; // maximum pink/magenta, fully opaque
  const NEW_SHADOW_DISTANCE = "40px"; // the Shadow Size slider's max
  const SERIF_STACK = `"Georgia", "Times New Roman", Times, serif`;
  const MONO_FALLBACK = `ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace`;

  // The Font / Code Font options resolve from installed system fonts at runtime,
  // which the website can't enumerate, so the demo supplies a small web-safe set
  // (the chosen faces all render in the browser).
  const FONT_OPTIONS = [
    { value: "default", label: "Default" },
    { value: "Georgia", label: "Georgia" },
    { value: "Times New Roman", label: "Times New Roman" },
    { value: "Courier New", label: "Courier New" },
  ];

  // The wrapper the appearance CSS variables are scoped to: writing them here
  // cascades to the whole settings panel (mirrors use-theme on documentElement),
  // so the window restyles live without touching the rest of the page.
  let themeRoot: HTMLElement | undefined = $state();

  const isDarkTheme = $derived(values["appearance.theme"] === "dark");

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();

  function setVal(key: string, v: unknown): void {
    values = { ...values, [key]: v };
  }
  function setProp(name: string, v: string): void {
    themeRoot?.style.setProperty(name, v);
  }

  function reset(): void {
    values = { ...defaults };
    if (themeRoot) {
      themeRoot.removeAttribute("style");
      themeRoot.classList.remove("theme-demo-shadowed");
    }
    shell?.reset();
    selectedGroupId = "appearance";
  }

  onMount(() => {
    if (!stageEl || !cursorRef) return;
    const demo = new Demo(cursorRef, stageEl);
    demo.placeFrac(0.5, 0.4);

    const tl = gsap.timeline({ paused: true });
    const scrollEl = () => shell?.getScrollEl();

    // 1. Fonts (Theme section, near the top). The default font is set directly on
    //    the wrapper (inherited text reads the element's font-family, not the
    //    `--font-default` var, which only `body` consumes).
    demo.move(tl, 'select[aria-label="Font"]', { duration: 0.9 });
    demo.click(tl, 'select[aria-label="Font"]', () => {
      setVal("appearance.defaultFont", NEW_FONT);
      if (themeRoot) themeRoot.style.fontFamily = SERIF_STACK;
    });
    demo.hold(tl, 0.4);
    demo.move(tl, 'select[aria-label="Code Font"]', { duration: 0.7 });
    demo.click(tl, 'select[aria-label="Code Font"]', () => {
      setVal("appearance.monoFont", NEW_MONO);
      setProp("--font-mono", `"${NEW_MONO}", ${MONO_FALLBACK}`);
    });
    demo.hold(tl, 0.4);

    // 2. Turn off the blur around bubbles FIRST, so the colour change below reads
    //    against a clean panel edge instead of a frosted halo.
    demo.scroll(tl, scrollEl, 360, { duration: 1.0 });
    demo.move(tl, '[aria-label="Blur Around Bubbles"]', { duration: 0.7 });
    demo.click(tl, '[aria-label="Blur Around Bubbles"]', () => {
      setVal("appearance.bubbleBlurEnabled", false);
    });
    demo.hold(tl, 0.4);
    // Shadow colour goes maximum pink/magenta. The flip re-asserts
    // `--bubble-shadow-color-*` on every element under `.demo-frame`, so an
    // inherited value is overridden; a marker class carries the override with
    // `!important` to win per-element (see the style block below).
    demo.move(tl, 'input[aria-label="Shadow Color value"]', { duration: 0.7 });
    demo.click(tl, 'input[aria-label="Shadow Color value"]', () => {
      setVal("appearance.bubbleShadowColor", NEW_SHADOW_COLOR);
      themeRoot?.classList.add("theme-demo-shadowed");
    });
    demo.hold(tl, 0.4);
    // Crank the shadow size to its max so the magenta shadow reads dramatically.
    demo.move(tl, 'input[aria-label="Shadow Size"]', { duration: 0.7 });
    demo.click(tl, 'input[aria-label="Shadow Size"]', () => {
      setVal("appearance.bubbleShadowDistance", Number.parseInt(NEW_SHADOW_DISTANCE, 10));
      setProp("--bubble-shadow-distance", NEW_SHADOW_DISTANCE);
    });
    demo.hold(tl, 0.4);

    // 3. Base colour recolours the whole panel.
    demo.scroll(tl, scrollEl, 700, { duration: 1.0 });
    demo.move(tl, 'input[aria-label="Base Color value"]', { duration: 0.7 });
    demo.click(tl, 'input[aria-label="Base Color value"]', () => {
      setVal("appearance.defaultColor", NEW_BASE_COLOR);
      setProp("--default-base", NEW_BASE_COLOR);
    });
    demo.hold(tl, 0.5);

    // 4. Corner radii to 0 squares everything.
    demo.scroll(tl, scrollEl, 1180, { duration: 1.0 });
    demo.move(tl, 'input[aria-label="Small Corner Radius"]', { duration: 0.7 });
    demo.click(tl, 'input[aria-label="Small Corner Radius"]', () => {
      setVal("appearance.roundedSmall", 0);
      setProp("--rounded-small", "0px");
    });
    demo.click(tl, 'input[aria-label="Medium Corner Radius"]', () => {
      setVal("appearance.roundedMedium", 0);
      setProp("--rounded-medium", "0px");
    });
    demo.click(tl, 'input[aria-label="Large Corner Radius"]', () => {
      setVal("appearance.roundedLarge", 0);
      setProp("--rounded-large", "0px");
    });
    register({
      timeline: tl,
      reset: () => {
        reset();
        tl.pause(0);
        demo.blur();
        demo.placeFrac(0.5, 0.4);
      },
    });
    return () => tl.kill();
  });
</script>

{#snippet fieldRenderer(f: SettingField)}
  {#if f.id === "appearance.defaultFont" || f.id === "appearance.monoFont"}
    <SettingsFieldView field={f} value={values[f.id]} horizontal selectOptions={FONT_OPTIONS} />
  {:else}
    <SettingsFieldView field={f} value={values[f.id]} horizontal isDark={isDarkTheme} />
  {/if}
{/snippet}

{#snippet sidebarFooter(collapsed: boolean)}
  <SettingsDemoFooter {collapsed} />
{/snippet}

<div
  bind:this={stageEl}
  class="relative w-full h-full overflow-hidden flex items-center justify-center"
>
  <!-- Appearance CSS vars are scoped to this wrapper so the panel restyles live. -->
  <div bind:this={themeRoot} class="shrink-0">
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

<style>
  /* The demo's vibrant shadow colour. The theme-flip writes `--bubble-shadow-color-*`
     onto every element under `.demo-frame`, so an inherited override never reaches
     the shadow; this per-element `!important` rule wins for each one. The colour
     matches NEW_SHADOW_COLOR. */
  :global(.theme-demo-shadowed),
  :global(.theme-demo-shadowed *) {
    --bubble-shadow-color-light: #ff00ffff !important;
    --bubble-shadow-color-dark: #ff00ffff !important;
  }
</style>

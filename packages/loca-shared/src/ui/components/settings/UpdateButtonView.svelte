<script lang="ts" module>
  // Presentational phases for the Settings sidebar update button. The client owns
  // the live update state machine (client + core + sidecar checks and the
  // install/restart handoff) and the version string; it maps all of that down to
  // one of these phases, so this View stays pure and the gallery renders every
  // visible state identically to the app.
  export type UpdateButtonPhase =
    "idle" | "checking" | "available" | "updating" | "clientRestartPending";
</script>

<script lang="ts">
  // Bottom-left of the Settings sidebar: the combined client + core + sidecar
  // update affordance. Custom row (not SidebarItem) because the idle/available
  // states render the Loca SVG mask rather than a unocss icon class, and the
  // "available" phase pulses between two yellow shades. The phase is the only
  // copy input: this View owns the label for each one so no caller can render
  // different text for the same state. Clicking routes the raw intent back out.
  import CollapsibleLabel from "../primitives/CollapsibleLabel.svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  function noop(): void {}

  let {
    phase = "idle",
    collapsed,
    /** "available" pulses between two yellow shades; the client toggles this on
     *  the button's color-transition cadence so the hue is always mid-tween. */
    blink = false,
    disabled = false,
    onClick = noop,
  }: {
    phase?: UpdateButtonPhase;
    collapsed: boolean;
    blink?: boolean;
    disabled?: boolean;
    onClick?: () => void;
  } = $props();

  const ui = useUiContext();
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));
  const mobile = $derived(ui.platform === "mobile");

  // Touch has no hover, so the "available" button can't reveal "Install Updates"
  // on hover the way desktop does; show the actionable label outright instead.
  let hovering = $state(false);

  const label = $derived.by<string>(() => {
    switch (phase) {
      case "checking":
        return "Checking…";
      case "available":
        return hovering || mobile ? "Install Updates" : "Updates Available";
      case "updating":
        return "Updating…";
      case "clientRestartPending":
        return "Restart to Update";
      default:
        return "Check for Updates";
    }
  });

  // Mobile renders the row as a tappable card matching SidebarItem (taller,
  // padded, rounded-large, resting on an inset fill); desktop keeps the compact
  // flush row that fills in on hover.
  const layoutClass = $derived(
    mobile
      ? "min-h-12 px-3 gap-3 rounded-large"
      : `h-8 pl-1.5 ${collapsed ? "pr-0" : "pr-2.5"} gap-1.5 rounded-medium`,
  );
  const bgClass = $derived(
    mobile ? "bg-surface-inset hov:bg-surface-inset-strong" : "hov:bg-surface-inset",
  );

  // State-driven icon. Idle / available render the Loca SVG mask; the transient
  // states use unocss icon classes for the loading / restart glyphs.
  const iconClass = $derived.by<string | null>(() => {
    switch (phase) {
      case "checking":
      case "updating":
        return "i-line-md:loading-loop";
      case "clientRestartPending":
        return "i-material-symbols-restart-alt-rounded";
      default:
        return null;
    }
  });

  // Lives on the button so the icon (currentColor) and label inherit it and the
  // button's 500ms color transition tweens the whole row together.
  const tone = $derived.by<string>(() => {
    if (phase === "available") {
      return blink ? "text-accent-yellow-700" : "text-accent-yellow-500";
    }
    return "text-default-500 hov:text-default-700";
  });
</script>

<button
  type="button"
  class="hov:cursor-pointer flex items-center {layoutClass} [transition:color_120ms,background-color_120ms,padding_200ms] disabled:opacity-50 disabled:pointer-events-none {tone} {bgClass}"
  title={collapsed ? label : undefined}
  aria-label={label}
  {disabled}
  onmouseenter={() => (hovering = true)}
  onmouseleave={() => (hovering = false)}
  onfocus={() => (hovering = true)}
  onblur={() => (hovering = false)}
  onclick={onClick}
  use:ripple={{ disabled, durationMs: rippleDuration }}
>
  <span class="relative flex shrink-0">
    {#if iconClass}
      <i class="flex {mobile ? 'text-2xl' : 'text-xl'} shrink-0 {iconClass}"></i>
    {:else}
      <span
        class="{mobile ? 'w-6 h-6' : 'w-5 h-5'} bg-current shrink-0"
        style="mask:url(/loca.svg) center/contain no-repeat;-webkit-mask:url(/loca.svg) center/contain no-repeat;"
        aria-hidden="true"
      ></span>
    {/if}
  </span>
  <CollapsibleLabel {collapsed} class="text-base text-left">
    {label}
  </CollapsibleLabel>
</button>

<script lang="ts">
  import type { Snippet } from "svelte";
  import { useUiContext } from "../../context.ts";

  type Placement = "top" | "bottom" | "auto";

  let {
    open,
    anchor,
    onclose,
    placement = "auto",
    margin = 8,
    dismissOnEsc = true,
    ariaLabel,
    class: extraClass = "",
    children,
  }: {
    open: boolean;
    anchor: HTMLElement | null | undefined;
    onclose: () => void;
    placement?: Placement;
    margin?: number;
    dismissOnEsc?: boolean;
    ariaLabel?: string;
    class?: string;
    children: Snippet;
  } = $props();

  const ui = useUiContext();

  let backdropEl: HTMLDivElement | undefined = $state();
  let popupEl: HTMLDivElement | undefined = $state();
  // Hide the popup until the real height has been measured and the position
  // computed. Without this guard it briefly flashes at the backdrop origin
  // before snapping into place.
  let popupStyle = $state("visibility: hidden;");

  function computeStyle() {
    if (!backdropEl || !popupEl || !anchor) return;
    const aRect = anchor.getBoundingClientRect();
    const b = backdropEl.getBoundingClientRect();
    // Measure actual popup height. Content varies and a hardcoded estimate
    // routinely under-counted, causing "above" placement to clip when the
    // real popup couldn't fit.
    const ph = popupEl.offsetHeight;
    const cx = aRect.left - b.left + aRect.width / 2;
    const spaceAbove = aRect.top - b.top;
    const spaceBelow = b.bottom - aRect.bottom;

    let placeAbove: boolean;
    if (placement === "top") placeAbove = true;
    else if (placement === "bottom") placeAbove = false;
    else if (spaceAbove >= ph + margin) placeAbove = true;
    else if (spaceBelow >= ph + margin) placeAbove = false;
    else placeAbove = spaceAbove > spaceBelow;

    if (placeAbove) {
      const top = aRect.top - b.top - margin;
      popupStyle = `left: ${cx}px; top: ${top}px; transform: translate(-50%, -100%); visibility: visible;`;
    } else {
      const top = aRect.bottom - b.top + margin;
      popupStyle = `left: ${cx}px; top: ${top}px; transform: translate(-50%, 0); visibility: visible;`;
    }
  }

  $effect(() => {
    if (open && anchor) {
      popupStyle = "visibility: hidden;";
      // Double rAF: first frame lets Svelte mount the popup so layout can
      // run, second frame guarantees offsetHeight reflects the laid-out size.
      requestAnimationFrame(() => requestAnimationFrame(() => computeStyle()));
    }
  });

  $effect(() => {
    if (!open || !dismissOnEsc) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onclose();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });

  // The Android back press closes an open popover (the touch analogue of Esc)
  // before the app navigates. Registered with the host's back registry (inert
  // on the website), so this shared primitive owns back without client imports.
  $effect(() => {
    if (!open || !dismissOnEsc) return;
    return ui.registerBack(() => {
      onclose();
      return true;
    });
  });

  // Move focus into the dialog on open and restore it to the previously-focused
  // element on close, so keyboard users aren't stranded at the document root.
  $effect(() => {
    if (!open) return;
    const prevFocus = document.activeElement as HTMLElement | null;
    requestAnimationFrame(() => popupEl?.focus());
    return () => prevFocus?.focus?.();
  });
</script>

<svelte:window onresize={computeStyle} />

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- pointer-events-auto re-enables clicks: this popover mounts under
       pointer-events-none wrappers (the settings Bubble root and the panel-column
       layer), so without it the backdrop and popup inherit none and every click
       falls through to the settings items behind. Modal re-enables the same way. -->
  <div
    bind:this={backdropEl}
    class="absolute inset-0 z-50 pointer-events-auto"
    onpointerdown={onclose}
  >
    <!-- Blur+dim lives on its OWN layer, never as the popup's parent. WebKit
         folds a backdrop-filter element's compositing-layer children (e.g. a
         scroll container) into the filtered region, which would blur the popup's
         own content. Keeping this layer childless and the popup a separate
         sibling above it avoids that. -->
    <div
      class="absolute inset-0 bg-black/20 backdrop-blur pointer-events-none"
      aria-hidden="true"
    ></div>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- Dismiss on pointerdown OUTSIDE only (not click): a press that starts
         inside the popup (e.g. dragging a slider) and releases on the backdrop
         must not close it. Stopping pointerdown here keeps presses that begin
         inside from reaching the backdrop. -->
    <div
      bind:this={popupEl}
      class="absolute bg-surface rounded-large shadow-xl flex flex-col p-5 gap-3 {extraClass}"
      style={popupStyle}
      role="dialog"
      tabindex="-1"
      aria-label={ariaLabel}
      onpointerdown={(e) => e.stopPropagation()}
    >
      {@render children()}
    </div>
  </div>
{/if}

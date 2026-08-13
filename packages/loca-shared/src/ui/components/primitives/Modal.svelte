<script lang="ts">
  import type { Snippet } from "svelte";
  import { fade } from "svelte/transition";
  import { useUiContext } from "../../context.ts";

  type MaxWidth = "sm" | "md" | "lg" | "xl" | "fit";
  type Positioning = "absolute" | "fixed";
  type Surface = "default" | "transparent";

  let {
    open,
    onclose,
    maxWidth = "md",
    positioning = "absolute",
    surface = "default",
    dismissOnBackdrop = true,
    dismissOnEsc = true,
    ariaLabel,
    class: extraClass = "",
    children,
  }: {
    open: boolean;
    onclose: () => void;
    maxWidth?: MaxWidth;
    positioning?: Positioning;
    surface?: Surface;
    dismissOnBackdrop?: boolean;
    dismissOnEsc?: boolean;
    ariaLabel?: string;
    class?: string;
    children: Snippet;
  } = $props();

  const ui = useUiContext();
  // On mobile a modal presents as a bottom sheet: full-width, pinned to the
  // bottom edge, rounded only on top, and height-capped so long content
  // scrolls. The single-source rule keeps this in the shared primitive so every
  // modal consumer (color picker, downloads, confirm, ...) inherits it at once.
  const sheet = $derived(ui.platform === "mobile");
  // A dismissable sheet gets a grab handle and drag-to-dismiss; a forced one
  // (dismissOnBackdrop false) gets neither, so the handle never implies an exit
  // the modal won't honor.
  const draggable = $derived(sheet && dismissOnBackdrop);

  // Drag-to-dismiss: track the finger's downward travel and translate the sheet
  // to follow it; release past the threshold closes, otherwise it springs back.
  // Desktop dialogs never enter this path (draggable is false off touch).
  const DISMISS_PX = 100;
  let dragY = $state(0);
  let dragging = $state(false);
  let settling = $state(false);
  let dragStartY = 0;
  let surfaceEl: HTMLDivElement | undefined = $state();
  // How far down (percent of sheet height) a drag-to-dismiss had already pulled
  // the sheet when it was released, so the exit slide continues from the finger
  // instead of snapping back to rest first. Read once by the exit transition.
  let exitStartPct = 0;

  function onDragStart(e: PointerEvent): void {
    if (!draggable) return;
    dragging = true;
    settling = false;
    dragStartY = e.clientY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onDragMove(e: PointerEvent): void {
    if (!dragging) return;
    dragY = Math.max(0, e.clientY - dragStartY);
  }
  function onDragEnd(): void {
    if (!dragging) return;
    dragging = false;
    if (dragY > DISMISS_PX) {
      // Hand the current offset to the exit transition so the slide-out
      // continues from the finger. The transition's animation overrides the
      // inline transform, so clearing dragY now keeps the next open un-dragged
      // with no flash.
      exitStartPct = Math.min(100, (dragY / (surfaceEl?.offsetHeight || 1)) * 100);
      dragY = 0;
      onclose();
    } else {
      // Spring back: animate dragY -> 0 with a brief CSS transition (no Svelte
      // transition is running while the sheet stays open, so they can't fight).
      settling = true;
      dragY = 0;
      setTimeout(() => (settling = false), 200);
    }
  }

  // Dismiss on a backdrop press. preventDefault() stops the tap's synthesized
  // click: closing on pointerdown removes this overlay before the click fires, so
  // without it the click falls through to whatever the backdrop was covering
  // (e.g. a message or button behind a bottom sheet) instead of only closing.
  function onBackdropPointerDown(e: PointerEvent): void {
    e.preventDefault();
    onclose();
  }

  // Slide the sheet up on open and down on close; instant (no css) off touch so
  // desktop dialogs keep their existing appear/disappear behavior.
  function sheetSlide(_node: Element, { enabled }: { enabled: boolean }) {
    if (!enabled) return { duration: 0 };
    // Begin at the drag offset on a drag-dismiss (else 0), so the slide is one
    // continuous motion; the open intro and a no-drag close both begin at 0.
    const from = exitStartPct;
    exitStartPct = 0;
    return {
      duration: ui.animationDurationMs(220),
      css: (t: number) => `transform: translateY(${from + (1 - t) * (100 - from)}%)`,
    };
  }

  const maxWidthClass = $derived(
    {
      sm: "max-w-sm",
      md: "max-w-md",
      lg: "max-w-lg",
      xl: "max-w-xl",
      fit: "",
    }[maxWidth],
  );

  const sizeClass = $derived(
    sheet
      ? "w-full max-w-full"
      : maxWidth === "fit"
        ? "w-fit"
        : `w-[calc(100%-2rem)] ${maxWidthClass}`,
  );

  const positioningClass = $derived(
    positioning === "fixed" ? "fixed pointer-events-auto" : "absolute pointer-events-auto",
  );

  // Bottom-aligned as a sheet, centered as a dialog.
  const alignClass = $derived(sheet ? "items-end justify-stretch" : "items-center justify-center");

  const surfaceClass = $derived(
    surface === "transparent"
      ? ""
      : // The sheet pads its bottom past the home indicator (safe-area); the soft
        // keyboard is cleared by lifting the WHOLE sheet (the container's
        // --keyboard-inset padding below), so its bottom edge rests on the keyboard
        // top instead of hiding behind it. max-h shrinks by the keyboard so a tall
        // sheet stays fully above it; overscroll-contain stops a scroll at the
        // sheet's end from chaining to the page behind it.
        sheet
        ? "bg-surface rounded-t-large p-5 pb-[calc(1.25rem+var(--safe-area-inset-bottom,0px))] shadow-xl max-h-[calc(85dvh-var(--keyboard-inset,0px))] overflow-y-auto overscroll-contain"
        : "bg-surface rounded-large p-5 shadow-xl",
  );

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

  // The Android back press is the touch analogue of Esc: while a dismissable
  // modal is open it closes the modal before the app navigates away. Registered
  // with the host's back registry (a no-op on the website), so the shared
  // primitive owns back without importing client state.
  $effect(() => {
    if (!open || !dismissOnEsc) return;
    return ui.registerBack(() => {
      onclose();
      return true;
    });
  });
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="{positioningClass} inset-0 flex {alignClass} z-50 rounded-large {sheet
      ? 'pb-[var(--keyboard-inset,0px)]'
      : ''}"
    onpointerdown={dismissOnBackdrop ? onBackdropPointerDown : undefined}
  >
    <!-- Blur+dim lives on its OWN layer behind the dialog, never as the dialog's
         parent. WebKit folds a backdrop-filter element's compositing-layer
         children (e.g. a scroll container) into the filtered region, which would
         blur the dialog's own content. Keeping this layer childless and the
         dialog a separate sibling above it avoids that. -->
    <div
      class="absolute inset-0 bg-black/20 backdrop-blur rounded-large pointer-events-none"
      aria-hidden="true"
      transition:fade={{ duration: sheet ? ui.animationDurationMs(220) : 0 }}
    ></div>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- Dismiss on pointerdown OUTSIDE only (not click): a press that starts
         inside the dialog (e.g. dragging a slider) and releases on the backdrop
         must not close it. Stopping pointerdown here keeps inside-presses from
         reaching the backdrop. -->
    <div
      bind:this={surfaceEl}
      class="relative {surfaceClass} {sizeClass} flex flex-col gap-3 {extraClass}"
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      tabindex={-1}
      onpointerdown={(e) => e.stopPropagation()}
      transition:sheetSlide={{ enabled: sheet }}
      style:transform={dragY ? `translateY(${dragY}px)` : undefined}
      style:transition={settling ? "transform 0.2s cubic-bezier(0.4, 0, 0.2, 1)" : undefined}
    >
      {#if draggable}
        <!-- Grab handle: a drag here (or a downward flick) dismisses the sheet.
             touch-none so the gesture drags the sheet instead of scrolling it. -->
        <div
          class="-mt-2 mb-1 flex shrink-0 justify-center touch-none cursor-grab"
          onpointerdown={onDragStart}
          onpointermove={onDragMove}
          onpointerup={onDragEnd}
          onpointercancel={onDragEnd}
        >
          <div class="h-1.5 w-10 rounded-large bg-default-300"></div>
        </div>
      {/if}
      {@render children()}
    </div>
  </div>
{/if}

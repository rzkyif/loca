/**
 * Tracks the width of a container element via ResizeObserver and exposes a
 * `horizontal` flag that flips when the width crosses a threshold. Caller
 * binds the element with `bind:this={layout.containerEl}`; `containerWidth`
 * and `horizontal` update reactively.
 */

export class ResponsiveLayout {
  containerEl: HTMLElement | undefined = $state();
  containerWidth = $state(0);
  threshold = $state(680);

  horizontal = $derived(this.containerWidth >= this.threshold);

  /** Wire up the ResizeObserver. Call from a `$effect` so teardown is automatic. */
  observe(): () => void {
    if (!this.containerEl) return () => {};
    const ro = new ResizeObserver((entries) => {
      this.containerWidth = entries[0].contentRect.width;
    });
    ro.observe(this.containerEl);
    return () => ro.disconnect();
  }
}

export function useResponsiveLayout(threshold = 680): ResponsiveLayout {
  const layout = new ResponsiveLayout();
  layout.threshold = threshold;
  return layout;
}

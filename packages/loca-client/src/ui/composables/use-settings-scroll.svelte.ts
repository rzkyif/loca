/**
 * Layout-shift "anchor pin" helper for the Settings panel's scroll container
 * (the scroll viewport itself, plus the top/bottom fades, are owned by the
 * shared `SettingsShellView`; the client feeds its scroll element in via
 * `scrollEl` for anchoring only).
 *
 * Anchor pinning (`withAnchor`): a single tick-and-scroll isn't enough because
 * the sidebar's slide transition keeps shifting offsets for ~200 ms after our
 * first measurement. Re-pinning per frame for the full transition window tracks
 * the anchor through the whole settle process. The fallback chain (field →
 * section → group) handles cases where the original anchor gets unmounted
 * mid-transition.
 */

import { BASE_MS, getDuration } from "$lib/appearance/animations";

type AnchorEntry = { selector: string; offset: number };

export class SettingsScroll {
  scrollEl: HTMLElement | undefined = $state();

  /**
   * Capture the topmost field/group anchor before a layout-shifting change,
   * run the change, then re-pin the same anchor to its previous viewport
   * offset every frame for the full animation window. The fallback chain
   * (field → section → group) ensures we don't lose the anchor when the
   * original element gets unmounted (e.g. an advanced field is hidden).
   */
  withAnchor(fn: () => void): void {
    if (!this.scrollEl) {
      fn();
      return;
    }

    const scrollTop = this.scrollEl.scrollTop;
    const candidates = this.scrollEl.querySelectorAll<HTMLElement>(
      "[data-field-id], [data-group-id]",
    );
    let topAnchor: HTMLElement | null = null;
    for (const el of candidates) {
      if (el.offsetTop >= scrollTop) {
        topAnchor = el;
        break;
      }
    }

    const chain: AnchorEntry[] = [];
    if (topAnchor) {
      const offset = topAnchor.offsetTop - scrollTop;
      const fid = topAnchor.dataset.fieldId;
      const gid = topAnchor.dataset.groupId;
      if (fid) {
        chain.push({
          selector: `[data-field-id="${CSS.escape(fid)}"]`,
          offset,
        });
        // Fallback 1: the enclosing section. Pinned at offset 0 so its top
        // edge (with the sticky section header) lands at the viewport top,
        // keeps the user "in the same section" when only their specific
        // field has been hidden.
        const sectionAncestor = topAnchor.closest<HTMLElement>("[data-section-key]");
        if (sectionAncestor?.dataset.sectionKey) {
          chain.push({
            selector: `[data-section-key="${CSS.escape(sectionAncestor.dataset.sectionKey)}"]`,
            offset: 0,
          });
        }
        // Fallback 2: the enclosing group. Used when the entire section is
        // also gone (whole section marked advanced, or all of its fields
        // are advanced).
        const groupAncestor = topAnchor.closest<HTMLElement>("[data-group-id]");
        if (groupAncestor?.dataset.groupId) {
          chain.push({
            selector: `[data-group-id="${CSS.escape(groupAncestor.dataset.groupId)}"]`,
            offset: 0,
          });
        }
      } else if (gid) {
        chain.push({
          selector: `[data-group-id="${CSS.escape(gid)}"]`,
          offset,
        });
      }
    }

    fn();

    if (chain.length === 0) return;

    // Run a buffered window: full animation duration + 100ms grace so the
    // post-transition ResizeObserver tick (and any horizontal-mode flip it
    // triggers) have time to land.
    const deadline = performance.now() + getDuration(BASE_MS) + 100;

    const step = () => {
      if (!this.scrollEl) return;
      for (const entry of chain) {
        const el = this.scrollEl.querySelector<HTMLElement>(entry.selector);
        if (!el) continue;
        const target = Math.max(0, el.offsetTop - entry.offset);
        // Skip the assignment when we're already there to avoid spurious
        // scroll events stealing focus from a user-initiated scroll.
        if (Math.abs(this.scrollEl.scrollTop - target) > 0.5) {
          this.scrollEl.scrollTop = target;
        }
        break;
      }
      if (performance.now() < deadline) {
        requestAnimationFrame(step);
      }
    };

    requestAnimationFrame(step);
  }
}

export function useSettingsScroll(): SettingsScroll {
  return new SettingsScroll();
}

// The CoreBar's status pill is three different controls depending on the state:
// a plain label, an expand toggle (when the status carries specifics), or the
// way out of a settled failure. The last one is what "Disconnected" and
// "Stopped" add, and it must never be confused with the expand toggle: clicking
// Disconnected has to retry, not open an empty detail card.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import CoreBarView from "@tomat/shared/ui/components/chat/CoreBarView.svelte";

const CORES = [{ id: "laptop", name: "Laptop" }];

function pill(container: HTMLElement): HTMLElement {
  const el = container.querySelector<HTMLElement>('[data-region="core-status"]');
  if (!el) throw new Error("no status pill rendered");
  return el;
}

describe("CoreBarView status pill", () => {
  it.each(["disconnected", "stopped"] as const)("%s renders an actionable button", (status) => {
    const onStatusAction = vi.fn();
    const { container } = render(CoreBarView, {
      props: { status, cores: CORES, currentCoreId: "laptop", onStatusAction },
    });
    const el = pill(container);
    expect(el.tagName).toBe("BUTTON");
    // Not an expander: no chevron, and no aria-expanded to mislead a screen reader.
    expect(el.getAttribute("aria-expanded")).toBeNull();
    expect(el.querySelector(".i-material-symbols-chevron-right-rounded")).toBeNull();
  });

  it("clicking Disconnected fires the recovery action, not the expander", async () => {
    const onStatusAction = vi.fn();
    const onToggleExpand = vi.fn();
    const { container } = render(CoreBarView, {
      props: {
        status: "disconnected",
        cores: CORES,
        currentCoreId: "laptop",
        onStatusAction,
        onToggleExpand,
      },
    });
    await fireEvent.click(pill(container));
    expect(onStatusAction).toHaveBeenCalledOnce();
    expect(onToggleExpand).not.toHaveBeenCalled();
  });

  it("Stopped names the recovery in its tooltip", () => {
    const { container } = render(CoreBarView, {
      props: { status: "stopped", cores: CORES, currentCoreId: "laptop", onStatusAction: vi.fn() },
    });
    expect(pill(container).getAttribute("title")).toBe("Start Core");
    expect(container.textContent).toContain("Stopped");
  });

  // The transient states resolve on their own, so they carry no action.
  it.each(["idle", "connecting", "reconnecting", "unauthorized"] as const)(
    "%s stays a plain, non-interactive pill",
    (status) => {
      const { container } = render(CoreBarView, {
        props: { status, cores: CORES, currentCoreId: "laptop", onStatusAction: vi.fn() },
      });
      expect(pill(container).tagName).toBe("SPAN");
    },
  );

  // An expandable status keeps expanding: the detail card wins over the (absent)
  // recovery action, and aria-expanded still describes it.
  it("error with subsystems still toggles the detail card", async () => {
    const onToggleExpand = vi.fn();
    const onStatusAction = vi.fn();
    const { container } = render(CoreBarView, {
      props: {
        status: "error",
        subsystems: [{ kind: "llama", status: "Error", message: "out of memory" }],
        cores: CORES,
        currentCoreId: "laptop",
        onToggleExpand,
        onStatusAction,
      },
    });
    const el = pill(container);
    expect(el.tagName).toBe("BUTTON");
    expect(el.getAttribute("aria-expanded")).toBe("false");
    await fireEvent.click(el);
    expect(onToggleExpand).toHaveBeenCalledOnce();
    expect(onStatusAction).not.toHaveBeenCalled();
  });
});

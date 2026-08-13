// smoke-test the Svelte 5 component path. Toggle is the smallest
// stateful leaf; if this test runs, the runes/@testing-library/svelte/jsdom
// stack is wired correctly. Most UI primitives don't get dedicated tests
// (too low ROI). This one exists to lock in the test harness.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import Toggle from "@loca/shared/ui/components/primitives/Toggle.svelte";

describe("Toggle", () => {
  it("renders with the initial checked state", () => {
    const { container } = render(Toggle, { props: { checked: true } });
    const input = container.querySelector<HTMLInputElement>("input[type=checkbox]");
    expect(input).toBeTruthy();
    expect(input?.checked).toBe(true);
  });

  it("calls onchange with the new value when the input is toggled", async () => {
    const onchange = vi.fn();
    const { container } = render(Toggle, {
      props: { checked: false, onchange },
    });
    const input = container.querySelector<HTMLInputElement>("input[type=checkbox]")!;
    await fireEvent.click(input);
    expect(onchange).toHaveBeenCalledWith(true);
  });

  it("renders an aria-label when ariaLabel prop is provided", () => {
    const { container } = render(Toggle, {
      props: { checked: false, ariaLabel: "enable-tts" },
    });
    expect(container.querySelector("input[aria-label='enable-tts']")).toBeTruthy();
  });

  it("disables the input when disabled prop is true", () => {
    const { container } = render(Toggle, {
      props: { checked: false, disabled: true },
    });
    const input = container.querySelector<HTMLInputElement>("input[type=checkbox]");
    expect(input?.disabled).toBe(true);
  });

  it("slides the knob clip to the selected side", async () => {
    const { container, rerender } = render(Toggle, {
      props: { checked: false },
    });
    const knob = container.querySelector<HTMLElement>("[aria-hidden=true]")!;
    const offClip = knob.style.clipPath;
    expect(offClip).toContain("inset(");
    expect(knob.style.transition).toContain("clip-path");
    await rerender({ checked: true });
    expect(knob.style.clipPath).not.toBe(offClip);
  });

  it("renders a radiogroup and reports the picked value in segmented mode", async () => {
    const onselect = vi.fn();
    const { container, getByRole } = render(Toggle, {
      props: {
        value: "ask",
        options: [
          { value: "denied", label: "Deny" },
          { value: "ask", label: "Ask" },
          { value: "granted", label: "Allow" },
        ],
        onselect,
      },
    });
    expect(container.querySelector("[role=radiogroup]")).toBeTruthy();
    // The selected option is the checked radio.
    const ask = container.querySelector<HTMLButtonElement>("[role=radio][aria-checked=true]");
    expect(ask?.textContent?.trim()).toBe("Ask");
    // getByRole skips the aria-hidden knob overlay, which repeats each label.
    await fireEvent.click(getByRole("radio", { name: "Allow" }));
    expect(onselect).toHaveBeenCalledWith("granted");
  });
});

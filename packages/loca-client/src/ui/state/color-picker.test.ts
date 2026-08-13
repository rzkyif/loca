// ColorPickerState: open/close + apply-callback semantics.

import { afterEach, describe, expect, it, vi } from "vitest";
import { colorPickerState } from "./color-picker.svelte";

afterEach(() => {
  colorPickerState.close();
});

describe("colorPickerState", () => {
  it("starts with no pending request", () => {
    expect(colorPickerState.pending).toBeNull();
  });

  it("open() stores the request, close() clears it", () => {
    const anchor = {} as HTMLElement;
    colorPickerState.open({
      anchor,
      initialColor: "oklch(0.7 0.1 250 / 1)",
      onApply: () => {},
    });
    expect(colorPickerState.pending?.initialColor).toBe("oklch(0.7 0.1 250 / 1)");
    colorPickerState.close();
    expect(colorPickerState.pending).toBeNull();
  });

  it("apply(color) clears pending and invokes the onApply callback", () => {
    const onApply = vi.fn();
    colorPickerState.open({
      anchor: {} as HTMLElement,
      initialColor: "oklch(0 0 0 / 1)",
      onApply,
    });
    colorPickerState.apply("oklch(0.2 0.05 250 / 1)");
    expect(onApply).toHaveBeenCalledWith("oklch(0.2 0.05 250 / 1)");
    expect(colorPickerState.pending).toBeNull();
  });

  it("apply() with no pending request is a no-op", () => {
    expect(() => colorPickerState.apply("#ffffffff")).not.toThrow();
    expect(colorPickerState.pending).toBeNull();
  });
});

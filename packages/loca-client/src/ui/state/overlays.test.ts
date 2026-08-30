// Overlay store semantics: confirm push/pull + alert(), color picker open/apply.

import { afterEach, describe, expect, it, vi } from "vitest";
import { colorPickerState, confirmState } from "./overlays.svelte";

afterEach(() => {
  // Drain any pending request between tests so cross-test pollution can't
  // mask bugs in the next test.
  if (confirmState.pending) confirmState.cancel();
  colorPickerState.close();
});

describe("confirmState", () => {
  it("starts with no pending request", () => {
    expect(confirmState.pending).toBeNull();
  });

  it("request() pushes onto pending and confirm() runs the callback", async () => {
    const onConfirm = vi.fn();
    confirmState.request({
      title: "Delete?",
      message: "are you sure",
      onConfirm,
    });
    expect(confirmState.pending?.title).toBe("Delete?");
    await confirmState.confirm();
    expect(onConfirm).toHaveBeenCalledOnce();
    expect(confirmState.pending).toBeNull();
  });

  it("cancel() clears pending and runs the optional onCancel", () => {
    const onCancel = vi.fn();
    confirmState.request({
      title: "x",
      message: "y",
      onConfirm: () => {},
      onCancel,
    });
    confirmState.cancel();
    expect(onCancel).toHaveBeenCalledOnce();
    expect(confirmState.pending).toBeNull();
  });

  it("alert() pushes a single-button notice", () => {
    confirmState.alert({ title: "Oops", message: "broke" });
    expect(confirmState.pending?.alert).toBe(true);
    expect(confirmState.pending?.confirmLabel).toBe("OK");
  });

  it("alert() honors a custom confirmLabel", () => {
    confirmState.alert({ title: "x", message: "y", confirmLabel: "Got it" });
    expect(confirmState.pending?.confirmLabel).toBe("Got it");
  });

  it("confirm() awaits an async callback before clearing pending", async () => {
    let resolved = false;
    confirmState.request({
      title: "x",
      message: "y",
      onConfirm: async () => {
        await new Promise<void>((r) => setTimeout(r, 5));
        resolved = true;
      },
    });
    await confirmState.confirm();
    expect(resolved).toBe(true);
    expect(confirmState.pending).toBeNull();
  });
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

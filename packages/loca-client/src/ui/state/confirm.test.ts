// ConfirmState push/pull semantics + the alert() one-button shortcut.

import { afterEach, describe, expect, it, vi } from "vitest";
import { confirmState } from "./confirm.svelte";

afterEach(() => {
  // Drain any pending request between tests so cross-test pollution can't
  // mask bugs in the next test.
  if (confirmState.pending) confirmState.cancel();
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

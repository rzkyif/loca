// The extracted settings field-change engine. Exercises the semantics both
// Settings and Quick Settings depend on: validation gating the optimistic
// apply, the preset-to-Custom flip for managed keys, and optionalWhen
// re-evaluation through condition deps.
//
// settingsState is the real singleton: no core is paired under vitest, so
// flushes only write the stub's client settings (localStorage) and the
// core PATCH path is skipped entirely.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { getDefaultSettings } from "@loca/shared";
import { installPlatformStub } from "../test/platform-stub.ts";
import { settingsState } from "$stores/settings.svelte";
import { useSettingsForm } from "./use-settings-form.svelte";

// The app installs a platform at boot; flushes write client settings through
// it, so give the tests the localStorage-backed stub.
installPlatformStub();

beforeEach(() => {
  // deno-lint-ignore no-explicit-any
  settingsState.currentSettings = getDefaultSettings() as Record<string, any>;
});

describe("SettingsForm", () => {
  it("applies a valid change optimistically", async () => {
    const form = useSettingsForm();
    await form.handleChange("appearance.textSize", 20);
    expect(form.validationErrors["appearance.textSize"]).toBeUndefined();
    expect(settingsState.currentSettings["appearance.textSize"]).toBe(20);
  });

  it("blocks an empty required value and records the error", async () => {
    const form = useSettingsForm();
    await form.handleChange("llm.external.model", "");
    expect(form.validationErrors["llm.external.model"]).toBe("This field is required");
  });

  it("blocks a regex-invalid value and leaves the setting untouched", async () => {
    const form = useSettingsForm();
    await form.handleChange("appearance.textSize", 50);
    expect(form.validationErrors["appearance.textSize"]).toBe("Must be between 12 and 32");
    expect(settingsState.currentSettings["appearance.textSize"]).toBe(16);
  });

  it("flips llm.preset to custom when a managed key changes", async () => {
    const form = useSettingsForm();
    expect(settingsState.currentSettings["llm.preset"]).toBe("smallest");
    await form.handleChange("llm.contextSize", 8192);
    expect(settingsState.currentSettings["llm.preset"]).toBe("custom");
    expect(settingsState.currentSettings["llm.contextSize"]).toBe(8192);
  });

  it("does not flip llm.preset for llm.reasoning (not a managed key)", async () => {
    // Quick Settings renders llm.reasoning standalone; changing it must not
    // silently switch the user off their smart preset.
    const form = useSettingsForm();
    await form.handleChange("llm.reasoning", "off");
    expect(settingsState.currentSettings["llm.preset"]).toBe("smallest");
    expect(settingsState.currentSettings["llm.reasoning"]).toBe("off");
  });

  it("re-validates optionalWhen dependents when their controller changes", async () => {
    const form = useSettingsForm();
    // An empty vision file is an error while image support is on (off by default).
    await settingsState.updateSetting("llm.supportImages", true);
    await settingsState.updateSetting("llm.mmprojPath", "");
    form.validateField("llm.mmprojPath", "");
    expect(form.validationErrors["llm.mmprojPath"]).toBe("This field is required");
    // ...and clears via reEvaluateDeps once image support turns off.
    await form.handleChange("llm.supportImages", false);
    expect(form.validationErrors["llm.mmprojPath"]).toBeUndefined();
  });

  it("never fires onSectionExpand under the current schema (no expandWhen)", async () => {
    const onSectionExpand = vi.fn();
    const form = useSettingsForm(onSectionExpand);
    await form.handleChange("llm.supportImages", false);
    await form.handleChange("tools.enabled", true);
    expect(onSectionExpand).not.toHaveBeenCalled();
  });
});

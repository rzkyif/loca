// SettingsSearch is the small state machine that drives the search-mode
// slide-swap. Animation timing is irrelevant for this layer; we only care
// that mode toggling, debounced onInput, and clear() do the right thing.

import { describe, expect, it } from "vitest";
import { SettingsSearch, useSettingsSearch } from "./use-settings-search.svelte";

describe("SettingsSearch", () => {
  it("starts inactive with an empty query", () => {
    const s = new SettingsSearch();
    expect(s.mode).toBe(false);
    expect(s.query).toBe("");
  });

  it("setMode(active) flips mode synchronously when no layer is attached", async () => {
    const s = new SettingsSearch();
    await s.setMode(true);
    expect(s.mode).toBe(true);
    await s.setMode(false);
    expect(s.mode).toBe(false);
  });

  it("setMode is idempotent: same target is a no-op", async () => {
    const s = new SettingsSearch();
    await s.setMode(true);
    await s.setMode(true);
    expect(s.mode).toBe(true);
  });

  it("onInput() enters search mode when query is non-empty whitespace-trimmed", async () => {
    const s = new SettingsSearch();
    s.query = "  hi  ";
    s.onInput();
    // setMode is async (returns a Promise), but with no layerEl it resolves
    // on the same microtask.
    await Promise.resolve();
    expect(s.mode).toBe(true);
  });

  it("onInput() exits search mode when query is whitespace-only", async () => {
    const s = new SettingsSearch();
    s.mode = true;
    s.query = "   ";
    s.onInput();
    await Promise.resolve();
    expect(s.mode).toBe(false);
  });

  it("clear() resets query and exits search mode", async () => {
    const s = new SettingsSearch();
    s.query = "hello";
    s.mode = true;
    s.clear();
    await Promise.resolve();
    expect(s.query).toBe("");
    expect(s.mode).toBe(false);
  });

  it("useSettingsSearch() returns a fresh independent instance", () => {
    const a = useSettingsSearch();
    const b = useSettingsSearch();
    a.mode = true;
    expect(b.mode).toBe(false);
  });
});

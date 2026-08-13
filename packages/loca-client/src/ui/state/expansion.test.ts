// expansion state: the small per-message map that bubbles read/write
// to decide whether they break the horizontal stack.

import { beforeEach, describe, expect, it } from "vitest";
import { expansionState, isExpanded } from "./expansion.svelte";

describe("expansionState + isExpanded", () => {
  beforeEach(() => {
    expansionState.clear();
  });

  it("isExpanded returns false for undefined id", () => {
    expect(isExpanded(undefined)).toBe(false);
  });

  it("isExpanded reflects the last set value for that id", () => {
    expansionState.set("m1", true);
    expect(isExpanded("m1")).toBe(true);
    expansionState.set("m1", false);
    expect(isExpanded("m1")).toBe(false);
  });

  it("isExpanded falls back to false for unknown ids", () => {
    expect(isExpanded("never-set")).toBe(false);
  });

  it("multiple ids are tracked independently", () => {
    expansionState.set("a", true);
    expansionState.set("b", false);
    expect(isExpanded("a")).toBe(true);
    expect(isExpanded("b")).toBe(false);
  });
});

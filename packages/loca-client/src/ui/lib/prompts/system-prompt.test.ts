// renderContextTemplate + applySystemPromptOverride are pure functions of
// their inputs. The higher-level builders (buildSystemPrompt /
// buildContextBlock) read settingsState and are covered by component flows.

import { describe, expect, it } from "vitest";
import { applySystemPromptOverride, renderContextTemplate } from "./system-prompt";

describe("renderContextTemplate", () => {
  it("substitutes simple {name} placeholders", () => {
    expect(renderContextTemplate("Hi {who}.", { who: "loca" })).toBe("Hi loca.");
  });

  it("missing variable substitutes empty string", () => {
    expect(renderContextTemplate("Hi {who}.", {})).toBe("Hi .");
  });

  it("keeps [name:body] when the var is non-empty", () => {
    expect(renderContextTemplate("[loc:Location: {loc}]", { loc: "Tokyo" })).toBe(
      "Location: Tokyo",
    );
  });

  it("drops [name:body] when the var is empty or missing", () => {
    expect(renderContextTemplate("Before[loc:!]After", { loc: "" })).toBe("BeforeAfter");
    expect(renderContextTemplate("X[loc:!]Y", {})).toBe("XY");
  });

  it("collapses 2+ blank lines into one (trimmed)", () => {
    expect(renderContextTemplate("a\n\n\n\nb", {})).toBe("a\n\nb");
  });

  it("conditional bodies may contain their own placeholder", () => {
    expect(renderContextTemplate("[user:Hello {user}]", { user: "Riz" })).toBe("Hello Riz");
  });
});

describe("applySystemPromptOverride", () => {
  const base = "BASE";
  const ctx = "CTX";

  it("returns null when nothing to render", () => {
    expect(applySystemPromptOverride("", undefined, "")).toBeNull();
  });

  it("returns base + context when no override", () => {
    expect(applySystemPromptOverride(base, undefined, ctx)).toBe("BASE\n\nCTX");
  });

  it("prepend goes before base, append after, replace swaps base", () => {
    const out = applySystemPromptOverride(
      base,
      {
        prepend: "PRE",
        append: "POST",
      },
      ctx,
    );
    expect(out).toBe("PRE\n\nBASE\n\nPOST\n\nCTX");
  });

  it("replace=='' suppresses the base entirely", () => {
    // `replace !== undefined` means "use replace, even if empty".
    const out = applySystemPromptOverride(base, { replace: "" }, ctx);
    // Empty replace removes the base; prepend/append/context still apply.
    expect(out).toBe("CTX");
  });

  it("replace wins over base", () => {
    const out = applySystemPromptOverride(base, { replace: "NEW" }, ctx);
    expect(out).toBe("NEW\n\nCTX");
  });

  it("prepend + replace + append + context: order is preserved", () => {
    const out = applySystemPromptOverride(
      base,
      {
        prepend: "A",
        replace: "B",
        append: "C",
      },
      "D",
    );
    expect(out).toBe("A\n\nB\n\nC\n\nD");
  });
});

// snippet expansion. Pure transform that drives the #/@// trigger substitution
// + placement semantics that snake through the UI's input pipeline.

import { describe, expect, it } from "vitest";
import {
  applySnippets,
  normalizeName,
  recommendedSymbol,
  type Snippet,
  type SnippetPlacement,
  type SnippetSymbol,
  snippetTrigger,
  validateName,
} from "./snippets";

// `trigger` is a full display trigger like "@greet" or "/run"; split it into
// the symbol + bare name the Snippet shape stores.
const snippet = (
  trigger: string,
  text: string,
  placement: SnippetPlacement = "insert-user",
): Snippet => ({
  id: `id-${trigger}`,
  name: trigger.slice(1),
  symbol: trigger[0] as SnippetSymbol,
  symbolPinned: false,
  placement,
  text,
});

describe("normalizeName", () => {
  it("strips leading symbols and internal whitespace", () => {
    expect(normalizeName("foo")).toBe("foo");
    expect(normalizeName("@foo bar")).toBe("foobar");
    expect(normalizeName("@@@x")).toBe("x");
    expect(normalizeName("/run")).toBe("run");
  });
  it("returns empty string for symbol/whitespace-only input", () => {
    expect(normalizeName("   ")).toBe("");
    expect(normalizeName("@")).toBe("");
  });
});

describe("recommendedSymbol", () => {
  it("maps placement to a default symbol", () => {
    expect(recommendedSymbol("insert-user")).toBe("#");
    expect(recommendedSymbol("replace-user")).toBe("/");
    expect(recommendedSymbol("prepend-user")).toBe("/");
    expect(recommendedSymbol("append-system")).toBe("@");
  });
});

describe("validateName", () => {
  it("requires a non-empty alphanumeric+_- name", () => {
    expect(validateName("@", "", [])).toMatch(/required/);
    expect(validateName("@", "!", [])).toMatch(/letters/);
    expect(validateName("@", "hello-world_1", [])).toBeNull();
  });
  it("rejects duplicate triggers against the existing-list", () => {
    expect(validateName("@", "foo", ["@foo"])).toMatch(/already used/);
    expect(validateName("@", "bar", ["@foo"])).toBeNull();
    // Same name, different symbol is a distinct trigger.
    expect(validateName("/", "foo", ["@foo"])).toBeNull();
  });
});

describe("snippetTrigger", () => {
  it("joins symbol and name", () => {
    expect(snippetTrigger({ symbol: "/", name: "run" })).toBe("/run");
  });
});

describe("applySnippets", () => {
  it("returns raw text unchanged when no snippets are defined", () => {
    expect(applySnippets("hello @foo", [])).toEqual({ userText: "hello @foo" });
  });

  it("leaves unknown @triggers in place", () => {
    expect(applySnippets("hello @unknown world", [snippet("@known", "X")])).toEqual({
      userText: "hello @unknown world",
    });
  });

  it("does NOT match @ inside identifiers (email@domain)", () => {
    const out = applySnippets("contact email@foo", [snippet("@foo", "replaced")]);
    expect(out.userText).toBe("contact email@foo");
  });

  it("insert-user substitutes inline", () => {
    const out = applySnippets("hi @greet world", [snippet("@greet", "hello")]);
    expect(out.userText).toBe("hi hello world");
  });

  it("expands `/` and `#` triggers too", () => {
    const out = applySnippets("go /run now", [snippet("/run", "DO IT", "replace-user")]);
    expect(out.userText).toBe("DO IT");
    const tag = applySnippets("a #sig b", [snippet("#sig", "~me", "insert-user")]);
    expect(tag.userText).toBe("a ~me b");
  });

  it("prepend-user and append-user join with blank lines", () => {
    const out = applySnippets("middle @intro @outro", [
      snippet("@intro", "INTRO", "prepend-user"),
      snippet("@outro", "OUTRO", "append-user"),
    ]);
    // Triggers themselves disappear; middle text is preserved + sandwiched.
    expect(out.userText).toBe("INTRO\n\nmiddle\n\nOUTRO");
  });

  it("preserves the body's leading indentation when there are no affixes", () => {
    const out = applySnippets("  indented @greet code", [snippet("@greet", "hello")]);
    // insert-user only: no prepend/append, so the body is kept verbatim.
    expect(out.userText).toBe("  indented hello code");
  });

  it("keeps the body's far-edge whitespace, trimming only the affix seam", () => {
    const out = applySnippets("  indented code\n@sig", [
      snippet("@sig", "SIGNATURE", "append-user"),
    ]);
    // Leading indentation survives; only the seam before the appended part is
    // collapsed to the blank-line join.
    expect(out.userText).toBe("  indented code\n\nSIGNATURE");
  });

  it("replace-user wins over inline / prepend / append", () => {
    const out = applySnippets("ignored @drop", [snippet("@drop", "EVERYTHING", "replace-user")]);
    expect(out.userText).toBe("EVERYTHING");
  });

  it("system-prompt placements only populate systemOverride", () => {
    const out = applySnippets("hello @sys", [snippet("@sys", "you are a cat", "prepend-system")]);
    // @sys disappears from the user text; the system override carries it.
    expect(out.userText).toBe("hello");
    expect(out.systemOverride).toEqual({ prepend: "you are a cat" });
  });

  it("replace-system / prepend-system / append-system can co-exist", () => {
    const out = applySnippets("@a @b @c text", [
      snippet("@a", "A", "prepend-system"),
      snippet("@b", "B", "replace-system"),
      snippet("@c", "C", "append-system"),
    ]);
    expect(out.systemOverride).toEqual({
      prepend: "A",
      replace: "B",
      append: "C",
    });
  });

  it("is case-insensitive on the trigger lookup", () => {
    const out = applySnippets("hi @GREET there", [snippet("@greet", "hello")]);
    expect(out.userText).toBe("hi hello there");
  });
});

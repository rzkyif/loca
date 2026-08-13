// Autocomplete: exercises the token-before-caret detection, the trigger
// replacement math, and the key-nav wrap-around. The caret-anchor measurement
// and the consumer-owned $effect wiring are covered by the running app; here we
// drive the methods directly with a real (jsdom) textarea for selectionStart
// and an empty mirror span (so measureCaretAt short-circuits to {0,0}).

import { afterEach, describe, expect, it } from "vitest";
import { Autocomplete, collectExistingTriggers, useAutocomplete } from "./use-autocomplete.svelte";

function setup(text: string, caret: number) {
  const ta = document.createElement("textarea");
  const mirror = document.createElement("span");
  document.body.append(ta, mirror);
  ta.value = text;
  ta.setSelectionRange(caret, caret);
  const ac = new Autocomplete();
  ac.bind(
    () => ta,
    () => mirror,
  );
  return { ac, ta };
}

afterEach(() => {
  document.body.innerHTML = "";
});

function key(name: string): KeyboardEvent {
  return new KeyboardEvent("keydown", { key: name, cancelable: true });
}

describe("Autocomplete.updateFromInput", () => {
  it("opens with the bare token being typed before the caret", () => {
    const { ac } = setup("hello @fo", 9);
    ac.updateFromInput("hello @fo");
    expect(ac.open).toBe(true);
    expect(ac.prefix).toBe("@fo");
    expect(ac.triggerStart).toBe(6);
    expect(ac.triggerEnd).toBe(9);
  });

  it("captures an open quoted memory token", () => {
    const src = 'see @"My Do';
    const { ac } = setup(src, src.length);
    ac.updateFromInput(src);
    expect(ac.open).toBe(true);
    expect(ac.prefix).toBe('@"My Do');
  });

  it("keeps the dropdown open while a dotted/slashed name is typed", () => {
    const src = "ref @ext/skills/file-bug";
    const { ac } = setup(src, src.length);
    ac.updateFromInput(src);
    expect(ac.open).toBe(true);
    expect(ac.prefix).toBe("@ext/skills/file-bug");
  });

  it("stays closed when there is no trigger before the caret", () => {
    const { ac } = setup("just text", 9);
    ac.updateFromInput("just text");
    expect(ac.open).toBe(false);
  });

  it("does not open mid-IME-composition", () => {
    const { ac } = setup("hello @fo", 9);
    ac.onCompositionStart();
    ac.updateFromInput("hello @fo");
    expect(ac.open).toBe(false);
  });

  it("resets the index only when the prefix changes", () => {
    const { ac, ta } = setup("@fo", 3);
    ac.updateFromInput("@fo");
    ac.index = 2;
    // Same caret + token: re-running should NOT bounce the highlight back.
    ac.updateFromInput("@fo");
    expect(ac.index).toBe(2);
    // Extend the token: prefix changes, index resets.
    ta.value = "@foo";
    ta.setSelectionRange(4, 4);
    ac.updateFromInput("@foo");
    expect(ac.index).toBe(0);
  });
});

describe("Autocomplete.applyTrigger", () => {
  it("replaces the in-progress token and returns the caret offset", () => {
    const { ac } = setup("hi @fo bye", 6);
    ac.updateFromInput("hi @fo bye");
    const out = ac.applyTrigger("hi @fo bye", "@foo");
    expect(out).toEqual({ text: "hi @foo  bye", caret: 8 });
    expect(ac.open).toBe(false);
  });

  it("returns null and closes when there is no active token", () => {
    const ac = new Autocomplete();
    expect(ac.applyTrigger("nothing", "@foo")).toBeNull();
    expect(ac.open).toBe(false);
  });
});

describe("Autocomplete.handleKey", () => {
  const options = [
    { id: "a", name: "A", trigger: "@a", source: "snippet" as const },
    { id: "b", name: "B", trigger: "@b", source: "snippet" as const },
  ];

  it("wraps the selection on ArrowDown / ArrowUp", () => {
    const ac = useAutocomplete();
    ac.open = true;
    ac.index = 0;
    expect(ac.handleKey(key("ArrowDown"), options, () => {})).toBe(true);
    expect(ac.index).toBe(1);
    ac.handleKey(key("ArrowDown"), options, () => {});
    expect(ac.index).toBe(0); // wrapped
    ac.handleKey(key("ArrowUp"), options, () => {});
    expect(ac.index).toBe(1); // wrapped backwards
  });

  it("commits the highlighted option on Enter and Tab", () => {
    const ac = useAutocomplete();
    ac.open = true;
    ac.index = 1;
    let chosen: string | null = null;
    expect(ac.handleKey(key("Enter"), options, (o) => (chosen = o.trigger))).toBe(true);
    expect(chosen).toBe("@b");
    chosen = null;
    ac.handleKey(key("Tab"), options, (o) => (chosen = o.trigger));
    expect(chosen).toBe("@b");
  });

  it("closes on Escape", () => {
    const ac = useAutocomplete();
    ac.open = true;
    expect(ac.handleKey(key("Escape"), options, () => {})).toBe(true);
    expect(ac.open).toBe(false);
  });

  it("does not consume keys when closed or with no options", () => {
    const ac = useAutocomplete();
    ac.open = false;
    expect(ac.handleKey(key("ArrowDown"), options, () => {})).toBe(false);
    ac.open = true;
    expect(ac.handleKey(key("ArrowDown"), [], () => {})).toBe(false);
  });
});

describe("Autocomplete.clampIndex", () => {
  it("closes when the option list empties", () => {
    const ac = new Autocomplete();
    ac.open = true;
    ac.clampIndex(0);
    expect(ac.open).toBe(false);
  });

  it("resets an out-of-range index", () => {
    const ac = new Autocomplete();
    ac.open = true;
    ac.index = 5;
    ac.clampIndex(3);
    expect(ac.index).toBe(0);
  });

  it("is a no-op while closed", () => {
    const ac = new Autocomplete();
    ac.open = false;
    ac.index = 9;
    ac.clampIndex(0);
    expect(ac.index).toBe(9);
  });
});

describe("collectExistingTriggers", () => {
  it("collects bare and quoted tokens, lowercased", () => {
    const out = collectExistingTriggers('use @Foo and @"My Doc" here', -1, -1);
    expect([...out].sort()).toEqual(['@"my doc"', "@foo"]);
  });

  it("collects bare names with dots and slashes, dropping trailing punctuation", () => {
    const out = collectExistingTriggers("see @ext/skills/file-bug. and @node.js!", -1, -1);
    expect([...out].sort()).toEqual(["@ext/skills/file-bug", "@node.js"]);
  });

  it("excludes the token spanning [excludeStart, excludeEnd)", () => {
    // "a @foo @bar" -> exclude the @bar token (chars 7..11).
    const source = "a @foo @bar";
    expect([...collectExistingTriggers(source, 7, 11)]).toEqual(["@foo"]);
  });

  it("does not match @ inside identifiers (email@domain)", () => {
    expect(collectExistingTriggers("mail me at me@host.com", -1, -1).size).toBe(0);
  });

  it("keeps a token that only partially overlaps the exclude range", () => {
    // @foo spans [0,4); a range touching its end still keeps it.
    expect([...collectExistingTriggers("@foo", 2, 4)]).toEqual([]);
    expect([...collectExistingTriggers("@foo", 4, 6)]).toEqual(["@foo"]);
  });
});

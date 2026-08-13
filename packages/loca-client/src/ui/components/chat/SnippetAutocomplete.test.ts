// SnippetAutocomplete renders options as a listbox and dispatches
// `onSelect` when a row is mousedown'd. Mousedown (not click) is the
// production path so the parent textarea keeps focus.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import SnippetAutocomplete from "./SnippetAutocomplete.svelte";
import type { AutocompleteOption } from "$composables/use-autocomplete.svelte";

const snippet = (id: string, trigger: string, name = id): AutocompleteOption => ({
  id,
  name,
  trigger,
  source: "snippet",
});

describe("SnippetAutocomplete", () => {
  it("renders nothing when options is empty", () => {
    const { container } = render(SnippetAutocomplete, {
      props: {
        options: [],
        selectedIndex: 0,
        anchor: { top: 0, left: 0 },
        onSelect: () => {},
      },
    });
    expect(container.querySelector("[role=listbox]")).toBeNull();
  });

  it("renders a row per option with trigger + name", () => {
    const opts = [snippet("a", "@foo", "Foo"), snippet("b", "@bar", "Bar")];
    const { container } = render(SnippetAutocomplete, {
      props: {
        options: opts,
        selectedIndex: 0,
        anchor: { top: 50, left: 100 },
        onSelect: () => {},
      },
    });
    const rows = container.querySelectorAll("[role=option]");
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain("Foo");
    expect(rows[0].textContent).toContain("@foo");
    expect(rows[1].textContent).toContain("Bar");
    expect(rows[1].textContent).toContain("@bar");
  });

  it("marks the selectedIndex row with aria-selected", () => {
    const { container } = render(SnippetAutocomplete, {
      props: {
        options: [snippet("a", "@x"), snippet("b", "@y")],
        selectedIndex: 1,
        anchor: { top: 0, left: 0 },
        onSelect: () => {},
      },
    });
    const rows = container.querySelectorAll<HTMLElement>("[role=option]");
    expect(rows[0].getAttribute("aria-selected")).toBe("false");
    expect(rows[1].getAttribute("aria-selected")).toBe("true");
  });

  it("mousedown on a row dispatches onSelect with that snippet", async () => {
    const onSelect = vi.fn();
    const opts = [snippet("a", "@x"), snippet("b", "@y")];
    const { container } = render(SnippetAutocomplete, {
      props: {
        options: opts,
        selectedIndex: 0,
        anchor: { top: 0, left: 0 },
        onSelect,
      },
    });
    // ListItem renders the mousedown handler on the inner button (so focus
    // semantics work out for the parent textarea); query that.
    const buttons = container.querySelectorAll<HTMLElement>("[role=option] button");
    await fireEvent.mouseDown(buttons[1]);
    expect(onSelect).toHaveBeenCalledWith(opts[1]);
  });
});

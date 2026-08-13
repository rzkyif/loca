// SnippetsState: directory-backed snippets against an in-memory platform
// mock. One file per snippet (filename stem = id); the directory listing is
// the registry, so load() is also the "rescan" used to pick up files the
// user copied in by hand.

import { beforeEach, describe, expect, it } from "vitest";
import { type Platform, setPlatform } from "$lib/platform";
import type { Snippet, SnippetSymbol } from "$lib/snippets/snippets";
import { snippetsState } from "./snippets.svelte";

function makeMockPlatform(): {
  platform: Platform;
  files: Record<string, Record<string, unknown>>;
} {
  const files: Record<string, Record<string, unknown>> = {};
  const platform = {
    snippetFiles: {
      async readAll() {
        return JSON.parse(JSON.stringify(files));
      },
      async write(name: string, data: Record<string, unknown>) {
        files[name] = JSON.parse(JSON.stringify(data));
      },
      async delete(name: string) {
        delete files[name];
      },
    },
  } as unknown as Platform;
  return { platform, files };
}

const snippet = (id: string, symbol: SnippetSymbol, name: string, text: string): Snippet => ({
  id,
  name,
  symbol,
  symbolPinned: false,
  placement: "insert-user",
  text,
});

beforeEach(() => {
  // Reset the singleton between tests.
  snippetsState.snippets = [];
});

describe("snippetsState", () => {
  it("load() lists the snippets directory, keyed and ordered by filename stem", async () => {
    const { platform, files } = makeMockPlatform();
    files["b-snippet"] = {
      name: "B",
      symbol: "@",
      placement: "insert-user",
      text: "bee",
    };
    files["a-snippet"] = {
      name: "A",
      symbol: "@",
      placement: "insert-user",
      text: "ay",
    };
    setPlatform(platform);

    await snippetsState.load();
    expect(snippetsState.snippets.map((s) => s.id)).toEqual(["a-snippet", "b-snippet"]);
    expect(snippetsState.snippets[0].name).toBe("A");
    expect(snippetsState.snippets[0].symbol).toBe("@");
  });

  it("load() coerces shared files: the filename wins, bad fields default, symbol follows placement", async () => {
    const { platform, files } = makeMockPlatform();
    files["shared"] = { id: "other-id", placement: "bogus" };
    setPlatform(platform);

    await snippetsState.load();
    const s = snippetsState.snippets[0];
    expect(s.id).toBe("shared");
    expect(s.name).toBe("shared");
    expect(s.placement).toBe("append-system");
    // append-system recommends the `@` symbol.
    expect(s.symbol).toBe("@");
    expect(s.text).toBe("");
  });

  it("load() infers symbolPinned from a symbol that differs from the recommendation", async () => {
    const { platform, files } = makeMockPlatform();
    // insert-user recommends `#`; a stored `@` means the user picked it.
    files["picked"] = { name: "p", symbol: "@", placement: "insert-user", text: "" };
    // append-system recommends `@`; matching it with no flag is "not pinned".
    files["default"] = { name: "d", symbol: "@", placement: "append-system", text: "" };
    // An explicit flag always wins over the inference.
    files["explicit"] = {
      name: "e",
      symbol: "@",
      symbolPinned: true,
      placement: "append-system",
      text: "",
    };
    setPlatform(platform);

    await snippetsState.load();
    const byId = Object.fromEntries(snippetsState.snippets.map((s) => [s.id, s.symbolPinned]));
    expect(byId["picked"]).toBe(true);
    expect(byId["default"]).toBe(false);
    expect(byId["explicit"]).toBe(true);
  });

  it("create() derives a slug filename from the name and uniquifies it", async () => {
    const { platform, files } = makeMockPlatform();
    setPlatform(platform);

    const a = await snippetsState.create({
      name: "New snippet",
      symbol: "@",
      symbolPinned: false,
      placement: "insert-user",
      text: "",
    });
    const b = await snippetsState.create({
      name: "New snippet",
      symbol: "@",
      symbolPinned: false,
      placement: "insert-user",
      text: "",
    });
    expect(a.id).toBe("new-snippet");
    expect(b.id).toBe("new-snippet-2");
    expect(Object.keys(files).sort()).toEqual(["new-snippet", "new-snippet-2"]);
    // The body never carries the id; the filename is the identity.
    expect("id" in files["new-snippet"]).toBe(false);
  });

  it("save() and delete() round-trip a snippet file", async () => {
    const { platform, files } = makeMockPlatform();
    setPlatform(platform);

    await snippetsState.save(snippet("greet", "@", "hi", "hello"));
    expect(files["greet"]).toEqual({
      name: "hi",
      symbol: "@",
      symbolPinned: false,
      placement: "insert-user",
      text: "hello",
    });
    expect(snippetsState.findByTrigger("@HI")?.id).toBe("greet");

    await snippetsState.delete("greet");
    expect(files["greet"]).toBeUndefined();
    expect(snippetsState.snippets).toEqual([]);
  });
});

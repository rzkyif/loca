import { describe, expect, it } from "vitest";
import { parseQuery, setSortToken, toggleFilterToken } from "./query.ts";

describe("parseQuery", () => {
  it("splits free text from filter and sort tokens", () => {
    const q = parseQuery("@installed @sort:name cat food");
    expect(q.text).toBe("cat food");
    expect([...q.filters]).toEqual(["installed"]);
    expect(q.sort).toBe("name");
  });

  it("returns empty parts for an empty query", () => {
    const q = parseQuery("   ");
    expect(q.text).toBe("");
    expect(q.filters.size).toBe(0);
    expect(q.sort).toBeNull();
  });

  it("collects multiple filters and keeps the last sort", () => {
    const q = parseQuery("@installed @npm @sort:name @sort:downloads x");
    expect([...q.filters].sort()).toEqual(["installed", "npm"]);
    expect(q.sort).toBe("downloads");
    expect(q.text).toBe("x");
  });

  it("treats a bare @sort: (no value) as no sort", () => {
    expect(parseQuery("@sort:").sort).toBeNull();
  });

  it("treats a lone @ as free text", () => {
    const q = parseQuery("@ hi");
    expect(q.text).toBe("@ hi");
    expect(q.filters.size).toBe(0);
  });
});

describe("toggleFilterToken", () => {
  it("adds a filter to the right of the last token, left of free text", () => {
    expect(toggleFilterToken("cat", "installed")).toBe("@installed cat");
    expect(toggleFilterToken("@npm cat", "installed")).toBe("@npm @installed cat");
  });

  it("removes the filter when already present", () => {
    expect(toggleFilterToken("@installed cat", "installed")).toBe("cat");
    expect(toggleFilterToken("@npm @installed cat", "installed")).toBe("@npm cat");
  });

  it("inserts before free text when there are no tokens", () => {
    expect(toggleFilterToken("hello world", "npm")).toBe("@npm hello world");
  });

  it("inserts after a sort token", () => {
    expect(toggleFilterToken("@sort:name cat", "installed")).toBe("@sort:name @installed cat");
  });
});

describe("setSortToken", () => {
  it("inserts the sort token after the last filter, before free text", () => {
    expect(setSortToken("@installed cat", "name")).toBe("@installed @sort:name cat");
  });

  it("replaces an existing sort token in place", () => {
    expect(setSortToken("@installed @sort:name cat", "downloads")).toBe(
      "@installed @sort:downloads cat",
    );
  });

  it("clears the sort when re-selecting the active one", () => {
    expect(setSortToken("@installed @sort:name cat", "name")).toBe("@installed cat");
  });

  it("inserts a sort when none exists and there is no other token", () => {
    expect(setSortToken("cat", "name")).toBe("@sort:name cat");
  });
});

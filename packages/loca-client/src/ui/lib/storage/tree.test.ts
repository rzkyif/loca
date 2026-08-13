// Pure tree algorithms for the Storage settings field. Drives every non-trivial
// transformation (collectPaths / findNode / expandToFiles / visibleRows /
// subtreeLockReason / clearableNodes) against in-memory category fixtures.

import { describe, expect, it } from "vitest";
import {
  categoryKey,
  clearableNodes,
  collectPaths,
  expandToFiles,
  findNode,
  type StorageCategory,
  type StorageNode,
  type StorageTree,
  subtreeLockReason,
  visibleRows,
} from "./tree";

function file(name: string, path: string, size = 1, lock?: string): StorageNode {
  return lock
    ? { kind: "file", name, path, size, lock_reason: lock }
    : { kind: "file", name, path, size };
}

function folder(name: string, path: string, children: StorageNode[], lock?: string): StorageNode {
  const size = children.reduce((a, c) => a + c.size, 0);
  return lock
    ? { kind: "folder", name, path, size, children, lock_reason: lock }
    : { kind: "folder", name, path, size, children };
}

function category(id: string, nodes: StorageNode[]): StorageCategory {
  return {
    id: id as StorageCategory["id"],
    label: id,
    deletable: true,
    nodes,
    size: 0,
  };
}

const repo = folder("u/r", "/root/models/u/r", [
  file("m.gguf", "/root/models/u/r/m.gguf", 10),
  file("mm.gguf", "/root/models/u/r/mm.gguf", 5),
]);

const TREE: StorageTree = {
  categories: [
    category("models", [repo]),
    category("sessions", [folder("Chat", "/root/sessions/s1", [])]),
  ],
  total_size: 15,
  root_path: "/root",
};

describe("collectPaths", () => {
  it("walks a folder and adds every descendant path + the folder itself", () => {
    const out = new Set<string>();
    collectPaths(repo, out);
    expect([...out].sort()).toEqual([
      "/root/models/u/r",
      "/root/models/u/r/m.gguf",
      "/root/models/u/r/mm.gguf",
    ]);
  });
});

describe("findNode", () => {
  it("finds a nested file by path", () => {
    expect(findNode(TREE, "/root/models/u/r/m.gguf")?.name).toBe("m.gguf");
  });
  it("finds a session folder", () => {
    expect(findNode(TREE, "/root/sessions/s1")?.kind).toBe("folder");
  });
  it("returns null for unknown paths", () => {
    expect(findNode(TREE, "/no/such/path")).toBeNull();
  });
});

describe("expandToFiles", () => {
  it("a folder with file children expands to those files", () => {
    expect(expandToFiles(TREE, ["/root/models/u/r"]).sort()).toEqual([
      "/root/models/u/r/m.gguf",
      "/root/models/u/r/mm.gguf",
    ]);
  });
  it("a childless folder (session/summary dir) deletes as a whole", () => {
    expect(expandToFiles(TREE, ["/root/sessions/s1"])).toEqual(["/root/sessions/s1"]);
  });
  it("unknown paths are silently dropped", () => {
    expect(expandToFiles(TREE, ["/no/such"])).toEqual([]);
  });
});

describe("visibleRows", () => {
  it("renders nothing when no groups are expanded", () => {
    expect(visibleRows(TREE, new Set())).toEqual([]);
  });
  it("expanding a category surfaces its top-level nodes", () => {
    const rows = visibleRows(TREE, new Set([categoryKey("models")]));
    expect(rows.map((r) => r.path)).toEqual(["/root/models/u/r"]);
  });
  it("expanding a folder under an expanded category surfaces its children", () => {
    const rows = visibleRows(TREE, new Set([categoryKey("models"), "/root/models/u/r"]));
    expect(rows.map((r) => r.path)).toEqual([
      "/root/models/u/r",
      "/root/models/u/r/m.gguf",
      "/root/models/u/r/mm.gguf",
    ]);
  });
});

describe("subtreeLockReason / clearableNodes", () => {
  it("a folder is locked when a descendant is locked", () => {
    const locked = category("models", [
      folder("u/r", "/m/u/r", [
        file("a.gguf", "/m/u/r/a.gguf", 1, "Required by current settings"),
        file("b.gguf", "/m/u/r/b.gguf", 1),
      ]),
    ]);
    expect(subtreeLockReason(locked.nodes[0])).toContain("Required");
    expect(clearableNodes(locked)).toEqual([]);
  });
  it("an unlocked node is clearable", () => {
    const cat = category("models", [file("x.gguf", "/m/x.gguf", 1)]);
    expect(clearableNodes(cat).length).toBe(1);
  });
});

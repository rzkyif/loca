// pure attachment helpers: base64 round-trip, filename hygiene, and
// MIME -> extension mapping. writeSessionAttachment needs the platform() seam
// and is integration-tested by the running app; readSessionAttachment is
// covered here because WHICH transport it uses is a correctness question.

import { describe, expect, it, vi } from "vitest";
import {
  base64ToUtf8,
  classifyAttachment,
  collectAttachmentPaths,
  diffRemovedAttachmentPaths,
  ensureMarkdownExtension,
  imageExtFromMime,
  readSessionAttachment,
  toPreviewParts,
  utf8ToBase64,
} from "./attachments";
import { cores, CoreClient } from "$lib/core";
import { localTransport } from "$lib/core/local-transport";
import { type Platform, setPlatform } from "$lib/platform";
import { fakeLocalCore } from "../../test/local-core-fake";
import type { MessagePart } from "$lib/util/types";

describe("utf8ToBase64 + base64ToUtf8", () => {
  it("round-trips ASCII", () => {
    expect(base64ToUtf8(utf8ToBase64("hello"))).toBe("hello");
  });

  it("round-trips multi-byte unicode", () => {
    expect(base64ToUtf8(utf8ToBase64("日本語 + 🍅"))).toBe("日本語 + 🍅");
  });

  it("round-trips the empty string", () => {
    expect(base64ToUtf8(utf8ToBase64(""))).toBe("");
  });
});

describe("imageExtFromMime", () => {
  it("maps known MIME types", () => {
    expect(imageExtFromMime("image/png")).toBe("png");
    expect(imageExtFromMime("image/jpeg")).toBe("jpg");
    expect(imageExtFromMime("image/gif")).toBe("gif");
    expect(imageExtFromMime("image/webp")).toBe("webp");
    expect(imageExtFromMime("image/svg+xml")).toBe("svg");
  });

  it("is case-insensitive", () => {
    expect(imageExtFromMime("IMAGE/PNG")).toBe("png");
  });

  it("falls back to png on unknown MIME", () => {
    expect(imageExtFromMime("application/octet-stream")).toBe("png");
  });
});

describe("ensureMarkdownExtension", () => {
  it("appends .md when missing", () => {
    expect(ensureMarkdownExtension("notes")).toBe("notes.md");
    expect(ensureMarkdownExtension("doc.txt")).toBe("doc.txt.md");
  });

  it("is idempotent for .md files (case-insensitive)", () => {
    expect(ensureMarkdownExtension("notes.md")).toBe("notes.md");
    expect(ensureMarkdownExtension("README.MD")).toBe("README.MD");
  });
});

describe("collectAttachmentPaths", () => {
  it("returns empty for string content", () => {
    expect(collectAttachmentPaths("hello").size).toBe(0);
  });

  it("collects image_file + document_file paths only", () => {
    const out = collectAttachmentPaths([
      { type: "text", text: "hi" },
      {
        type: "image_file",
        path: "/a.png",
        filename: "a.png",
        mime: "image/png",
      },
      { type: "document_file", path: "/b.md", filename: "b.md" },
      { type: "image_url", image_url: { url: "https://x" } },
      { type: "document", filename: "c.md", markdown: "inline" },
    ] as MessagePart[]);
    expect([...out].sort()).toEqual(["/a.png", "/b.md"]);
  });
});

describe("diffRemovedAttachmentPaths", () => {
  it("returns paths removed between two contents", () => {
    const prev: MessagePart[] = [
      {
        type: "image_file",
        path: "/a.png",
        filename: "a.png",
        mime: "image/png",
      },
      { type: "document_file", path: "/b.md", filename: "b.md" },
    ];
    const next: MessagePart[] = [
      {
        type: "image_file",
        path: "/a.png",
        filename: "a.png",
        mime: "image/png",
      },
    ];
    expect(diffRemovedAttachmentPaths(prev, next)).toEqual(["/b.md"]);
  });

  it("empty diff when nothing changed", () => {
    const same: MessagePart[] = [
      { type: "image_file", path: "/x", filename: "x", mime: "image/png" },
    ];
    expect(diffRemovedAttachmentPaths(same, same)).toEqual([]);
  });
});

describe("classifyAttachment", () => {
  it("classifies an image by extension when images are supported", () => {
    expect(classifyAttachment("photo.png", "", true)).toEqual({
      kind: "image",
      ext: "png",
    });
  });

  it("classifies an image by MIME when the extension is unknown", () => {
    // No image extension, but an image/* type: normalize ext to png.
    expect(classifyAttachment("clip", "image/jpeg", true)).toEqual({
      kind: "image",
      ext: "png",
    });
  });

  it("treats an image as a document when images are unsupported", () => {
    // png is not in DOC_EXTENSIONS, so it is rejected rather than imported.
    expect(classifyAttachment("photo.png", "image/png", false)).toBeNull();
  });

  it("classifies a known document extension", () => {
    expect(classifyAttachment("notes.md", "", true)).toEqual({
      kind: "document",
    });
    expect(classifyAttachment("data.csv", "text/csv", false)).toEqual({
      kind: "document",
    });
  });

  it("returns null for an unknown type", () => {
    expect(classifyAttachment("archive.zip", "application/zip", true)).toBeNull();
  });

  it("is case-insensitive on the extension", () => {
    expect(classifyAttachment("PHOTO.PNG", "", true)).toEqual({
      kind: "image",
      ext: "png",
    });
  });
});

describe("toPreviewParts", () => {
  it("filters to the previewable subset", () => {
    const input: MessagePart[] = [
      { type: "text", text: "hi" },
      { type: "image_file", path: "/a", filename: "a", mime: "image/png" },
      { type: "image_url", image_url: { url: "https://x" } },
      { type: "document", filename: "doc.md", markdown: "doc" },
      { type: "document_file", path: "/b.md", filename: "b.md" },
    ];
    const out = toPreviewParts(input);
    expect(out.length).toBe(4);
    // Text part is excluded.
    expect(out.find((p) => p.type === "text")).toBeUndefined();
  });

  it("returns [] for string content", () => {
    expect(toPreviewParts("plain text")).toEqual([]);
  });
});

describe("readSessionAttachment", () => {
  // The stored path is an absolute `<baseUrl>/api/v1/...` URL, so this is one of
  // the few sites that fetches outside CoreClient's path-based helpers. It still
  // has to go through the SELECTED core's transport: when that core is the one
  // inside this app, its base URL is a sentinel with nothing behind it, so a
  // plain network fetch would fail for every attachment.
  it("fetches through the selected client's transport, not the network layer", async () => {
    const core = fakeLocalCore();
    setPlatform({
      localCore: core,
      net: {
        fetch: () => Promise.reject(new Error("the network must not be reached")),
      },
    } as unknown as Platform);

    const client = new CoreClient(
      { baseUrl: "https://local.loca.invalid", token: "", trustMode: "webpki" },
      undefined,
      localTransport(),
    );
    vi.spyOn(cores(), "currentClient").mockReturnValue(client);
    core.reply({ status: 200, bodyB64: btoa("FILE") });

    const b64 = await readSessionAttachment(
      "https://local.loca.invalid/api/v1/sessions/s1/attachments/a1",
    );
    expect(atob(b64)).toBe("FILE");
    expect(core.requests.at(-1)?.url).toBe("/api/v1/sessions/s1/attachments/a1");
  });
});

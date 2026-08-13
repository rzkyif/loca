// tiny session-domain helpers. `isMultipart` is a type guard, but
// `contentToText` has interesting behavior worth pinning down. Every
// caller that needs a flat representation of (possibly multipart)
// MessageContent funnels through it.

import { assertEquals } from "@std/assert";
import { contentToText, isMultipart } from "./session.ts";

Deno.test("isMultipart: returns true for an array, false for a string", () => {
  assertEquals(isMultipart("plain"), false);
  assertEquals(isMultipart([]), true);
  assertEquals(isMultipart([{ type: "text", text: "x" }]), true);
});

Deno.test("contentToText: string content passes through unchanged", () => {
  assertEquals(contentToText("hello"), "hello");
});

Deno.test("contentToText: empty array yields the empty string", () => {
  assertEquals(contentToText([]), "");
});

Deno.test("contentToText: joins text parts in order, drops every other kind", () => {
  const out = contentToText([
    { type: "text", text: "hello " },
    { type: "image_file", filename: "a.png", path: "/a", mime: "image/png" },
    { type: "text", text: "world" },
    { type: "document_file", filename: "b.md", path: "/b" },
    { type: "image_url", image_url: { url: "https://x" } },
  ]);
  assertEquals(out, "hello world");
});

Deno.test("contentToText: only-non-text parts produce empty string", () => {
  const out = contentToText([
    { type: "image_file", filename: "a", path: "/a", mime: "image/png" },
    { type: "document", filename: "b", markdown: "..." },
  ]);
  assertEquals(out, "");
});

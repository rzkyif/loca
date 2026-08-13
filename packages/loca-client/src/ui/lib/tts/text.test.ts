// TTS text sanitization. Pure helpers, no DOM.

import { describe, expect, it } from "vitest";
import { stripEmojisForTTS, stripMarkdownForTTS } from "./text";

describe("stripMarkdownForTTS", () => {
  it("drops fenced code blocks but keeps surrounding sentences", () => {
    const out = stripMarkdownForTTS("First.\n```\nprint('x')\n```\nSecond.");
    expect(out).not.toContain("print");
    expect(out).toContain("First");
    expect(out).toContain("Second");
  });

  it("truncates at an unterminated fence so partial code never leaks", () => {
    expect(stripMarkdownForTTS("First.\n```\npartial mid-stream")).toBe("First.");
  });

  it("keeps link label text and drops the URL", () => {
    const out = stripMarkdownForTTS("See [Anthropic](https://example.com) docs.");
    expect(out).toContain("Anthropic");
    expect(out).not.toContain("https://");
    expect(out).not.toContain("example.com");
  });

  it("drops standalone autolinks and bare URLs", () => {
    expect(stripMarkdownForTTS("hi <https://example.com> bye")).toBe("hi bye");
    expect(stripMarkdownForTTS("go to https://example.com/foo now")).toBe("go to now");
  });

  it("strips bold and italic markers while preserving content", () => {
    expect(stripMarkdownForTTS("**bold** and *italic* and _under_")).toBe(
      "bold and italic and under",
    );
  });

  it("preserves snake_case identifiers despite underscore-italic syntax", () => {
    expect(stripMarkdownForTTS("call snake_case_name here")).toBe("call snake_case_name here");
  });

  it("drops heading hashes but keeps the heading text and inserts a sentence break", () => {
    const out = stripMarkdownForTTS("# Title\nBody.");
    expect(out).toContain("Title");
    expect(out).toContain("Body");
  });

  it("flattens table rows into comma-separated lists and drops separator rows", () => {
    const md = ["| A | B |", "|---|---|", "| 1 | 2 |"].join("\n");
    const out = stripMarkdownForTTS(md);
    expect(out).toContain("A, B");
    expect(out).toContain("1, 2");
    expect(out).not.toContain("---");
  });

  it("returns empty string for empty input", () => {
    expect(stripMarkdownForTTS("")).toBe("");
  });
});

describe("stripEmojisForTTS", () => {
  it("removes single-codepoint emoji", () => {
    expect(stripEmojisForTTS("hello 😀 world")).toBe("hello world");
  });

  it("removes ZWJ-joined emoji sequences in one pass", () => {
    expect(stripEmojisForTTS("family 👨‍👩‍👧 today")).toBe("family today");
  });

  it("removes variation-selector emoji (U+FE0F)", () => {
    expect(stripEmojisForTTS("love ❤️ you")).toBe("love you");
  });

  it("leaves emoji-free text alone", () => {
    expect(stripEmojisForTTS("plain ascii words")).toBe("plain ascii words");
  });
});

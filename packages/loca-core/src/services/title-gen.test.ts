// title sanitizer. Pure transform over LLM output. Covers the bits
// that matter most for UI hygiene (length cap, `<think>` strip, prefix
// normalization).

import { assertEquals } from "@std/assert";
import {
  __sanitizeForTesting as sanitize,
  __selectTitleContextForTesting as selectContext,
} from "@loca/core-engine/services/title-gen";

// A template just large enough to exercise the placeholders; overhead is tiny
// so the context window is effectively the message budget in these tests.
const TEMPLATE = "Title this.\nFirst:\n{firstMessage}\nRecent:\n{recentMessages}";
const msg = (role: "user" | "assistant", content: string) => ({
  role,
  content,
});

Deno.test("sanitize: strips <think>...</think> blocks the model may leak", () => {
  assertEquals(sanitize("<think>hmm</think>Quick Recipe"), "Quick Recipe");
});

Deno.test("sanitize: drops everything after the first line", () => {
  assertEquals(sanitize("Title One\nIgnored second line"), "Title One");
});

Deno.test("sanitize: strips a 'Title:' / 'title -' prefix the model re-emits", () => {
  assertEquals(sanitize("Title: Hello World"), "Hello World");
  assertEquals(sanitize("title - Hello World"), "Hello World");
  assertEquals(sanitize("TITLE: Hello"), "Hello");
});

Deno.test("sanitize: strips surrounding quotes and trailing punctuation", () => {
  assertEquals(sanitize(`"Hello"`), "Hello");
  assertEquals(sanitize("'Hello'"), "Hello");
  assertEquals(sanitize("Hello!"), "Hello");
  assertEquals(sanitize("Hello..."), "Hello");
});

Deno.test("sanitize: caps length at 80 chars", () => {
  const long = "a".repeat(200);
  assertEquals(sanitize(long).length, 80);
});

Deno.test("sanitize: collapses runs of whitespace", () => {
  assertEquals(sanitize("Hello   World\t\t!"), "Hello World");
});

Deno.test("sanitize: returns empty string when input collapses to nothing", () => {
  assertEquals(sanitize(""), "");
  assertEquals(sanitize("<think>only</think>"), "");
  assertEquals(sanitize(`"  "`), "");
});

Deno.test("selectContext: first message is always the opening turn", () => {
  const { firstMessage } = selectContext(
    [msg("user", "hello world"), msg("assistant", "hi there")],
    TEMPLATE,
    100_000,
    0,
  );
  assertEquals(firstMessage, "hello world");
});

Deno.test("selectContext: recent transcript reads chronologically, excluding the opener", () => {
  const { firstMessage, recentMessages } = selectContext(
    [msg("user", "first"), msg("assistant", "a1"), msg("user", "u2"), msg("assistant", "a2")],
    TEMPLATE,
    100_000,
    0,
  );
  assertEquals(firstMessage, "first");
  assertEquals(recentMessages, "Assistant: a1\nUser: u2\nAssistant: a2");
});

Deno.test("selectContext: tight budget keeps the newest and drops older-but-not-first", () => {
  // contextSize=80 leaves room for the opener plus a single newest line; the
  // next-older line no longer fits and the walk stops (see title-gen budget).
  const { firstMessage, recentMessages } = selectContext(
    [
      msg("user", "AAAA"),
      msg("assistant", "BBBBBBBB"),
      msg("user", "CCCCCCCC"),
      msg("assistant", "DDDD"),
    ],
    TEMPLATE,
    80,
    0,
  );
  assertEquals(firstMessage, "AAAA");
  assertEquals(recentMessages, "Assistant: DDDD");
});

Deno.test("selectContext: single turn does not duplicate the opener into recent", () => {
  const { firstMessage, recentMessages } = selectContext(
    [msg("user", "only"), msg("assistant", "reply")],
    TEMPLATE,
    100_000,
    0,
  );
  assertEquals(firstMessage, "only");
  assertEquals(recentMessages, "Assistant: reply");
});

Deno.test("selectContext: no conversational turns yields empty fields", () => {
  assertEquals(selectContext([], TEMPLATE, 100_000, 0), {
    firstMessage: "",
    recentMessages: "",
  });
});

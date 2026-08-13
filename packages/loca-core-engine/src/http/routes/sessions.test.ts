import { assertEquals } from "@std/assert";
import { sanitizeFilename } from "./sessions.ts";

Deno.test("sanitizeFilename: strips path separators and shell glyphs", () => {
  assertEquals(sanitizeFilename("foo/bar"), "foo_bar");
  assertEquals(sanitizeFilename("..\\foo"), "_foo");
  assertEquals(sanitizeFilename('a:b*c?d"e<f>g|h'), "a_b_c_d_e_f_g_h");
});

Deno.test("sanitizeFilename: strips control chars", () => {
  assertEquals(sanitizeFilename("foo\x00bar\x1fbaz"), "foo_bar_baz");
});

Deno.test("sanitizeFilename: strips leading dots (hidden) and trailing dots/spaces (Windows)", () => {
  assertEquals(sanitizeFilename("...secret.txt"), "secret.txt");
  assertEquals(sanitizeFilename("evil.txt."), "evil.txt");
  assertEquals(sanitizeFilename("evil.txt   "), "evil.txt");
});

Deno.test("sanitizeFilename: prefixes Windows reserved basenames", () => {
  assertEquals(sanitizeFilename("con"), "_con");
  assertEquals(sanitizeFilename("CON.txt"), "_CON.txt");
  assertEquals(sanitizeFilename("LPT9.log"), "_LPT9.log");
  // Non-reserved names that look similar should pass through unchanged.
  assertEquals(sanitizeFilename("console.log"), "console.log");
});

Deno.test("sanitizeFilename: caps length, preserving short extensions", () => {
  const long = "a".repeat(300) + ".jpg";
  const out = sanitizeFilename(long);
  assertEquals(out.length, 200);
  assertEquals(out.endsWith(".jpg"), true);
  // Long pseudo-extensions are not treated as extensions.
  const weird = "b".repeat(150) + "." + "c".repeat(150);
  assertEquals(sanitizeFilename(weird).length, 200);
});

Deno.test("sanitizeFilename: falls back to 'file' when input collapses to empty", () => {
  assertEquals(sanitizeFilename(""), "file");
  assertEquals(sanitizeFilename("..."), "file");
  // Path separators collapse to underscores rather than disappearing, so a
  // string of pure separators stays non-empty.
  assertEquals(sanitizeFilename("///"), "___");
});

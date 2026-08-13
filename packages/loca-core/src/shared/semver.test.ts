import { assertEquals } from "@std/assert";
import { compareSemver } from "./semver.ts";

Deno.test("compareSemver: equal versions return 0", () => {
  assertEquals(compareSemver("1.2.3", "1.2.3"), 0);
});

Deno.test("compareSemver: orders by major, then minor, then patch", () => {
  assertEquals(compareSemver("1.0.0", "2.0.0"), -1);
  assertEquals(compareSemver("2.0.0", "1.0.0"), 1);
  assertEquals(compareSemver("1.2.0", "1.10.0"), -1);
  assertEquals(compareSemver("1.10.0", "1.2.0"), 1);
  assertEquals(compareSemver("1.2.3", "1.2.10"), -1);
});

Deno.test("compareSemver: ignores prerelease + build metadata", () => {
  assertEquals(compareSemver("1.2.3-alpha", "1.2.3"), 0);
  assertEquals(compareSemver("1.2.3+build.99", "1.2.3"), 0);
});

Deno.test("compareSemver: missing minor/patch defaults to 0", () => {
  assertEquals(compareSemver("1", "1.0.0"), 0);
  assertEquals(compareSemver("1.5", "1.5.0"), 0);
});

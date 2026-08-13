import { assertEquals } from "@std/assert";
import { toolPlatformSupported } from "./extension.ts";

Deno.test("toolPlatformSupported: empty/absent declared list = all platforms", () => {
  assertEquals(toolPlatformSupported(undefined, ["linux", "linux_wayland"]), true);
  assertEquals(toolPlatformSupported([], ["darwin"]), true);
});

Deno.test("toolPlatformSupported: matches when the lists intersect", () => {
  // Generic linux matches any linux host.
  assertEquals(toolPlatformSupported(["linux"], ["linux", "linux_x11"]), true);
  assertEquals(toolPlatformSupported(["linux"], ["linux", "linux_wayland"]), true);
  // Specific display server gates within linux.
  assertEquals(toolPlatformSupported(["linux_x11"], ["linux", "linux_x11"]), true);
  assertEquals(toolPlatformSupported(["linux_x11"], ["linux", "linux_wayland"]), false);
  // Cross-OS.
  assertEquals(toolPlatformSupported(["darwin", "windows"], ["darwin"]), true);
  assertEquals(toolPlatformSupported(["darwin"], ["windows"]), false);
});

import { assertEquals } from "@std/assert";
import { __resetForTesting, hostPlatforms, isWayland, resolvePlatforms } from "./platform.ts";

Deno.test("resolvePlatforms: per-OS tokens", () => {
  assertEquals(resolvePlatforms("darwin", false), ["darwin"]);
  assertEquals(resolvePlatforms("windows", true), ["windows"]);
  assertEquals(resolvePlatforms("linux", false), ["linux", "linux_x11"]);
  assertEquals(resolvePlatforms("linux", true), ["linux", "linux_wayland"]);
});

Deno.test("isWayland: WAYLAND_DISPLAY or XDG_SESSION_TYPE", () => {
  const wd = Deno.env.get("WAYLAND_DISPLAY");
  const st = Deno.env.get("XDG_SESSION_TYPE");
  try {
    Deno.env.delete("WAYLAND_DISPLAY");
    Deno.env.delete("XDG_SESSION_TYPE");
    assertEquals(isWayland(), false);

    Deno.env.set("WAYLAND_DISPLAY", "wayland-0");
    assertEquals(isWayland(), true);

    Deno.env.delete("WAYLAND_DISPLAY");
    Deno.env.set("XDG_SESSION_TYPE", "wayland");
    assertEquals(isWayland(), true);

    Deno.env.set("XDG_SESSION_TYPE", "x11");
    assertEquals(isWayland(), false);
  } finally {
    if (wd === undefined) Deno.env.delete("WAYLAND_DISPLAY");
    else Deno.env.set("WAYLAND_DISPLAY", wd);
    if (st === undefined) Deno.env.delete("XDG_SESSION_TYPE");
    else Deno.env.set("XDG_SESSION_TYPE", st);
  }
});

Deno.test("hostPlatforms: memoizes and includes the base OS token", () => {
  __resetForTesting();
  const first = hostPlatforms();
  assertEquals(first, hostPlatforms()); // same memoized array
  assertEquals(
    first.includes(
      Deno.build.os === "windows" ? "windows" : Deno.build.os === "darwin" ? "darwin" : "linux",
    ),
    true,
  );
});

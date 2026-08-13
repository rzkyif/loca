// Guards the deb prerm / rpm %preun body against the data-loss-on-upgrade bug:
// the teardown that runs `uninstall-service` must fire ONLY on a genuine
// removal, never on upgrade. dpkg calls prerm with "remove" vs "upgrade"; rpm
// calls %preun with 0 (final erase) vs 1 (upgrade). Also pins dpkg's
// remove-vs-purge split: `apt remove` keeps the per-user data
// (uninstall-service --keep-data) and only `dpkg -P` (postrm "purge") wipes it.

import { assertEquals, assertStringIncludes } from "@std/assert";

import { linuxPostrmBody, linuxPreunBody } from "./core-installers.ts";

Deno.test("linuxPreunBody only tears down on removal, not upgrade", () => {
  const body = linuxPreunBody("stable", "/usr/lib/tomat-core", "tomat-core");
  // The removal-only guard: dpkg "remove" and rpm 0 both match; "upgrade"/1 fall
  // through with no branch, so the wipe never runs on a version bump.
  assertStringIncludes(body, 'case "$1" in');
  assertStringIncludes(body, "remove|0)");
  assertStringIncludes(body, "uninstall-service");
  // The wipe is inside the guard, not at the top level.
  const beforeCase = body.slice(0, body.indexOf('case "$1" in'));
  assertEquals(beforeCase.includes("uninstall-service"), false);
});

Deno.test("linuxPreunBody keepData (deb prerm) stops the service but keeps data", () => {
  const body = linuxPreunBody("stable", "/usr/lib/tomat-core", "tomat-core", { keepData: true });
  assertStringIncludes(body, "uninstall-service --keep-data");
  // rpm's %preun (no keepData) keeps the full teardown: its erase IS the purge.
  const rpmBody = linuxPreunBody("stable", "/usr/lib/tomat-core", "tomat-core");
  assertEquals(rpmBody.includes("--keep-data"), false);
});

Deno.test("linuxPostrmBody wipes the channel core dir only on purge", () => {
  const body = linuxPostrmBody("latest");
  assertStringIncludes(body, 'case "$1" in');
  assertStringIncludes(body, "purge)");
  assertStringIncludes(body, '.tomat/latest/core"');
  // The wipe is inside the purge branch, and the shared models dir is untouched.
  const beforeCase = body.slice(0, body.indexOf('case "$1" in'));
  assertEquals(beforeCase.includes("rm -rf"), false);
  assertEquals(body.includes(".tomat/models"), false);
});

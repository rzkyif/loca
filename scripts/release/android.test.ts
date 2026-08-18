// Release item: android. See android.ts.
//
// These cover the two halves of F-J, both of which need no APK build: the
// filename-to-ABI-key mapping, and the fact that the fragment list in
// `ABI_KEYS` and the `include(...)` list in the gradle splits block are two
// copies of one thing with nothing comparing them.

import { assert, assertEquals } from "@std/assert";
import { abiKeyFor } from "./android.ts";

const GRADLE = "packages/loca-client/src/tauri/gen/android/app/build.gradle.kts";

Deno.test("android: each gradle split filename maps to its own manifest key", () => {
  // The names AGP produces for an ABI split, and the universal one alongside
  // them. If gradle ever changes this shape, `abiKeyFor` falls through to
  // `android-universal` for every split and the duplicate check in
  // `composeAndUploadAndroid` fails the release rather than publishing an APK
  // under a key that does not describe it.
  const mapped = [
    "app-arm64-v8a-release.apk",
    "app-armeabi-v7a-release.apk",
    "app-x86_64-release.apk",
    "app-x86-release.apk",
    "app-universal-release.apk",
  ].map(abiKeyFor);

  assertEquals(mapped, [
    "android-arm64",
    "android-armv7",
    "android-x86_64",
    "android-x86",
    "android-universal",
  ]);
  // The property that matters more than any single row: no two split
  // filenames collide, because a collision is a race for one R2 object.
  assertEquals(new Set(mapped).size, mapped.length, "two filenames share one key");
});

Deno.test("android: x86_64 is not read as x86, which substring order decides", () => {
  // `ABI_KEYS` is ordered and `includes` is a substring test, so listing "x86"
  // before "x86_64" would silently map the 64-bit APK to the 32-bit key. This
  // is the one row where the ordering of that list is load-bearing.
  assertEquals(abiKeyFor("app-x86_64-release.apk"), "android-x86_64");
});

Deno.test("android: the gradle split list and ABI_KEYS name the same ABIs", () => {
  // Two copies of one list, in two languages, with nothing else comparing them.
  // A rung added to one and not the other produces APKs that publish under
  // `android-universal` (see abiKeyFor's fall-through) rather than an error.
  const gradle = Deno.readTextFileSync(GRADLE);
  // Scope to the splits block first. A bare /include\(/ matches
  // `proguardFiles { include("**/*.pro") }` higher up the file, which is how the
  // first version of this test failed against a correct tree.
  const block = /splits\s*\{[\s\S]*?abi\s*\{([\s\S]*?)\n\s*\}/.exec(gradle);
  assert(block, `no splits { abi { ... } } block found in ${GRADLE}`);
  const include = /include\(([^)]*)\)/.exec(block[1]);
  assert(include, `no include(...) inside the splits block in ${GRADLE}`);
  const fromGradle = [...include[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]).sort();

  const source = Deno.readTextFileSync("scripts/release/android.ts");
  const table = /const ABI_KEYS[^=]*=\s*\[([\s\S]*?)\n\];/.exec(source);
  assert(table, "no ABI_KEYS table found in android.ts");
  const fromTable = [...table[1].matchAll(/\["([^"]+)",/g)].map((m) => m[1]).sort();

  assertEquals(fromGradle, fromTable, "the gradle splits and ABI_KEYS have drifted");
});

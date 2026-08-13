import { assertEquals } from "@std/assert";
import { denoFs } from "./deno-fs.ts";

// Conformance for the DenoHost HostFs adapter. These pin the behaviours the
// engine relies on (atomic writes, stat->null for missing, idempotent remove,
// owner-only permissions) so a mobile HostFs must satisfy the same contract.

async function withTmp(fn: (dir: string) => Promise<void>): Promise<void> {
  const dir = await Deno.makeTempDir({ prefix: "hostfs-test-" });
  try {
    await fn(dir);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
}

Deno.test("writeTextFileAtomic + readTextFile round-trip", async () => {
  await withTmp(async (dir) => {
    const p = `${dir}/a.txt`;
    await denoFs.writeTextFileAtomic(p, "hello");
    assertEquals(await denoFs.readTextFile(p), "hello");
    // A rewrite replaces the content atomically (no leftover .tmp).
    await denoFs.writeTextFileAtomic(p, "world");
    assertEquals(await denoFs.readTextFile(p), "world");
    assertEquals(await denoFs.stat(`${p}.tmp`), null);
  });
});

Deno.test("writeFileAtomic + readFile round-trip (bytes)", async () => {
  await withTmp(async (dir) => {
    const p = `${dir}/b.bin`;
    const bytes = new Uint8Array([0, 1, 2, 255]);
    await denoFs.writeFileAtomic(p, bytes);
    assertEquals(await denoFs.readFile(p), bytes);
  });
});

Deno.test("stat: null for missing, shape for present", async () => {
  await withTmp(async (dir) => {
    assertEquals(await denoFs.stat(`${dir}/nope`), null);
    await denoFs.writeTextFileAtomic(`${dir}/f`, "xy");
    const st = await denoFs.stat(`${dir}/f`);
    assertEquals(st?.isDir, false);
    assertEquals(st?.size, 2);
    assertEquals((await denoFs.stat(dir))?.isDir, true);
  });
});

Deno.test("remove: idempotent (no throw on missing)", async () => {
  await withTmp(async (dir) => {
    await denoFs.remove(`${dir}/ghost`); // must not throw
    await denoFs.writeTextFileAtomic(`${dir}/x`, "1");
    await denoFs.remove(`${dir}/x`);
    assertEquals(await denoFs.stat(`${dir}/x`), null);
  });
});

Deno.test("mkdir recursive + readDir", async () => {
  await withTmp(async (dir) => {
    await denoFs.mkdir(`${dir}/a/b`, { recursive: true });
    await denoFs.writeTextFileAtomic(`${dir}/a/b/f.txt`, "z");
    const entries = await denoFs.readDir(`${dir}/a`);
    assertEquals(
      entries.sort((x, y) => x.name.localeCompare(y.name)),
      [{ name: "b", isDir: true }],
    );
    const inner = await denoFs.readDir(`${dir}/a/b`);
    assertEquals(inner, [{ name: "f.txt", isDir: false }]);
  });
});

Deno.test("rename moves a file", async () => {
  await withTmp(async (dir) => {
    await denoFs.writeTextFileAtomic(`${dir}/from`, "v");
    await denoFs.rename(`${dir}/from`, `${dir}/to`);
    assertEquals(await denoFs.stat(`${dir}/from`), null);
    assertEquals(await denoFs.readTextFile(`${dir}/to`), "v");
  });
});

Deno.test({
  name: "restrictPermissions writes an owner-only (0600) file",
  ignore: Deno.build.os === "windows",
  fn: async () => {
    await withTmp(async (dir) => {
      const p = `${dir}/secret`;
      await denoFs.writeFileAtomic(p, new Uint8Array([1]), { restrictPermissions: true });
      const mode = (await Deno.stat(p)).mode ?? 0;
      assertEquals(mode & 0o777, 0o600);
    });
  },
});

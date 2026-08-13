// .zip extraction: the executable (named `<kind>`) and any shared libraries
// are located wherever they sit in the archive and placed at <dir>/<kind> and
// <dir>/lib/<kind>/<name> (per-kind so same-named libs from different sidecars
// don't collide); unrelated files are ignored.

import { assertEquals } from "@std/assert";
import { join } from "@std/path";
import { BlobWriter, TextReader, ZipWriter } from "@zip.js/zip.js";
import { extractArchive } from "./manager.ts";
import { binaryName } from "./versions.ts";

Deno.test("extractArchive(.zip): places exe at root and libs under lib/<kind>/, ignores extras", async () => {
  const tmp = await Deno.makeTempDir();
  try {
    // Build a zip with the exe + a shared lib in nested dirs, plus a stray file.
    const zw = new ZipWriter(new BlobWriter("application/zip"));
    await zw.add(`bin/${binaryName("llama-server")}`, new TextReader("EXE-BYTES"));
    await zw.add("nested/lib/libllama.dylib", new TextReader("LIB-BYTES"));
    await zw.add("README.md", new TextReader("ignore me"));
    const blob = await zw.close();
    const zipPath = join(tmp, "llama.zip");
    await Deno.writeFile(zipPath, new Uint8Array(await blob.arrayBuffer()));

    const target = join(tmp, "bin");
    await extractArchive(zipPath, target, "llama-server");

    assertEquals(await Deno.readTextFile(join(target, binaryName("llama-server"))), "EXE-BYTES");
    assertEquals(
      await Deno.readTextFile(join(target, "lib", "llama-server", "libllama.dylib")),
      "LIB-BYTES",
    );
    // The stray file is not copied anywhere.
    await assertMissing(join(target, "README.md"));
    await assertMissing(join(target, "lib", "README.md"));
  } finally {
    await Deno.remove(tmp, { recursive: true });
  }
});

Deno.test("extractArchive: per-kind lib dirs keep two sidecars' same-named libs from colliding", async () => {
  const tmp = await Deno.makeTempDir();
  try {
    const target = join(tmp, "bin");
    // Two sidecars can ship a same-named lib (e.g. `ggml-cpu.dll`) with
    // different bytes. Without per-kind lib dirs the second install would
    // clobber the first.
    const cases = [
      ["llama-server", "LLAMA-GGML"],
      ["loca-core-speech", "SPEECH-GGML"],
    ] as const;
    for (const [kind, body] of cases) {
      const zw = new ZipWriter(new BlobWriter("application/zip"));
      await zw.add(binaryName(kind), new TextReader("EXE"));
      await zw.add("ggml-cpu.dll", new TextReader(body));
      const blob = await zw.close();
      const zipPath = join(tmp, `${kind}.zip`);
      await Deno.writeFile(zipPath, new Uint8Array(await blob.arrayBuffer()));
      await extractArchive(zipPath, target, kind);
    }
    assertEquals(
      await Deno.readTextFile(join(target, "lib", "llama-server", "ggml-cpu.dll")),
      "LLAMA-GGML",
    );
    assertEquals(
      await Deno.readTextFile(join(target, "lib", "loca-core-speech", "ggml-cpu.dll")),
      "SPEECH-GGML",
    );
  } finally {
    await Deno.remove(tmp, { recursive: true });
  }
});

Deno.test({
  name: "extractArchive(.tar.gz): finds nested exe + sibling libs, recreates SONAME symlinks, ignores extras",
  ignore: Deno.build.os === "windows", // uses the `tar` CLI + symlinks
  async fn() {
    const tmp = await Deno.makeTempDir();
    try {
      // Realistic llama.cpp layout: everything under a tag dir; exe + real lib
      // as siblings; a SONAME symlink; plus an extra binary + LICENSE to ignore.
      const stage = join(tmp, "stage", "llama-bXX");
      await Deno.mkdir(stage, { recursive: true });
      await Deno.writeTextFile(join(stage, "llama-server"), "EXE");
      await Deno.writeTextFile(join(stage, "libllama.so.0.0.1"), "REALLIB");
      await Deno.symlink("libllama.so.0.0.1", join(stage, "libllama.so.0"));
      await Deno.writeTextFile(join(stage, "llama-cli"), "OTHER-BIN");
      await Deno.writeTextFile(join(stage, "LICENSE"), "license");

      const archive = join(tmp, "llama.tar.gz");
      const { success } = await new Deno.Command("tar", {
        args: ["czf", archive, "-C", join(tmp, "stage"), "llama-bXX"],
      }).output();
      if (!success) throw new Error("tar CLI failed to build the fixture");

      const target = join(tmp, "bin");
      await extractArchive(archive, target, "llama-server");

      assertEquals(await Deno.readTextFile(join(target, "llama-server")), "EXE");
      const lib = join(target, "lib", "llama-server");
      assertEquals(await Deno.readTextFile(join(lib, "libllama.so.0.0.1")), "REALLIB");
      // The SONAME symlink is recreated and resolves to the real lib.
      assertEquals((await Deno.lstat(join(lib, "libllama.so.0"))).isSymlink, true);
      assertEquals(await Deno.readTextFile(join(lib, "libllama.so.0")), "REALLIB");
      // Extra binary + license are not copied anywhere.
      await assertMissing(join(target, "llama-cli"));
      await assertMissing(join(lib, "llama-cli"));
      await assertMissing(join(target, "LICENSE"));
    } finally {
      await Deno.remove(tmp, { recursive: true });
    }
  },
});

async function assertMissing(path: string): Promise<void> {
  let exists = true;
  try {
    await Deno.stat(path);
  } catch {
    exists = false;
  }
  assertEquals(exists, false, `expected ${path} to be absent`);
}

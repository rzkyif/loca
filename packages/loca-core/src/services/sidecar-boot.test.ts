// Gating regression: a configured-but-missing mmproj must keep llama Disabled
// rather than spawn `llama-server --mmproj <missing>`, which would fail the
// whole sidecar boot (not just vision). The mmproj check runs before the binary
// check, so with the model + binary present the ONLY thing that can stop a
// start is the missing mmproj; asserting no "Loading" transition proves the
// guard fired (and that nothing was spawned).

import { assert, assertEquals } from "@std/assert";
import { dirname } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { __resetForTesting, sidecarManager } from "../sidecars/manager.ts";
import type { SidecarSnapshot } from "../sidecars/types.ts";
import { applyLlama } from "./sidecar-boot.ts";
import { resolveHfPath } from "../models/manager.ts";
import { binPath } from "../paths.ts";
import { binaryName } from "../binaries/versions.ts";

async function writeStub(abs: string): Promise<void> {
  await Deno.mkdir(dirname(abs), { recursive: true });
  await Deno.writeTextFile(abs, "stub");
}

Deno.test("applyLlama: a configured-but-missing mmproj keeps llama Disabled (never starts)", async () => {
  const env = await setupTestEnv();
  __resetForTesting();
  try {
    const modelSpec = "@test/llm/main/model.gguf";
    const mmprojSpec = "@test/llm/main/mmproj.gguf";
    // Model present + binary present so the only missing file is mmproj.
    await writeStub(resolveHfPath(modelSpec));
    await writeStub(binPath(binaryName("llama-server")));
    // mmproj deliberately NOT written.

    const states: SidecarSnapshot["status"][] = [];
    sidecarManager().subscribe((s) => {
      if (s.kind === "llama") states.push(s.status);
    });

    await applyLlama({
      "llm.provider": "local",
      "llm.modelPath": modelSpec,
      "llm.supportImages": true,
      "llm.mmprojPath": mmprojSpec,
    });

    assertEquals(sidecarManager().status("llama").status, "Disabled");
    assert(!states.includes("Loading"), "llama must not start with a missing mmproj");
    assert(!states.includes("Running"), "llama must not start with a missing mmproj");
  } finally {
    __resetForTesting();
    await env.teardown();
  }
});

// Hardware detection for the fit engine.
//
// Primary source is the `loca-core-hwinfo` helper binary (RAM + physical cores
// + GPU backend/VRAM). When it is absent or errors, we fall back to Deno's
// built-in memory + concurrency APIs, which cover RAM and cores but not the GPU;
// the fallback assumes Metal/unified on Apple Silicon and CPU elsewhere. Either
// way detection never throws. The result is cached in-process since hardware
// does not change at runtime; pass force to re-probe (used by re-check).

import { binPath } from "../paths.ts";
import { coreBinaryName } from "../binaries/versions.ts";
import { getLogger } from "../shared/log.ts";
import { errMessage, type GpuBackend, type HardwareInfo } from "@loca/shared";

const log = getLogger("hwinfo");

let cached: HardwareInfo | null = null;

export async function detectHardware(force = false): Promise<HardwareInfo> {
  if (cached && !force) return cached;
  cached = (await probeHelper()) ?? fallbackInfo();
  return cached;
}

function helperPath(): string {
  return binPath(coreBinaryName("loca-core-hwinfo"));
}

async function probeHelper(): Promise<HardwareInfo | null> {
  try {
    await Deno.stat(helperPath());
  } catch {
    return null;
  }
  try {
    const { code, stdout, stderr } = await new Deno.Command(helperPath(), {
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
    }).output();
    if (code !== 0) {
      log.warn(`hwinfo exited ${code}: ${new TextDecoder().decode(stderr).trim()}`);
      return null;
    }
    const parsed = JSON.parse(new TextDecoder().decode(stdout)) as HardwareInfo;
    if (!parsed || typeof parsed.totalRamBytes !== "number" || parsed.totalRamBytes <= 0) {
      return null;
    }
    return parsed;
  } catch (err) {
    log.warn(`hwinfo helper spawn failed: ${errMessage(err)}`);
    return null;
  }
}

/** RAM + cores from Deno; GPU inferred from the platform. */
function fallbackInfo(): HardwareInfo {
  const mem = Deno.systemMemoryInfo();
  const isMac = Deno.build.os === "darwin";
  const unified = isMac && Deno.build.arch === "aarch64";
  const backend: GpuBackend = isMac ? "metal" : "cpu";
  return {
    totalRamBytes: mem.total,
    // `available` accounts for reclaimable cache; better than `free` as a budget.
    availableRamBytes: mem.available > 0 ? mem.available : mem.free,
    cpuCoresPhysical: navigator.hardwareConcurrency || 4,
    gpu: {
      backend,
      name: isMac ? "Apple GPU" : "CPU",
      vramBytes: unified ? mem.total : 0,
    },
    unifiedMemory: unified,
  };
}

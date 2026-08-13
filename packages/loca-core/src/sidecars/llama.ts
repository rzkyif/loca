// llama-server arg builder + readiness wiring.
//
// Reads from the local `llm.*` settings (model path, host, port, threads,
// context size, mmproj). Thinking and sampling are sent per request, not at
// boot. Returns the `StartOptions`
// the SidecarManager needs to spawn `llama-server`. Caller is responsible
// for resolving the model path; this module assumes the file exists.

import { getDefaultSettings } from "@loca/shared";
import { binPath, llmPort, paths } from "../paths.ts";
import { binaryName, libDirFor, platformExe } from "../binaries/versions.ts";
import { resolveHfPath } from "../models/manager.ts";
import { boolSetting, numSetting, strSetting } from "@loca/core-engine/services/settings-access";
import type { StartOptions } from "./types.ts";

export interface LlamaStartArgs {
  modelPath: string;
  mmprojPath?: string;
  host: string;
  port: string;
  threads: number;
  contextSize: number;
  mmap: boolean;
  webui: boolean;
  /** Layers to offload to the GPU. 0 = CPU only; a large value (e.g. 999) =
   *  offload all. Omitted/undefined lets llama.cpp use its own default. */
  gpuLayers?: number;
  /** Enable flash attention (faster attention; broadly supported). */
  flashAttn: boolean;
}

export function llamaStartArgsFromSettings(
  settings: Record<string, unknown>,
): LlamaStartArgs | null {
  const provider = strSetting(settings, "llm.provider", "local");
  if (provider !== "local") return null;
  // Settings arrive sparse (only user-changed keys are persisted), so
  // untouched model paths must fall back to the schema defaults, not "".
  // Ports keep their channel-aware fallbacks instead: the schema's port
  // defaults are stable-channel values.
  const modelSpec = strSetting(settings, "llm.modelPath", schemaDefault("llm.modelPath"));
  if (!modelSpec) return null;
  // Must fall back to the schema default (false): the requirements flow decides
  // whether to download the mmproj from the same key, so a mismatched fallback
  // here would pass --mmproj for a file that was never fetched.
  const supportImages = boolSetting(
    settings,
    "llm.supportImages",
    schemaDefaultBool("llm.supportImages"),
  );
  const mmprojSpec = supportImages
    ? strSetting(settings, "llm.mmprojPath", schemaDefault("llm.mmprojPath"))
    : "";
  return {
    modelPath: resolveHfPath(modelSpec),
    mmprojPath: mmprojSpec ? resolveHfPath(mmprojSpec) : undefined,
    host: strSetting(settings, "llm.host", "127.0.0.1"),
    port: strSetting(settings, "llm.port", String(llmPort())),
    threads: numSetting(settings, "llm.threads", 4),
    contextSize: numSetting(settings, "llm.contextSize", 4096),
    mmap: boolSetting(settings, "llm.mmap", true),
    webui: boolSetting(settings, "llm.webui", false),
    gpuLayers: presentNumSetting(settings, "llm.gpuLayers"),
    flashAttn: boolSetting(settings, "llm.flashAttn", false),
  };
}

/** The first missing on-disk prerequisite for llama-server (model, mmproj,
 *  or the binary itself), or null when everything is present. Every spawn path
 *  (boot re-apply, idle reload, manual restart route) must check this: spawning
 *  anyway either fails outright (no binary) or, for a configured-but-missing
 *  mmproj, boots a process that exits on load and burns the flap-guard budget.
 *  The mmproj check runs before the binary check (the gating regression test
 *  relies on that order). */
export async function llamaMissingPrereq(args: LlamaStartArgs): Promise<string | null> {
  if (!(await fileExists(args.modelPath))) return `model not on disk: ${args.modelPath}`;
  if (args.mmprojPath && !(await fileExists(args.mmprojPath))) {
    return `mmproj not on disk: ${args.mmprojPath}`;
  }
  if (!(await fileExists(binPath(binaryName("llama-server"))))) return "binary not installed";
  return null;
}

async function fileExists(path: string): Promise<boolean> {
  try {
    const st = await Deno.stat(path);
    return st.isFile;
  } catch {
    return false;
  }
}

/** Build a StartOptions from the parsed args. Caller passes this to
 *  `sidecarManager().start("llama", opts)`. */
export function buildLlamaStartOptions(args: LlamaStartArgs): StartOptions {
  const argv: string[] = [
    "-m",
    args.modelPath,
    "-c",
    String(args.contextSize),
    "-t",
    String(args.threads),
    "--host",
    args.host,
    "--port",
    args.port,
    // One serial slot: the whole context window goes to a single turn.
    // `--parallel N` splits the context evenly across N slots (each turn gets
    // ctx/N), which starves a thinking model and can cut it off mid-thought.
    // Concurrent work (background single-shot calls, a second client) queues
    // on the server instead; the scheduler also serializes ahead of it.
    "--parallel",
    "1",
    // Thinking on/off and its token budget are sent per request now (so editing
    // them needs no server restart). Pin the parse format so the server always
    // extracts the model's thoughts into `reasoning_content`, which the stream
    // reader keys on, regardless of the llama.cpp default.
    "--reasoning-format",
    "deepseek",
  ];
  if (args.mmap) argv.push("--mmap");
  if (typeof args.gpuLayers === "number") {
    argv.push("--n-gpu-layers", String(args.gpuLayers));
  }
  if (args.flashAttn) argv.push("--flash-attn", "on");
  // The llama.cpp web UI is unauthenticated. Only ever enable it when the
  // server is bound to loopback (local debugging); if the sidecar is reachable
  // from the network (a non-loopback host), force it off so it can't be hit by
  // other devices on the LAN.
  const hostIsLoopback =
    args.host === "127.0.0.1" || args.host === "localhost" || args.host === "::1";
  if (args.webui && hostIsLoopback) argv.push("--webui");
  else argv.push("--no-webui");
  if (args.mmprojPath) argv.push("--mmproj", args.mmprojPath);
  return {
    binary: binPath(`llama-server${platformExe()}`),
    args: argv,
    readiness: {
      kind: "http",
      url: `http://${args.host}:${args.port}/health`,
    },
    libraryDir: libDirFor(paths().binDir, "llama-server"),
    startupTimeoutMs: 60_000,
  };
}

/** Readiness timeout for llama-server, scaled to the model file size. Loading a
 *  multi-GB GGUF (disk read + mmap warm + KV-cache alloc) on a slow disk or CPU
 *  can far exceed the 60s baseline; a false timeout kills the sidecar and burns
 *  its flap-guard budget, stranding the user on "Failed to start LLM server"
 *  with no recovery. A generous ~25s/GiB on top of a 60s base avoids that,
 *  capped so a pathological size can't wait effectively forever. */
export function llamaReadinessTimeoutMs(modelSizeBytes: number): number {
  const gib = modelSizeBytes / 1_073_741_824;
  const ms = 60_000 + Math.ceil(gib * 25_000);
  return Math.min(ms, 600_000);
}

/** Like {@link buildLlamaStartOptions} but with the readiness window scaled to
 *  the model file size (see {@link llamaReadinessTimeoutMs}). The single
 *  (re)start path for the llama sidecar, used by both boot and the manual
 *  restart route so a large model gets the same generous startup window
 *  whichever way it's launched. Best-effort: keeps the static default if the
 *  stat fails. */
export async function buildLlamaStartOptionsScaled(args: LlamaStartArgs): Promise<StartOptions> {
  const opts = buildLlamaStartOptions(args);
  try {
    const { size } = await Deno.stat(args.modelPath);
    opts.startupTimeoutMs = llamaReadinessTimeoutMs(size);
  } catch {
    /* keep buildLlamaStartOptions' default */
  }
  return opts;
}

function schemaDefault(key: string): string {
  const v = getDefaultSettings()[key];
  return typeof v === "string" ? v : "";
}

function schemaDefaultBool(key: string): boolean {
  return getDefaultSettings()[key] === true;
}

/** Like optionalNumSetting but keeps 0 (a meaningful value for gpuLayers). Returns
 *  undefined only when the key is absent/blank/non-numeric. */
function presentNumSetting(s: Record<string, unknown>, k: string): number | undefined {
  const v = s[k];
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v !== "") {
    const n = Number(v);
    if (Number.isFinite(n)) return n;
  }
  return undefined;
}

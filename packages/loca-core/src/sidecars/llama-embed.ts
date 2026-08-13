// llama-embed: a second llama-server instance hosting the embedding model and
// serving OpenAI-style /v1/embeddings. Tool-relevance + memory RAG embed
// through it (see services/embedding.ts). Unlike the chat `llama` sidecar it is
// NOT gated on llm.provider: embeddings are always-on infrastructure, so it runs
// whenever its model + the llama-server binary are present (the gate lives in
// services/sidecar-boot.ts applyLlamaEmbed). The model is fixed (the MiniLM
// GGUF, EMBED_MODEL_FILE); only the shared `llm.threads` knob is read.

import { EMBED_MODEL_FILE } from "@loca/shared";
import { binPath, embedPort, paths } from "../paths.ts";
import { libDirFor, platformExe } from "../binaries/versions.ts";
import { resolveHfPath } from "../models/manager.ts";
import { numSetting } from "@loca/core-engine/services/settings-access";
import type { StartOptions } from "./types.ts";

// MiniLM's trained context length. llama-server caps n_batch to n_ubatch (512)
// for embeddings anyway, and embed inputs (tool descriptions, doc chunks, query
// text) are short, so 512 is both model-correct and sufficient.
const EMBED_CONTEXT = 512;

export interface LlamaEmbedStartArgs {
  modelPath: string;
  host: string;
  port: string;
  threads: number;
}

/** Resolve the embed sidecar args from settings. Always returns args (embedding
 *  is unconditional); the boot gate decides whether the model + binary are on
 *  disk. Reuses the shared `llm.threads` knob for the CPU thread count. */
export function llamaEmbedStartArgsFromSettings(
  settings: Record<string, unknown>,
): LlamaEmbedStartArgs {
  return {
    modelPath: resolveHfPath(EMBED_MODEL_FILE),
    host: "127.0.0.1",
    port: String(embedPort()),
    threads: numSetting(settings, "llm.threads", 4),
  };
}

/** Build a StartOptions for the embed sidecar. Mirrors the chat llama args but
 *  drops multi-stream chat flags (`--parallel`/`--cont-batching`), reasoning,
 *  and mmproj, and adds `--embeddings --pooling mean` so /v1/embeddings returns
 *  L2-normalized vectors. Same binary + library dir as the chat llama sidecar. */
export function buildLlamaEmbedStartOptions(args: LlamaEmbedStartArgs): StartOptions {
  const argv: string[] = [
    "-m",
    args.modelPath,
    "-c",
    String(EMBED_CONTEXT),
    "-t",
    String(args.threads),
    "--host",
    args.host,
    "--port",
    args.port,
    "--embeddings",
    "--pooling",
    "mean",
    "--no-webui",
  ];
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

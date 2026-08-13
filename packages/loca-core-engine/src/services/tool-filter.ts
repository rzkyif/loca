// Tool-relevance filter.
//
// Phase 1: cosine similarity over the persisted embeddings for the user's
// query vector, top-K. Vectors are L2-normalized at index time so cosine is
// just a dot product.
//
// Phase 2: second-pass LLM relevance check. The LLM is shown the user
// query + each phase-1 candidate's name/description and asked to keep only
// those that could plausibly be useful. Fails open: if the call errors or
// the response is unparseable, we fall back to the phase-1 candidates so
// the user can still pick a tool even with a flaky LLM.

import type OpenAI from "openai";
import { errMessage } from "@loca/shared";
import type { Tool, ToolDescriptor } from "@loca/shared";
import { host } from "../platform/runtime.ts";
import { type LlmEndpointConfig, type LlmRequest, streamChatCompletion } from "./llm-provider.ts";
import {
  cosineNormalized,
  embedSourceHash,
  toolEmbedText,
  toolNameMentioned,
} from "./relevance.ts";
import { getLogger } from "../platform/log.ts";

const log = getLogger("toolFilter");

export interface Phase1Options {
  // Maximum candidates to return after cosine sort. Always-available tools
  // are added on top of this (so the effective tool list passed to the LLM
  // may exceed topK).
  topK?: number;
  // If true, always-available tools are appended to the result (deduped).
  includeAlwaysAvailable?: boolean;
  // The raw user query. When present, tools whose name it directly mentions
  // are returned in `nameMatched` so the caller can force-include them.
  queryText?: string;
}

export interface Phase1Result {
  candidates: ToolDescriptor[];
  alwaysAvailable: ToolDescriptor[];
  // Tools the query names directly (see toolNameMentioned). Force-included by
  // the caller ahead of the cosine candidates and the phase-2 filter, so a
  // tool the user names by hand always reaches the model, even when it has no
  // (fresh) embedding or would sort past topK.
  nameMatched: ToolDescriptor[];
}

export class ToolFilter {
  // Returns the top-K most similar enabled tools, plus the always-available
  // set (if requested). Tools without a stored embedding are silently
  // skipped; re-index via /api/v1/extensions/reindex to backfill them.
  phase1(queryVector: Float32Array, options: Phase1Options = {}): Phase1Result {
    const topK = options.topK ?? 10;
    const tools = enabledTools();
    // One batched query instead of one loadEmbedding() round-trip per tool.
    const embeddings = host().tools?.loadToolEmbeddings() ?? new Map();
    const scored: Array<{ tool: Tool; score: number }> = [];
    const always: ToolDescriptor[] = [];
    const nameMatched: ToolDescriptor[] = [];
    for (const tool of tools) {
      // An always-available tool is offered every turn, so it bypasses the
      // relevance pipeline entirely: no cosine scoring, no candidate slot, no
      // phase-2 pass. The global "allow always-available tools" toggle rides in
      // as includeAlwaysAvailable; when it's off, the tool falls through and is
      // ranked like any other.
      if (tool.alwaysAvailable && options.includeAlwaysAvailable !== false) {
        always.push(toDescriptor(tool));
        continue;
      }
      // Direct name mention is checked before the embedding gate: a named tool
      // is force-included even when it has no (fresh) embedding yet.
      if (options.queryText && toolNameMentioned(options.queryText, tool.name)) {
        nameMatched.push(toDescriptor(tool));
      }
      const emb = embeddings.get(tool.id);
      if (!emb) continue;
      // Skip vectors embedded by a different model: a stale source hash means
      // the stored vector isn't comparable to the current-model query vector,
      // so scoring it would silently mis-rank. It reads as "not embedded" until
      // /extensions/reindex backfills it.
      if (emb.sourceHash !== embedSourceHash(toolEmbedText(tool))) continue;
      const score = cosineNormalized(queryVector, emb.vector);
      scored.push({ tool, score });
    }
    scored.sort((a, b) => b.score - a.score);
    const candidates: ToolDescriptor[] = [];
    for (const { tool, score } of scored) {
      if (candidates.length >= topK) break;
      candidates.push({ ...toDescriptor(tool), similarity: score });
    }
    return { candidates, alwaysAvailable: always, nameMatched };
  }

  // Phase-2 second-pass filter. Asks the LLM which of the phase-1
  // candidates are actually relevant to the user's query. Returns the
  // subset to expose to the chat completion. Always-available tools are
  // NOT passed through here; the caller appends them after phase-2.
  async phase2(
    query: string,
    candidates: ToolDescriptor[],
    endpoint: LlmEndpointConfig,
    reasoningBudget: number,
    signal?: AbortSignal,
  ): Promise<ToolDescriptor[]> {
    if (candidates.length === 0) return [];

    const listing = candidates.map((c, i) => `${i + 1}. ${c.name}: ${c.description}`).join("\n");

    const messages: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [
      {
        role: "system",
        content:
          "You filter a candidate tool list down to those plausibly useful " +
          "for the user's query. Be inclusive when uncertain. Respond with " +
          "ONLY a comma-separated list of tool numbers (e.g. `1,3,5`) or the " +
          "literal word `none`.",
      },
      {
        role: "user",
        content: `User query: ${query}\n\nCandidates:\n${listing}`,
      },
    ];

    const req: LlmRequest = {
      endpoint,
      messages,
      // Thinking off by default (Refinement Thinking Budget = 0); a positive
      // budget opts in, added on top of the short answer allowance.
      overrides: {
        temperature: 0,
        maxTokens: 64 + reasoningBudget,
        reasoningBudget,
      },
      signal,
    };

    let response = "";
    try {
      for await (const delta of streamChatCompletion(req)) {
        if (delta.contentDelta) response += delta.contentDelta;
      }
    } catch (err) {
      log.warn(`phase-2 LLM call failed; falling back to phase-1 candidates: ${errMessage(err)}`);
      return candidates;
    }

    const text = response.trim().toLowerCase();
    if (text === "none" || text === "") return [];

    const picks = new Set<number>();
    for (const part of text.split(/[,\s]+/)) {
      const n = parseInt(part, 10);
      if (Number.isInteger(n) && n >= 1 && n <= candidates.length) {
        picks.add(n - 1);
      }
    }
    if (picks.size === 0) {
      // Unparseable response: fail open with phase-1 set.
      log.warn(
        `phase-2 response unparseable: ${JSON.stringify(response.slice(0, 80))}; falling back`,
      );
      return candidates;
    }
    return [...picks].sort((a, b) => a - b).map((i) => candidates[i]);
  }
}

let _instance: ToolFilter | null = null;
export function toolFilter(): ToolFilter {
  if (!_instance) _instance = new ToolFilter();
  return _instance;
}

// Test-only: drops the cached filter so the next `toolFilter()` call
// rebuilds it.
export function __resetForTesting(): void {
  _instance = null;
}

// This query bypasses the registry, so it re-applies the same OS gating the
// registry does (toolPlatformSupported): an OS-incompatible tool must never
// reach the relevance filter either.
function enabledTools(): Tool[] {
  // Enabled tools of installed extensions, OS-gated by the host tool catalog.
  return (
    host()
      .tools?.installedExtensionTools()
      .filter((t) => t.enabled) ?? []
  );
}

function toDescriptor(tool: Tool): ToolDescriptor {
  return {
    toolId: tool.id,
    extensionId: tool.extensionId,
    name: tool.name,
    description: tool.description,
  };
}

// Background memory indexing: keeps each memory's LLM summary and
// embedding in sync with its content, via the idle-gated background queue.
// One job per memory; the job no-ops cheaply when both are fresh (hash
// comparisons), so callers schedule liberally: on boot and after every
// mutation. Jobs are not persisted; staleness is re-derived from the
// hashes, so anything dropped at shutdown is found again on the next boot.

import { DEFAULT_MEMORY_SUMMARY_PROMPT, errMessage } from "@loca/shared";
import { backgroundQueue } from "./background-queue.ts";
import { memoriesStore } from "@loca/core-engine/services/memories-store";
import { isEmbeddingModelReady } from "@loca/core-engine/services/embedding";
import { llmIdle } from "./llm-idle.ts";
import {
  embedSourceHash,
  embedWithHash,
  memoryEmbedText,
} from "@loca/core-engine/services/relevance";
import {
  loadCoreSettings,
  loadCoreSettingsResolved,
} from "@loca/core-engine/services/core-settings";
import { resolveEndpoint } from "@loca/core-engine/services/endpoint-resolver";
import { singleShot } from "@loca/core-engine/services/single-shot";
import { thinkingBudget } from "@loca/core-engine/services/thinking-budget";
import { getLogger } from "../shared/log.ts";

const log = getLogger("doc-indexer");

// Caps so a huge pasted memory can't blow up the summary request or the
// embedding text.
const SUMMARY_INPUT_MAX_CHARS = 8_000;
const EMBED_FALLBACK_HEAD_CHARS = 512;

/** Enqueue an index pass for one memory, or for every memory when no id
 *  is given (boot, rescan). */
export function scheduleMemoryIndexing(id?: string): void {
  const ids =
    id !== undefined
      ? [id]
      : memoriesStore()
          .listIndexStates()
          .map((s) => s.id);
  for (const docId of ids) {
    backgroundQueue().enqueue({
      key: `doc-index:${docId}`,
      run: () => indexMemory(docId),
    });
  }
}

async function indexMemory(id: string): Promise<void> {
  const store = memoriesStore();
  const state = store.listIndexStates().find((s) => s.id === id);
  if (!state) return; // deleted while queued

  // Summary: regenerate when it was derived from different content.
  if (state.summarySourceHash !== state.contentHash) {
    const doc = await store.get(id);
    // Resolved: a background job has no client to overlay, but it still needs
    // the schema defaults. Reading raw here is what made the Summary Thinking
    // Budget resolve to 0 on every install while the panel advertised 256.
    const settings = await loadCoreSettingsResolved();
    try {
      // Background jobs run during the idle window, which is exactly when
      // idle-unload may have stopped the local model. ensureLoaded() reloads
      // it (no-op for an external provider or an already-loaded model);
      // without this the summary request races a stopped sidecar and the
      // job is dropped, leaving the memory permanently unsummarized.
      await llmIdle().ensureLoaded(settings);
      // The SPARSE record for the resolver: its `llm.port` fallback is the
      // channel-adjusted `llmPort()` and the schema default is the bare "7701",
      // so a resolved record points dev and latest at the stable channel's
      // llama-server. See D-078.
      const endpoint = await resolveEndpoint(await loadCoreSettings());
      // Summary Thinking Budget defaults to 256, added on top of the summary's
      // own token allowance; 0 turns thinking off for this call.
      const budget = thinkingBudget(settings, "prompts.memorySummaryThinkingBudget");
      const summary = await singleShot({
        systemPrompt: summaryPrompt(settings),
        userMessage: `Memory title: ${doc.title}\n\n${doc.content.slice(
          0,
          SUMMARY_INPUT_MAX_CHARS,
        )}`,
        endpoint,
        overrides: {
          temperature: 0,
          maxTokens: 160 + budget,
          reasoningBudget: budget,
        },
      });
      if (summary) store.setSummary(id, summary, state.contentHash);
    } catch (err) {
      // Re-found on the next mutation/boot; never fatal.
      log.warn(`summary for memory ${id} failed: ${errMessage(err)}`);
    }
  }

  // Embedding: derived from title + summary (or the content head until a
  // summary exists), so it refreshes after the summary lands above.
  if (!(await isEmbeddingModelReady())) {
    // The model isn't downloaded yet: re-enqueue so the embedding lands once
    // it is, rather than waiting for the next unrelated mutation. Guard with
    // the source hash so a steady not-ready state doesn't spin (the embed
    // text is still stale, so we only re-arm when there is real work).
    const pending = store.listIndexStates().find((s) => s.id === id);
    if (
      pending &&
      pending.embeddingSourceHash !== embedSourceHash(embedText(await store.get(id)))
    ) {
      backgroundQueue().enqueueDeferred({
        key: `doc-index:${id}`,
        run: () => indexMemory(id),
      });
    }
    return;
  }
  const doc = await store.get(id);
  const text = embedText(doc);
  const currentState = store.listIndexStates().find((s) => s.id === id);
  if (!currentState) return;
  if (currentState.embeddingSourceHash === embedSourceHash(text)) return; // fresh
  const result = await embedWithHash(text);
  if (result) store.setEmbedding(id, result.vector, result.sourceHash);
}

function embedText(doc: { title: string; summary?: string; content: string }): string {
  return memoryEmbedText(doc.title, doc.summary ?? doc.content.slice(0, EMBED_FALLBACK_HEAD_CHARS));
}

function summaryPrompt(settings: Record<string, unknown>): string {
  const v = settings["prompts.memorySummaryPrompt"];
  return typeof v === "string" && v.trim() !== "" ? v : DEFAULT_MEMORY_SUMMARY_PROMPT;
}

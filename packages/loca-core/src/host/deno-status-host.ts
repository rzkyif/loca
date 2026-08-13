// DenoHost StatusHost: wires the engine's status/idle/schedule hooks onto the
// Deno service's core-status telemetry, the local-model idle manager, and the
// prompt scheduler. On a host without local inference these would be no-ops; the
// Deno service has all three.

import type { ScheduledPromptDraft } from "@loca/shared";
import type { StatusHost } from "@loca/core-engine";
import { coreStatus } from "../services/core-status.ts";
import { llmIdle } from "../services/llm-idle.ts";
import { promptScheduler } from "../services/prompt-scheduler.ts";
import { scheduleMemoryIndexing } from "../services/memories-indexer.ts";

export const denoStatusHost: StatusHost = {
  noteActiveStreams(n: number): void {
    coreStatus().noteActiveStreams(n);
  },
  noteLlmQueue(active: number, queued: number): void {
    coreStatus().noteLlmQueue(active, queued);
  },
  ensureLocalModelLoaded(settings: Record<string, unknown>): Promise<void> {
    return llmIdle().ensureLoaded(settings);
  },
  noteLlmActivity(): void {
    llmIdle().noteActivity();
  },
  onTurnEnd(activeStreams: number): void {
    llmIdle().onTurnEnd(activeStreams);
  },
  createScheduledPrompt(clientId: string, draft: ScheduledPromptDraft): void {
    promptScheduler().create(clientId, draft);
  },
  scheduleMemoryIndexing(memoryId?: string): void {
    scheduleMemoryIndexing(memoryId);
  },
};

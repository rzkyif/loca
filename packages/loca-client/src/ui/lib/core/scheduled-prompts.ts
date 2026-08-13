// Scheduled prompt CRUD + manual run wrappers around the core REST API.
// Schedules live on the core (the scheduler arms from SQLite); the client
// only mirrors the list for the settings manager.

import type { ScheduledPrompt, ScheduledPromptDraft, ScheduleSpec } from "@loca/shared";
import type { CoreClient } from "./client";

export interface ScheduledPromptPatch {
  title?: string;
  instruction?: string;
  schedule?: ScheduleSpec;
  runMissed?: boolean;
  enabled?: boolean;
}

export class ScheduledPromptsApi {
  constructor(private readonly client: CoreClient) {}

  async list(): Promise<ScheduledPrompt[]> {
    const res = await this.client.get<{ scheduledPrompts: ScheduledPrompt[] }>(
      "/api/v1/scheduled-prompts",
    );
    return res.scheduledPrompts;
  }

  create(draft: ScheduledPromptDraft): Promise<ScheduledPrompt> {
    return this.client.post("/api/v1/scheduled-prompts", draft);
  }

  update(id: string, patch: ScheduledPromptPatch): Promise<ScheduledPrompt> {
    return this.client.patch(`/api/v1/scheduled-prompts/${encodeURIComponent(id)}`, patch);
  }

  delete(id: string): Promise<void> {
    return this.client.del(`/api/v1/scheduled-prompts/${encodeURIComponent(id)}`) as Promise<void>;
  }

  run(id: string): Promise<{ sessionId: string }> {
    return this.client.post(`/api/v1/scheduled-prompts/${encodeURIComponent(id)}/run`, {});
  }
}

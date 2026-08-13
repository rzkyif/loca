import type {
  AutocorrectRequest,
  AutocorrectResponse,
  MergeTranscriptionRequest,
  MergeTranscriptionResponse,
} from "@loca/shared";
import type { CoreClient } from "./client";

export class LlmApi {
  constructor(private readonly client: CoreClient) {}

  async autocorrect(text: string): Promise<string> {
    const req: AutocorrectRequest = { text };
    const res = await this.client.post<AutocorrectResponse>("/api/v1/llm/autocorrect", req);
    return res.text;
  }

  async merge(existing: string, next: string): Promise<string> {
    const req: MergeTranscriptionRequest = { existing, next };
    const res = await this.client.post<MergeTranscriptionResponse>("/api/v1/llm/merge", req);
    return res.text;
  }
}

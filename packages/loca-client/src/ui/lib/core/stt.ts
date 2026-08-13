import type { SttStatusResponse, SttTranscribeResponse } from "@loca/shared";
import type { CoreClient } from "./client";

export class SttApi {
  constructor(private readonly client: CoreClient) {}

  async transcribe(audio: Blob, language?: string): Promise<SttTranscribeResponse> {
    const form = new FormData();
    form.set("audio", audio, "audio.wav");
    if (language) form.set("language", language);
    return await this.client.postForm("/api/v1/stt/transcribe", form);
  }

  status(): Promise<SttStatusResponse> {
    return this.client.get("/api/v1/stt/status");
  }
}

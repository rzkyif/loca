import type { TtsStatusResponse, TtsSynthesizeRequest, TtsVoicesResponse } from "@loca/shared";
import type { CoreClient } from "./client";

export class TtsApi {
  constructor(private readonly client: CoreClient) {}

  async load(): Promise<void> {
    await this.client.post("/api/v1/tts/load", {});
  }

  async unload(): Promise<void> {
    await this.client.post("/api/v1/tts/unload", {});
  }

  // Returns a WAV blob. Caller wraps with `new Audio(URL.createObjectURL(...))`
  // or feeds to AudioContext.decodeAudioData.
  async synthesize(req: TtsSynthesizeRequest): Promise<Blob> {
    return await this.client.postBlob("/api/v1/tts/synthesize", req);
  }

  voices(): Promise<TtsVoicesResponse> {
    return this.client.get("/api/v1/tts/voices");
  }

  status(): Promise<TtsStatusResponse> {
    return this.client.get("/api/v1/tts/status");
  }
}

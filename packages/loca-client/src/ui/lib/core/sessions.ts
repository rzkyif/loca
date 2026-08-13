// Session CRUD + message append/patch wrappers around the core REST API.

import type { GetSessionResponse, Message, Session, SessionListEntry } from "@loca/shared";
import type { CoreClient } from "./client";

export class SessionsApi {
  constructor(private readonly client: CoreClient) {}

  list(): Promise<SessionListEntry[]> {
    return this.client.get("/api/v1/sessions");
  }

  create(title?: string, opts?: { temporary?: boolean }): Promise<Session> {
    return this.client.post("/api/v1/sessions", { title, temporary: opts?.temporary });
  }

  get(id: string): Promise<GetSessionResponse> {
    return this.client.get(`/api/v1/sessions/${encodeURIComponent(id)}`);
  }

  patchTitle(id: string, title: string): Promise<{ id: string; title: string }> {
    return this.client.patch(`/api/v1/sessions/${encodeURIComponent(id)}`, {
      title,
    });
  }

  regenerateTitle(id: string): Promise<void> {
    return this.client.post(
      `/api/v1/sessions/${encodeURIComponent(id)}/regenerate-title`,
      {},
    ) as Promise<void>;
  }

  delete(id: string): Promise<void> {
    return this.client.del(`/api/v1/sessions/${encodeURIComponent(id)}`) as Promise<void>;
  }

  appendMessage(sessionId: string, message: Message): Promise<{ id: string; ord: number }> {
    return this.client.post(`/api/v1/sessions/${encodeURIComponent(sessionId)}/messages`, message);
  }

  patchMessage(
    sessionId: string,
    messageId: string,
    patch: Partial<Message>,
  ): Promise<{ id: string; ord: number }> {
    return this.client.patch(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}`,
      patch,
    );
  }

  deleteMessage(sessionId: string, messageId: string): Promise<void> {
    return this.client.del(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}`,
    ) as Promise<void>;
  }

  async uploadAttachment(
    sessionId: string,
    messageId: string,
    file: Blob,
    filename: string,
  ): Promise<{ id: string; absPath: string; filename: string }> {
    const form = new FormData();
    form.set("file", file, filename);
    form.set("messageId", messageId);
    return await this.client.postForm(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/attachments`,
      form,
    );
  }

  attachmentUrl(sessionId: string, attachmentId: string): string {
    return `${this.client.endpoint.baseUrl}/api/v1/sessions/${encodeURIComponent(
      sessionId,
    )}/attachments/${encodeURIComponent(attachmentId)}`;
  }
}

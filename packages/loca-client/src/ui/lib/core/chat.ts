// Chat-stream control via WS frames. Streaming is server-driven; this module
// just sends start/interrupt/tool-response frames to the connected core.

import type { AskUserAnswer, ScheduledPromptDraft } from "@loca/shared";
import type { CoreClient } from "./client";

export class ChatApi {
  constructor(private readonly client: CoreClient) {}

  start(
    streamId: string,
    sessionId: string,
    route: "default" | "secondary" = "default",
    opts?: {
      systemPrompt?: string;
      toolsHint?: string;
      anchorMessageId?: string;
    },
  ): void {
    this.client.sendWs({
      kind: "chat.start",
      streamId,
      sessionId,
      route,
      ...(opts?.systemPrompt !== undefined ? { systemPrompt: opts.systemPrompt } : {}),
      ...(opts?.toolsHint ? { toolsHint: opts.toolsHint } : {}),
      ...(opts?.anchorMessageId ? { anchorMessageId: opts.anchorMessageId } : {}),
    });
  }

  interrupt(streamId: string): void {
    this.client.sendWs({ kind: "chat.interrupt", streamId });
  }

  /** Ask the core to (re)attach this client to a session: if a turn is still
   *  generating on it, the core re-emits the in-flight messages so far and any
   *  open tool prompt, then live deltas resume. A no-op server-side when
   *  nothing is in flight. Sent on session open and after a reconnect. */
  subscribe(sessionId: string): void {
    this.client.sendWs({ kind: "chat.subscribe", sessionId });
  }

  respondAskUser(callId: string, requestId: string, answers: AskUserAnswer[]): void {
    this.client.sendWs({
      kind: "tool.askuser_response",
      callId,
      requestId,
      answers,
    });
  }

  respondPermission(callId: string, requestId: string, allow: boolean): void {
    this.client.sendWs({
      kind: "tool.permission_response",
      callId,
      requestId,
      allow,
    });
  }

  /** Settle a schedule confirm form; `draft` carries the user's (possibly
   *  edited) version and must be present when accepted. */
  respondScheduleConfirm(
    callId: string,
    requestId: string,
    accepted: boolean,
    draft?: ScheduledPromptDraft,
  ): void {
    this.client.sendWs({
      kind: "schedule.confirm_response",
      callId,
      requestId,
      accepted,
      ...(draft ? { draft } : {}),
    });
  }

  cancelTool(callId: string): void {
    this.client.sendWs({ kind: "tool.cancel", callId });
  }
}

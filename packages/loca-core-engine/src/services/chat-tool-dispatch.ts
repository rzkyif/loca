// Tool-call dispatch for a turn. Given a resolved pending call and the (already
// born) tool message, run the call to completion and fold the outcome into the
// message; the orchestrator finalizes it afterwards. Two paths: MCP tools run on
// their server (a straight request/response), extension tools run in the
// worker-pool sandbox with the full progress / askUser / permission / schedule
// flow forwarded as prompts. The in-flight controller is registered for the
// call's lifetime so the WS forward* handlers can deliver responses and cancels.

import type { DisplayMessage, ServerToClientFrame, ToolMessage } from "@loca/shared";
import { errMessage } from "@loca/shared";
import { sessionsRepo } from "./sessions-store.ts";
import type { ActiveStream, ResolvedPendingCall } from "./chat-types.ts";
import type { TurnWriter } from "./chat-turn-writer.ts";
import { lastUserText } from "./chat-history.ts";
import { host } from "../platform/runtime.ts";
import type { ToolCallController, ToolHost } from "./tool-host.ts";
import { newMessageId } from "../platform/ids.ts";

// The host's tool provider (extension sandbox + MCP client). A chat turn only
// reaches dispatch when tools are in play, so its absence is a wiring error.
function toolHost(): ToolHost {
  const t = host().tools;
  if (!t) throw new Error("tool host not available");
  return t;
}

export type InFlightEntry = { clientId: string; ctl: ToolCallController; stream: ActiveStream };

export class ToolDispatcher {
  constructor(
    private readonly send: (clientId: string, frame: ServerToClientFrame) => void,
    private readonly emitPrompt: (
      stream: ActiveStream,
      callId: string,
      frame: ServerToClientFrame,
    ) => void,
    private readonly inFlight: Map<string, InFlightEntry>,
  ) {}

  // Runs one tool call and fills the outcome into `msg` (the message the
  // caller already announced as born); the caller finalizes it afterwards.
  /** Run an MCP tool: parse the model's arguments, call the server, and fold
   *  the returned content into the tool message. MCP tools have no local
   *  permissions, progress, or askUser flow, so this is a straight request. */
  private async executeMcpToolCall(
    pending: ResolvedPendingCall,
    msg: ToolMessage,
    signal?: AbortSignal,
  ): Promise<void> {
    let args: Record<string, unknown> = {};
    try {
      args = pending.arguments ? JSON.parse(pending.arguments) : {};
    } catch {
      // Leave args empty; the server validates against its own schema.
    }
    try {
      const result = (await toolHost().callMcpTool(
        pending.extensionId,
        pending.toolName,
        args,
        signal,
      )) as {
        content?: Array<{ type: string; text?: string }>;
        isError?: boolean;
      };
      const text = (result.content ?? [])
        .filter((c) => c.type === "text" && typeof c.text === "string")
        .map((c) => c.text)
        .join("\n");
      if (result.isError) {
        msg.status = "failed";
        msg.error = text || "MCP tool returned an error";
      } else {
        msg.status = "completed";
        msg.result = text || result;
      }
    } catch (err) {
      msg.status = "failed";
      msg.error = errMessage(err);
    }
  }

  async execute(
    stream: ActiveStream,
    pending: ResolvedPendingCall,
    msg: ToolMessage,
    writer: TurnWriter,
  ): Promise<DisplayMessage[]> {
    // Display bubbles a tool pushes are born live here but returned UNFINALIZED so
    // the caller can persist them AFTER the tool message, keeping the durable
    // order (tool, then display) equal to the live order. Finalizing them eagerly
    // here would persist them ahead of the still-running tool message.
    const bornDisplays: DisplayMessage[] = [];
    if (pending.unknown) {
      msg.status = "failed";
      msg.error = `tool ${pending.toolName} not available`;
      return bornDisplays;
    }
    // MCP tools run on their server, not in the local sandbox: dispatch via the
    // MCP client and skip the worker-pool / permission machinery.
    if (toolHost().isMcpServer(pending.extensionId)) {
      await this.executeMcpToolCall(pending, msg, stream.abort.signal);
      return bornDisplays;
    }
    const tool = toolHost().getTool(`${pending.extensionId}::${pending.toolName}`);
    if (!tool) {
      msg.status = "failed";
      msg.error = "tool not found";
      return bornDisplays;
    }
    // Pre-flight before dispatch: (1) re-verify the extension's content hash so a
    // extension tampered since boot can't execute (this runs on EVERY call, so a
    // reused warm worker is re-checked too); (2) validate the model-emitted
    // arguments against the tool's declared schema and fill defaults, so the
    // worker receives normalized args (not raw model output).
    //
    // The content-hash gate assumes the extension's install dir is immutable
    // between this check and the worker's import. That invariant is now enforced:
    // permissions.ts drops any write grant that resolves into the extension's own
    // installedPath ($extension), so a tool can't self-modify its code to open a
    // TOCTOU here.
    let normalizedArgs: string;
    try {
      await toolHost().verifyToolFresh(pending.extensionId);
      normalizedArgs = toolHost().validateToolArgs(tool, pending.arguments);
    } catch (err) {
      const errMsg = errMessage(err);
      this.send(stream.clientId, {
        kind: "tool.error",
        callId: pending.callId,
        error: errMsg,
      });
      msg.status = "failed";
      msg.error = errMsg;
      return bornDisplays;
    }
    const userMessage = lastUserText(await sessionsRepo().listMessages(stream.sessionId)) ?? "";
    const ctl = toolHost().startToolCall(
      {
        extensionId: pending.extensionId,
        tool,
        argumentsJson: normalizedArgs,
        chatContext: {
          userMessage,
          sessionId: stream.sessionId,
          locale: undefined,
        },
      },
      (event) => {
        if (event.kind === "progress") {
          // Persist the tool's latest wording + progress on the message so
          // the reloaded bubble keeps them.
          if (event.label !== undefined) msg.label = event.label;
          if (event.description !== undefined) {
            msg.description = event.description;
          }
          msg.progress = event.progress;
          this.send(stream.clientId, {
            kind: "tool.progress",
            callId: pending.callId,
            progress: event.progress,
            label: event.label,
            description: event.description,
          });
        } else if (event.kind === "ask_user_request") {
          this.emitPrompt(stream, pending.callId, {
            kind: "tool.askuser_request",
            callId: pending.callId,
            requestId: event.requestId,
            questions: event.questions,
          });
        } else if (event.kind === "schedule_request") {
          this.emitPrompt(stream, pending.callId, {
            kind: "schedule.confirm_request",
            callId: pending.callId,
            requestId: event.requestId,
            draft: event.draft,
          });
        } else if (event.kind === "permission_request") {
          this.emitPrompt(stream, pending.callId, {
            kind: "tool.permission_request",
            callId: pending.callId,
            requestId: event.requestId,
            permissionKind: event.permission,
            resource: event.resource,
            apiName: event.apiName,
            declared: event.declared,
            reason: event.reason,
            extensionId: pending.extensionId,
            toolName: pending.toolName,
          });
        } else if (event.kind === "log") {
          this.send(stream.clientId, {
            kind: "tool.log",
            callId: pending.callId,
            level: event.level,
            message: event.message,
          });
        } else if (event.kind === "display") {
          // One-way push: born now (live) so the client sees it immediately in
          // order (after the running tool message), but NOT finalized here.
          // Finalizing eagerly would persist it AHEAD of the still-running tool
          // message, so its durable order (display-before-tool) would contradict
          // the live order (tool-before-display). Collected instead and finalized
          // by the caller after the tool message, restoring birth-order persist.
          const displayMsg: DisplayMessage = {
            id: newMessageId(),
            ord: -1,
            role: "display",
            callId: pending.callId,
            content: event.content,
            createdAtMs: Date.now(),
          };
          writer.born(displayMsg);
          bornDisplays.push(displayMsg);
        } else if (event.kind === "tool_cancelled") {
          this.send(stream.clientId, {
            kind: "tool.cancelled",
            callId: pending.callId,
          });
        }
      },
    );
    this.inFlight.set(pending.callId, {
      clientId: stream.clientId,
      ctl,
      stream,
    });
    // Propagate a chat interrupt (stream abort) to the in-flight extension tool,
    // so interrupting the turn cancels the tool too - matching the MCP path,
    // which is handed stream.abort.signal directly, instead of relying on the
    // client to send a separate tool.cancel frame.
    const onStreamAbort = () => ctl.cancel();
    if (stream.abort.signal.aborted) ctl.cancel();
    else stream.abort.signal.addEventListener("abort", onStreamAbort, { once: true });
    try {
      const result = await ctl.done;
      this.send(stream.clientId, {
        kind: "tool.result",
        callId: pending.callId,
        result,
      });
      msg.status = "completed";
      msg.result = result;
    } catch (err) {
      const errMsg = errMessage(err);
      this.send(stream.clientId, {
        kind: "tool.error",
        callId: pending.callId,
        error: errMsg,
      });
      msg.status = "failed";
      msg.error = errMsg;
    } finally {
      stream.abort.signal.removeEventListener("abort", onStreamAbort);
      stream.outstandingPrompts.delete(pending.callId);
      this.inFlight.delete(pending.callId);
    }
    return bornDisplays;
  }
}

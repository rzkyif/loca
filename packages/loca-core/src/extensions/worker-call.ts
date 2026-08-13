// One in-flight tool call against a (warm) worker. Owns everything that used to
// live in the WorkerPool.startCall closure: the call's settle state, the
// pausable time budget (CallTimeout), the open prompt bookkeeping (askUser /
// permission / schedule + module-broker permission prompts), and the worker
// frame handler. The instance IS the CallController returned to the caller.
//
// It never touches the pool's maps or `worker.inFlightCalls` directly; the four
// PoolCallbacks route every lifecycle transition (started / settled / killed /
// boot-failed) back to the pool, which owns the in-flight accounting (so the
// count is decremented from exactly one place per settle) and the worker's
// warm/retire/kill disposition.
//
// Behavior-preserving invariants: the budget pauses while any user wait is open
// (a refcount, not a per-kind flag, so a sibling prompt can't re-arm the timer);
// a respond* with an unknown requestId is dropped whole; `off()` + `disarm()` +
// `settled = true` lead every terminal path (idempotency); the prompt-validation
// fast-answers that unblock the tool stay before the budget pause.

import type { AskUserAnswer, AskUserQuestion, ChatContext } from "./worker-protocol.ts";
import type { PoolToWorkerFrame, WorkerToPoolFrame } from "./worker-protocol.ts";
import { askUserQuestionSchema, errMessage, scheduledPromptDraftSchema } from "@loca/shared";
import type { ScheduledPromptDraft, Tool } from "@loca/shared";
import { handleModuleRequest, type ModulePrompt } from "../services/module-broker.ts";
import { scrubSecrets } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { AppError } from "@loca/core-engine";
import { boundDisplayContent } from "./worker-display-bounds.ts";
import { CallTimeout } from "./worker-call-timeout.ts";

const log = getLogger("workerpool");

export interface ToolCallStart {
  extensionId: string;
  tool: Tool;
  argumentsJson: string;
  chatContext: ChatContext;
}

export type CallEvent =
  | {
      kind: "progress";
      progress: number;
      label?: string;
      description?: string;
    }
  | {
      kind: "ask_user_request";
      requestId: string;
      questions: AskUserQuestion[];
    }
  // The running tool hit a Deno permission prompt that needs the user's
  // decision (declared ask-state permission, or undeclared with policy
  // `ask`). The call is paused on the prompt; answer via respondPermission.
  | {
      kind: "permission_request";
      requestId: string;
      permission: import("@loca/shared").PermissionKind;
      resource: string;
      apiName?: string;
      declared: boolean;
      reason?: string;
    }
  // The running tool proposed a scheduled prompt (ctx.schedulePrompt). The
  // call is paused until the user confirms (possibly after editing the
  // draft) or rejects in chat; answer via respondSchedule.
  | {
      kind: "schedule_request";
      requestId: string;
      draft: ScheduledPromptDraft;
    }
  | { kind: "log"; level: "debug" | "info" | "warn" | "error"; message: string }
  | { kind: "stderr_log"; line: string }
  // One-way display push from the tool (ctx.display.*). Chat persists it as
  // a DisplayMessage; no response flows back to the worker.
  | { kind: "display"; content: import("@loca/shared").DisplayContent }
  // Emitted synchronously when cancel() is invoked so the UI's ToolCall
  // bubble can transition to the "cancelled" terminal state. The promise
  // returned by startCall still rejects with AppError("tool call cancelled")
  // after the worker acks; consumers that only care about the UX bubble
  // can stop here.
  | { kind: "tool_cancelled" };

export interface CallController {
  callId: string;
  // Reject the underlying askUser promise + emit tool_error.
  cancel(): void;
  // Forward the user's answer back to the worker.
  respondAskUser(requestId: string, answers: AskUserAnswer[]): void;
  // Allow or reject a pending runtime permission prompt.
  respondPermission(requestId: string, allow: boolean): void;
  // Settle a pending schedule confirm; `draft` carries the user's edits
  // when accepted.
  respondSchedule(requestId: string, accepted: boolean, draft?: ScheduledPromptDraft): void;
  // True while `requestId` is an unanswered schedule confirm for this call.
  // Callers with side effects keyed to the confirm (chat persists the draft)
  // must check this first: a stale or replayed response is otherwise
  // indistinguishable from the real one.
  hasPendingSchedule(requestId: string): boolean;
  // Settle when the worker emits tool_result / tool_error.
  done: Promise<unknown>;
}

// The slice of WorkerHandle an in-flight call drives. Narrowed to a structural
// interface so the call's state machine is unit-testable against a fake worker.
export interface CallWorker {
  on(listener: (frame: WorkerToPoolFrame) => void): () => void;
  send(frame: PoolToWorkerFrame): void;
  waitForBoot(): Promise<void>;
  answerPrompt(requestId: string, allow: boolean): void;
}

// Lifecycle transitions the call reports back to the pool. The pool owns
// `worker.inFlightCalls` accounting and the worker's warm/retire/kill fate; the
// call only signals which transition occurred.
export interface PoolCallbacks {
  // Boot succeeded and the call frame was sent: account the in-flight call and
  // cancel any pending idle eviction.
  onStarted(): void;
  // Normal settle (tool_result / tool_error): release the in-flight count and
  // return the worker to the warm set (or retire it if it answered a prompt).
  onSettled(): void;
  // Forced settle (timeout / worker exit / unacked cancel): release the count
  // and kill the worker (wedged or already dead, so it can't be reused).
  onKilled(): void;
  // Boot failed: release the count and arm idle eviction.
  onBootFailed(): void;
}

export interface InFlightCallOptions {
  callId: string;
  worker: CallWorker;
  spec: ToolCallStart;
  onEvent: (event: CallEvent) => void;
  callbacks: PoolCallbacks;
  callTimeoutMs: number;
  drainTimeoutMs: number;
}

export class InFlightCall implements CallController {
  readonly callId: string;
  readonly done: Promise<unknown>;

  private readonly worker: CallWorker;
  private readonly spec: ToolCallStart;
  private readonly onEvent: (event: CallEvent) => void;
  private readonly callbacks: PoolCallbacks;
  private readonly drainTimeoutMs: number;
  private readonly timeout: CallTimeout;

  private cancelled = false;
  private settled = false;
  private callStarted = false;
  // Count of prompts (askUser, permission, schedule confirm) currently
  // waiting on the user. The timeout budget pauses while any wait is open
  // and resumes only when the last one settles; a flag per prompt kind
  // would re-arm the timer while a sibling prompt is still pending.
  private pendingUserWaits = 0;

  private resolveDone: (value: unknown) => void = () => {};
  private rejectDone: (err: Error) => void = () => {};
  private offHandler: () => void = () => {};

  // Pending module-broker permission prompts for this call, keyed by
  // requestId. respondPermission answers either one of these or a PTY
  // prompt (worker.answerPrompt); cancel resolves them all as rejected.
  private readonly brokerPrompts = new Map<string, (allow: boolean) => void>();
  // Open requestIds per prompt kind. A respond* with an unknown requestId
  // (stale, replayed, or forged) is dropped whole: forwarding it would
  // resume the timeout budget while the real prompt is still open.
  private readonly pendingAskRequests = new Set<string>();
  private readonly pendingScheduleRequests = new Set<string>();
  private readonly pendingPermRequests = new Set<string>();

  constructor(opts: InFlightCallOptions) {
    this.callId = opts.callId;
    this.worker = opts.worker;
    this.spec = opts.spec;
    this.onEvent = opts.onEvent;
    this.callbacks = opts.callbacks;
    this.drainTimeoutMs = opts.drainTimeoutMs;
    this.timeout = new CallTimeout(opts.callTimeoutMs, () => this.onTimeoutExpire());

    this.done = new Promise<unknown>((resolve, reject) => {
      this.resolveDone = resolve;
      this.rejectDone = reject;
      this.offHandler = this.worker.on((frame) => this.handleFrame(frame));

      this.worker
        .waitForBoot()
        .then(() => this.onBoot())
        .catch((err) => this.onBootFail(err));
    });
  }

  private beginUserWait(): void {
    this.pendingUserWaits++;
    this.timeout.pause();
  }

  private endUserWait(): void {
    this.pendingUserWaits = Math.max(0, this.pendingUserWaits - 1);
    if (this.pendingUserWaits === 0 && !this.timeout.armed && !this.cancelled) {
      this.armTimeout();
    }
  }

  private armTimeout(): void {
    if (this.cancelled) return;
    this.timeout.arm();
  }

  private promptUser = (prompt: ModulePrompt): Promise<boolean> => {
    // Same budget pause as a PTY prompt: waiting on the user's decision
    // must not consume the tool's time budget.
    this.beginUserWait();
    const requestId = `mod-${crypto.randomUUID()}`;
    return new Promise<boolean>((resolve) => {
      this.brokerPrompts.set(requestId, resolve);
      this.onEvent({
        kind: "permission_request",
        requestId,
        permission: prompt.permission,
        resource: prompt.resource,
        declared: prompt.declared,
        reason: prompt.reason,
      });
    });
  };

  private onTimeoutExpire(): void {
    if (this.pendingUserWaits > 0) {
      // Defensive: with the wait refcount the timer should never fire while a
      // prompt is open. The CallTimeout already cleared its handle, so a later
      // endUserWait can re-arm with the remaining budget.
      return;
    }
    // Bookkeeping must run even if the worker is already dead. Otherwise the
    // listener leaks and `done` hangs.
    this.offHandler();
    this.settled = true;
    // A cooperative `cancel` frame is useless against a CPU-bound or wedged
    // tool: it never reaches the worker's event loop. Force-kill the worker
    // and drop it from the pool so it stops burning a core, instead of
    // leaving it warm until idle eviction (~5 min). A fresh worker spawns on
    // the next call.
    this.callbacks.onKilled();
    this.rejectDone(new AppError("internal_error", "tool call timed out"));
  }

  private onBoot(): void {
    if (this.cancelled) {
      this.offHandler();
      this.rejectDone(new AppError("internal_error", "cancelled before boot"));
      return;
    }
    this.callStarted = true;
    this.callbacks.onStarted();
    this.worker.send({
      kind: "call",
      callId: this.callId,
      toolName: this.spec.tool.name,
      fnExport: this.spec.tool.fnExport,
      arguments: this.spec.argumentsJson,
      chatContext: this.spec.chatContext,
    });
    this.armTimeout();
  }

  private onBootFail(err: unknown): void {
    // Worker boot failed -> synthesize a tool_error event so the UI doesn't
    // hang in "running", then reject `done` so the caller's catch path runs.
    // Without the synthetic event the chat-side ToolCall bubble would never
    // reach a terminal state.
    this.offHandler();
    this.settled = true;
    this.callbacks.onBootFailed();
    const msg = errMessage(err);
    try {
      this.onEvent({
        kind: "log",
        level: "error",
        message: `worker boot failed: ${msg}`,
      });
    } catch {
      /* listener errors are non-fatal here */
    }
    this.rejectDone(
      err instanceof Error ? err : new AppError("internal_error", `worker boot failed: ${msg}`),
    );
  }

  private handleFrame(frame: WorkerToPoolFrame): void {
    if (
      (frame as { callId?: string }).callId !== undefined &&
      (frame as { callId: string }).callId !== this.callId &&
      frame.kind !== "stderr_log"
    ) {
      return;
    }
    switch (frame.kind) {
      case "progress":
        this.onEvent({
          kind: "progress",
          progress: frame.progress,
          label: frame.label,
          description: frame.description,
        });
        return;
      case "ask_user_request": {
        // The frame passed the structural screen, but the question shapes
        // are extension-supplied: a question the client's Zod would reject
        // must not pause the call on a form that never renders. Answer
        // empty instead so the tool's await unwinds.
        const questionsValid =
          Array.isArray(frame.questions) &&
          frame.questions.length > 0 &&
          frame.questions.every((q) => askUserQuestionSchema.safeParse(q).success);
        if (!questionsValid) {
          log.warn(
            `invalid ask_user_request from ${this.spec.extensionId}/${this.spec.tool.name}; answering empty`,
          );
          this.onEvent({
            kind: "log",
            level: "warn",
            message: "askUser request had invalid questions; answered empty",
          });
          this.worker.send({
            kind: "ask_user_response",
            callId: this.callId,
            requestId: frame.requestId,
            answers: [],
          });
          return;
        }
        // Pause the budget: subtract the time we've already consumed,
        // then disarm so the timer can be re-armed on response.
        this.beginUserWait();
        this.pendingAskRequests.add(frame.requestId);
        this.onEvent({
          kind: "ask_user_request",
          requestId: frame.requestId,
          questions: frame.questions,
        });
        return;
      }
      case "schedule_request": {
        // Same reasoning as askUser: a draft the client (or the insert
        // path) would reject must unblock the tool, not strand the call.
        const parsedDraft = scheduledPromptDraftSchema.safeParse(frame.draft);
        if (!parsedDraft.success) {
          log.warn(
            `invalid schedule_request draft from ${this.spec.extensionId}/${this.spec.tool.name}; rejecting`,
          );
          this.onEvent({
            kind: "log",
            level: "warn",
            message: "schedule proposal had an invalid draft; rejected",
          });
          this.worker.send({
            kind: "schedule_confirm_response",
            callId: this.callId,
            requestId: frame.requestId,
            accepted: false,
          });
          return;
        }
        // Same budget pause as askUser: the confirm form waits on the user.
        this.beginUserWait();
        this.pendingScheduleRequests.add(frame.requestId);
        this.onEvent({
          kind: "schedule_request",
          requestId: frame.requestId,
          draft: parsedDraft.data,
        });
        return;
      }
      case "permission_prompt":
        // Same budget pause as askUser: waiting on the user's decision
        // must not consume the tool's time budget. (Auto-denied prompts
        // never reach here; they settle inside the WorkerHandle within
        // about a second, against the running budget.)
        this.beginUserWait();
        this.pendingPermRequests.add(frame.requestId);
        this.onEvent({
          kind: "permission_request",
          requestId: frame.requestId,
          permission: frame.permission,
          resource: frame.resource,
          apiName: frame.apiName,
          declared: frame.declared,
          reason: frame.reason,
        });
        return;
      case "log":
        this.onEvent({
          kind: "log",
          level: frame.level,
          message: frame.message,
        });
        return;
      case "display": {
        const bounded = boundDisplayContent(frame.content);
        if ("error" in bounded) {
          log.warn(
            `dropping display from ${this.spec.extensionId}/${this.spec.tool.name}: ${bounded.error}`,
          );
          this.onEvent({
            kind: "log",
            level: "warn",
            message: `display dropped: ${bounded.error}`,
          });
          return;
        }
        this.onEvent({ kind: "display", content: bounded.content });
        return;
      }
      case "module_request": {
        const requestId = frame.requestId;
        const respondError = (error: string) =>
          this.worker.send({
            kind: "module_response",
            callId: this.callId,
            requestId,
            ok: false,
            error,
          });
        void handleModuleRequest({
          extensionId: this.spec.extensionId,
          toolName: this.spec.tool.name,
          callId: this.callId,
          module: frame.module,
          op: frame.op,
          args: frame.args,
          promptUser: this.promptUser,
        })
          .then(
            (result) => {
              try {
                this.worker.send({
                  kind: "module_response",
                  callId: this.callId,
                  requestId,
                  ok: true,
                  result,
                });
              } catch (err) {
                // The result slipped a non-JSON value (send serializes
                // before writing); fail the module call instead of
                // letting the worker's await hang.
                log.warn(
                  `module_response for ${this.spec.extensionId}/${this.spec.tool.name} not serializable: ${errMessage(
                    err,
                  )}`,
                );
                respondError("module result was not JSON-serializable");
              }
            },
            // Module errors flow back into extension code; scrub them like
            // log lines so a provider error can't leak a credential.
            (err) => respondError(scrubSecrets(errMessage(err))),
          )
          .catch((err) => {
            log.warn(`module_response delivery failed: ${errMessage(err)}`);
          });
        return;
      }
      case "stderr_log":
        this.onEvent({ kind: "stderr_log", line: frame.line });
        return;
      case "worker_exited":
        // The process died with a started call still open (crash, OOM, the
        // answer give-up kill, or a refreshPermissions teardown). Settle it
        // now instead of waiting for the call timeout. Pre-boot exits are
        // handled by the waitForBoot rejection below, so ignore those.
        if (this.settled || !this.callStarted) return;
        this.offHandler();
        this.timeout.disarm();
        this.settled = true;
        // Settle broker prompts still waiting on the user: their module
        // request can never complete on a dead worker, and an unanswered
        // promptUser promise would leak.
        for (const resolve of this.brokerPrompts.values()) resolve(false);
        this.brokerPrompts.clear();
        this.callbacks.onKilled();
        this.rejectDone(
          new AppError(
            "internal_error",
            this.cancelled ? "tool call cancelled" : "tool worker exited",
          ),
        );
        return;
      case "tool_result":
        this.offHandler();
        this.timeout.disarm();
        this.settled = true;
        this.callbacks.onSettled();
        this.resolveDone(frame.result);
        return;
      case "tool_error":
        this.offHandler();
        this.timeout.disarm();
        this.settled = true;
        this.callbacks.onSettled();
        // If we already emitted tool_cancelled, the consumer has moved
        // on; the worker's late tool_error is just bookkeeping. Reject
        // with the same "cancelled" message so callers waiting on
        // `done` see a consistent error class.
        if (this.cancelled) {
          this.rejectDone(new AppError("internal_error", "tool call cancelled"));
        } else {
          this.rejectDone(new AppError("provider_error", frame.error));
        }
        return;
    }
  }

  cancel(): void {
    if (this.cancelled) return;
    this.cancelled = true;
    // UI bubble transitions to cancelled immediately; the actual
    // worker abort + done-rejection happens via the worker's
    // tool_error frame (or the worker exits before then).
    try {
      this.onEvent({ kind: "tool_cancelled" });
    } catch {
      /* listener errors are non-fatal here */
    }
    // Settle any broker prompt still waiting on the user as rejected so
    // the module request resolves and the worker's await can unwind.
    for (const resolve of this.brokerPrompts.values()) resolve(false);
    this.brokerPrompts.clear();
    this.worker.send({ kind: "cancel", callId: this.callId });
    this.timeout.disarm();
    // If the worker doesn't ack the cancel within a short grace window
    // (wedged / CPU-bound, so the cooperative frame never runs), force-kill
    // it so `done` settles and the process stops consuming a core.
    setTimeout(() => {
      if (this.settled) return;
      this.settled = true;
      this.offHandler();
      this.callbacks.onKilled();
      this.rejectDone(new AppError("internal_error", "tool call cancelled"));
    }, this.drainTimeoutMs);
  }

  respondAskUser(requestId: string, answers: AskUserAnswer[]): void {
    if (!this.pendingAskRequests.delete(requestId)) return;
    this.worker.send({
      kind: "ask_user_response",
      callId: this.callId,
      requestId,
      answers,
    });
    // Resume the timer with the REMAINING budget instead of a fresh
    // callTimeoutMs window (once no sibling prompt is still open). Slow
    // user answers shouldn't extend the tool's effective time budget.
    this.endUserWait();
  }

  respondPermission(requestId: string, allow: boolean): void {
    const brokerResolve = this.brokerPrompts.get(requestId);
    if (brokerResolve) {
      this.brokerPrompts.delete(requestId);
      brokerResolve(allow);
    } else if (this.pendingPermRequests.delete(requestId)) {
      this.worker.answerPrompt(requestId, allow);
    } else {
      return;
    }
    // Same remaining-budget resume as askUser. The re-armed timer also
    // backstops the rare case where Deno never confirms the answer: the
    // handle kills the worker after its give-up window and the call
    // settles here via timeout.
    this.endUserWait();
  }

  respondSchedule(requestId: string, accepted: boolean, draft?: ScheduledPromptDraft): void {
    if (!this.pendingScheduleRequests.delete(requestId)) return;
    this.worker.send({
      kind: "schedule_confirm_response",
      callId: this.callId,
      requestId,
      accepted,
      draft,
    });
    // Same remaining-budget resume as askUser.
    this.endUserWait();
  }

  hasPendingSchedule(requestId: string): boolean {
    return this.pendingScheduleRequests.has(requestId);
  }
}

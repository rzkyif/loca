//! One in-flight tool call against a warm worker.
//!
//! Ports `packages/loca-core/src/extensions/worker-call.ts`.
//!
//! Owns the call's settle state, its pausable time budget ([`super::timeout`]),
//! the bookkeeping for every open user prompt (askUser, permission, schedule
//! confirmation, and the module broker's own permission prompts), and the
//! handler for everything the worker says.
//!
//! # What it deliberately does not own
//!
//! It never touches the pool's maps or the worker's in-flight count. Four
//! callbacks ([`PoolCallbacks`]) report each lifecycle transition back to the
//! pool, which owns both the accounting (so the count is released from exactly
//! one place per settle) and the worker's warm/retire/kill fate. A call that
//! decremented the count itself would double-decrement on any path that also
//! kills the worker, and a worker stuck at a negative in-flight count is a
//! worker eviction can never take.
//!
//! # The three invariants that are easy to lose
//!
//! - **The budget pauses on a REFCOUNT of open user waits, not a flag per
//!   kind.** Two prompts open at once, and a per-kind flag lets the first
//!   answer re-arm the timer while the second is still on the user's screen.
//! - **A `respond_*` naming an unknown request id is dropped whole.** Not
//!   forwarded-anyway, not treated as a cancel: a stale, replayed or forged
//!   response would otherwise resume the budget while the real prompt is still
//!   open, and the call would die under a form the user is still filling in.
//! - **Every terminal path unsubscribes, disarms and marks settled together.**
//!   They are the idempotency guard; a path that sets only one of the three
//!   settles twice, and the second settle reports a `killed` the pool acts on.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use crate::contract::api::{
    AskUserAnswer, AskUserQuestion, DisplayContent, PermissionKind, ScheduledPromptDraft,
};
use crate::error::{AppError, ErrorCode, Result};
use crate::log::scrub_secrets;

use super::display::bound_display_content;
use super::protocol::{ChatContext, LogLevel, ModuleName, PoolToWorker, WorkerEvent, WorkerToPool};
use super::timeout::CallTimeout;

/// What the pool was asked to run.
#[derive(Debug, Clone)]
pub struct ToolCallStart {
    pub extension_id: String,
    pub tool_name: String,
    /// The exported function inside the extension's entry module.
    pub fn_export: String,
    /// Already validated and defaulted by [`super::args`].
    pub arguments_json: String,
    pub chat_context: ChatContext,
}

/// Everything a caller sees while the call runs. Terminal outcomes arrive on
/// [`CallController::done`] instead.
#[derive(Debug, Clone)]
pub enum CallEvent {
    Progress {
        progress: f64,
        label: Option<String>,
        description: Option<String>,
    },
    AskUserRequest {
        request_id: String,
        questions: Vec<AskUserQuestion>,
    },
    /// The tool hit something outside its baked-in sandbox flags (a declared
    /// ask-state permission, or an undeclared one under an `ask` policy), or
    /// the module broker wants approval for a module the tool has not been
    /// granted. The call is paused on it; answer with
    /// [`CallController::respond_permission`].
    PermissionRequest {
        request_id: String,
        permission: PermissionKind,
        resource: String,
        api_name: Option<String>,
        declared: bool,
        reason: Option<String>,
    },
    /// The tool proposed a scheduled prompt. The call is paused until the user
    /// confirms, possibly after editing the draft, or rejects.
    ScheduleRequest {
        request_id: String,
        draft: ScheduledPromptDraft,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    StderrLog {
        line: String,
    },
    /// One way: the tool pushed a display bubble into chat. Nothing flows back.
    Display {
        content: DisplayContent,
    },
    /// Emitted the moment [`CallController::cancel`] is called, so the chat
    /// bubble can reach its terminal state without waiting for the worker to
    /// acknowledge. `done` still settles separately.
    ToolCancelled,
}

/// The lifecycle transitions the call reports to its pool.
pub trait PoolCallbacks: Send + Sync {
    /// Boot succeeded and the call frame went out: account the in-flight call
    /// and cancel any pending idle eviction.
    fn on_started(&self);
    /// A normal settle. Release the count and return the worker to the warm
    /// set, or retire it if it answered a prompt.
    fn on_settled(&self);
    /// A forced settle: a timeout, a worker exit, or a cancel the worker never
    /// acknowledged. Release the count and kill the worker, which is either
    /// wedged or already dead and must not be handed to the next call.
    fn on_killed(&self);
    /// Boot failed. Release the count and arm idle eviction.
    fn on_boot_failed(&self);
}

/// A permission the module broker wants before it will dispatch.
#[derive(Debug, Clone)]
pub struct ModulePrompt {
    pub permission: PermissionKind,
    pub resource: String,
    pub declared: bool,
    pub reason: Option<String>,
}

/// One gated call into a core module, on behalf of a running tool.
#[derive(Debug, Clone)]
pub struct ModuleCall {
    pub extension_id: String,
    pub tool_name: String,
    pub call_id: String,
    pub module: ModuleName,
    pub op: String,
    pub args: Value,
}

/// How the broker asks the user for a permission mid-dispatch.
///
/// The approval it returns is **call scoped**: it authorises this request and
/// is never written to the grants table. A port that persisted it would turn
/// one "allow this once" into a permanent grant with no user-visible record of
/// having done so.
#[async_trait]
pub trait ModulePrompter: Send + Sync {
    async fn ask(&self, prompt: ModulePrompt) -> bool;
}

/// The module broker, as the call sees it.
#[async_trait]
pub trait ModuleDispatch: Send + Sync {
    async fn dispatch(&self, call: ModuleCall, prompter: Arc<dyn ModulePrompter>) -> Result<Value>;
}

/// The slice of a worker one call drives.
///
/// A trait rather than [`super::handle::WorkerHandle`] so the state machine can
/// be tested against a recording worker on a paused clock. Every branch here is
/// a timing decision, and against a real `deno` none of them is observable.
#[async_trait]
pub trait CallWorker: Send + Sync {
    fn subscribe(&self) -> mpsc::UnboundedReceiver<WorkerEvent>;
    fn send(&self, frame: PoolToWorker);
    fn answer_prompt(&self, request_id: &str, allow: bool);
    async fn wait_for_boot(&self, budget: Duration) -> Result<()>;
}

#[async_trait]
impl CallWorker for super::handle::WorkerHandle {
    fn subscribe(&self) -> mpsc::UnboundedReceiver<WorkerEvent> {
        Self::subscribe(self)
    }
    fn send(&self, frame: PoolToWorker) {
        Self::send(self, frame);
    }
    fn answer_prompt(&self, request_id: &str, allow: bool) {
        Self::answer_prompt(self, request_id, allow);
    }
    async fn wait_for_boot(&self, budget: Duration) -> Result<()> {
        Self::wait_for_boot(self, budget).await
    }
}

/// What the driver is told from outside.
enum Command {
    Cancel,
    RespondAskUser {
        request_id: String,
        answers: Vec<AskUserAnswer>,
    },
    RespondPermission {
        request_id: String,
        allow: bool,
    },
    RespondSchedule {
        request_id: String,
        accepted: bool,
        draft: Option<Box<ScheduledPromptDraft>>,
    },
    /// The broker, reaching back through [`ModulePrompter`].
    BrokerPrompt {
        request_id: String,
        prompt: Box<ModulePrompt>,
        reply: oneshot::Sender<bool>,
    },
    /// A module dispatch finished. Carried through the driver rather than
    /// answered from its own task, so the reply cannot be written after the
    /// call has settled and the worker has been handed to somebody else.
    ModuleDone {
        request_id: String,
        outcome: Box<Result<Value>>,
    },
}

/// The caller's handle on a running call.
pub struct CallController {
    pub call_id: String,
    commands: mpsc::UnboundedSender<Command>,
    /// Shared with the driver so [`CallController::has_pending_schedule`] can
    /// answer synchronously. Callers with side effects keyed to a confirmation
    /// (chat persists the draft) must ask before acting: a replayed response is
    /// otherwise indistinguishable from the real one.
    pending_schedule: Arc<Mutex<HashSet<String>>>,
    /// Taken by the first [`CallController::done`]; `None` afterwards.
    done: Mutex<Option<oneshot::Receiver<Result<Value>>>>,
}

impl CallController {
    /// Ask the worker to stop.
    ///
    /// Returns at once. The chat bubble transitions on the
    /// [`CallEvent::ToolCancelled`] this emits synchronously; `done` settles
    /// when the worker acknowledges, or when the drain budget runs out and the
    /// worker is killed.
    pub fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel);
    }

    pub fn respond_ask_user(&self, request_id: &str, answers: Vec<AskUserAnswer>) {
        let _ = self.commands.send(Command::RespondAskUser {
            request_id: request_id.to_string(),
            answers,
        });
    }

    pub fn respond_permission(&self, request_id: &str, allow: bool) {
        let _ = self.commands.send(Command::RespondPermission {
            request_id: request_id.to_string(),
            allow,
        });
    }

    pub fn respond_schedule(
        &self,
        request_id: &str,
        accepted: bool,
        draft: Option<ScheduledPromptDraft>,
    ) {
        let _ = self.commands.send(Command::RespondSchedule {
            request_id: request_id.to_string(),
            accepted,
            draft: draft.map(Box::new),
        });
    }

    /// True while `request_id` is an unanswered schedule confirmation of THIS
    /// call.
    pub fn has_pending_schedule(&self, request_id: &str) -> bool {
        lock(&self.pending_schedule).contains(request_id)
    }

    /// The call's outcome: the tool's result, or the error it failed with.
    ///
    /// Takes the receiver, so it resolves once. A second await is a caller
    /// bug rather than a hang: it gets `internal_error` immediately.
    ///
    /// `&self` rather than `&mut self`, which is why the receiver sits behind a
    /// lock: the orchestrator holds this controller behind a `Box<dyn Trait>`
    /// shared with whatever answers the user's prompts, and a `&mut` outcome
    /// would mean nothing else could touch the call while a turn was awaiting
    /// it. The lock is never held across the await.
    pub async fn done(&self) -> Result<Value> {
        let taken = lock_outcome(&self.done).take();
        let Some(done) = taken else {
            return Err(AppError::new(
                ErrorCode::InternalError,
                "tool call outcome already taken",
            ));
        };
        done.await.unwrap_or_else(|_| {
            Err(AppError::new(
                ErrorCode::InternalError,
                "tool call driver stopped without settling",
            ))
        })
    }
}

/// The outcome receiver's lock. Poisoning cannot lose the receiver: a panic
/// while it is held would only mean the taker never awaited.
fn lock_outcome<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a cancelled worker gets to acknowledge before it is killed, and how
/// long a fresh worker gets to boot.
#[derive(Debug, Clone, Copy)]
pub struct CallBudgets {
    pub call_timeout: Duration,
    pub drain_timeout: Duration,
    pub boot_timeout: Duration,
}

impl Default for CallBudgets {
    fn default() -> Self {
        let pool = super::pool::DEFAULT_POOL_CONFIG;
        Self {
            call_timeout: pool.call_timeout,
            drain_timeout: pool.drain_timeout,
            boot_timeout: super::BOOT_TIMEOUT,
        }
    }
}

/// Start one tool call and return its controller.
pub fn start_call(
    call_id: String,
    worker: Arc<dyn CallWorker>,
    spec: ToolCallStart,
    events: mpsc::UnboundedSender<CallEvent>,
    callbacks: Arc<dyn PoolCallbacks>,
    modules: Option<Arc<dyn ModuleDispatch>>,
    budgets: CallBudgets,
) -> CallController {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (done_tx, done_rx) = oneshot::channel();
    let pending_schedule = Arc::new(Mutex::new(HashSet::new()));

    // Subscribe BEFORE the driver is spawned and before boot is awaited. A
    // subscription taken after the first await misses whatever the worker said
    // in between, which reproduces about one run in ten and looks like a hung
    // tool rather than a lost frame.
    let incoming = worker.subscribe();

    let driver = Driver {
        call_id: call_id.clone(),
        worker: Arc::clone(&worker),
        spec,
        events,
        callbacks,
        modules,
        budgets,
        timeout: CallTimeout::new(budgets.call_timeout),
        commands: command_tx.clone(),
        pending_schedule: Arc::clone(&pending_schedule),
        pending_ask: HashSet::new(),
        pending_permission: HashSet::new(),
        broker_prompts: HashMap::new(),
        open_user_waits: 0,
        cancelled: false,
        cancel_deadline: None,
    };
    tokio::spawn(driver.run(incoming, command_rx, done_tx));

    CallController {
        call_id,
        commands: command_tx,
        pending_schedule,
        done: Mutex::new(Some(done_rx)),
    }
}

/// The [`ModulePrompter`] the broker is handed: it posts a command to the
/// driver and waits for the user's answer to come back.
struct DriverPrompter {
    commands: mpsc::UnboundedSender<Command>,
}

#[async_trait]
impl ModulePrompter for DriverPrompter {
    async fn ask(&self, prompt: ModulePrompt) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        // `mod-` is load-bearing, not decoration: module prompts reuse the
        // `tool.permission_request` frame rather than defining their own, so
        // the prefix is how a response is routed back to the broker rather
        // than to the worker's terminal.
        let request_id = format!("mod-{}", uuid::Uuid::new_v4());
        if self
            .commands
            .send(Command::BrokerPrompt {
                request_id,
                prompt: Box::new(prompt),
                reply: reply_tx,
            })
            .is_err()
        {
            return false;
        }
        // A dropped sender means the call settled without answering, which is
        // a refusal: the safe direction for a permission nobody granted.
        reply_rx.await.unwrap_or(false)
    }
}

struct Driver {
    call_id: String,
    worker: Arc<dyn CallWorker>,
    spec: ToolCallStart,
    events: mpsc::UnboundedSender<CallEvent>,
    callbacks: Arc<dyn PoolCallbacks>,
    modules: Option<Arc<dyn ModuleDispatch>>,
    budgets: CallBudgets,
    timeout: CallTimeout,
    commands: mpsc::UnboundedSender<Command>,
    pending_schedule: Arc<Mutex<HashSet<String>>>,
    pending_ask: HashSet<String>,
    pending_permission: HashSet<String>,
    broker_prompts: HashMap<String, oneshot::Sender<bool>>,
    /// How many prompts are on the user's screen. A count rather than a flag
    /// per kind: see the module header.
    open_user_waits: usize,
    cancelled: bool,
    /// When a cancelled worker stops getting the benefit of the doubt.
    ///
    /// A deadline the main loop races rather than a sleep inside the cancel
    /// handler: during the drain the worker may still acknowledge with
    /// `tool_error`, and a handler that blocked here would not be listening
    /// when it did. The call would then be killed, and its worker discarded,
    /// for having answered.
    cancel_deadline: Option<tokio::time::Instant>,
}

/// Why the driver stopped, and what the pool should do about the worker.
enum Settle {
    /// The tool answered. The worker goes back to the warm set.
    Ok(Value),
    /// The tool failed on its own terms. The worker is still healthy.
    Failed(AppError),
    /// The worker is wedged or dead. Kill it.
    Kill(AppError),
    /// Boot never completed.
    BootFailed(AppError),
}

impl Driver {
    async fn run(
        mut self,
        mut incoming: mpsc::UnboundedReceiver<WorkerEvent>,
        mut commands: mpsc::UnboundedReceiver<Command>,
        done: oneshot::Sender<Result<Value>>,
    ) {
        let settle = self.boot_and_drive(&mut incoming, &mut commands).await;

        // Broker prompts still waiting on the user cannot be answered on a
        // settled call, and an unanswered one leaves the broker's task parked
        // forever holding whatever it was about to do.
        //
        // Deleting this loop currently changes nothing observable, and that is
        // recorded rather than acted on: dropping the driver drops these
        // senders, `ask` maps a dropped sender to `false`, and the outcome is
        // identical. It is kept because that equivalence is a property of drop
        // order rather than of this code, and the first change that keeps the
        // driver alive past a settle would turn every open broker prompt into a
        // permanently parked task with nothing failing to say so.
        for reply in std::mem::take(&mut self.broker_prompts).into_values() {
            let _ = reply.send(false);
        }
        lock(&self.pending_schedule).clear();

        let outcome = match settle {
            Settle::Ok(value) => {
                self.callbacks.on_settled();
                Ok(value)
            }
            Settle::Failed(err) => {
                self.callbacks.on_settled();
                Err(err)
            }
            Settle::Kill(err) => {
                self.callbacks.on_killed();
                Err(err)
            }
            Settle::BootFailed(err) => {
                self.callbacks.on_boot_failed();
                Err(err)
            }
        };
        let _ = done.send(outcome);
    }

    async fn boot_and_drive(
        &mut self,
        incoming: &mut mpsc::UnboundedReceiver<WorkerEvent>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Settle {
        // Boot is raced against a cancel, so a call cancelled while its worker
        // is still starting does not sit out the whole boot budget first.
        let booted = tokio::select! {
            result = self.worker.wait_for_boot(self.budgets.boot_timeout) => result,
            () = wait_for_cancel(commands) => {
                return Settle::BootFailed(AppError::new(
                    ErrorCode::InternalError,
                    "cancelled before boot",
                ));
            }
        };
        if let Err(err) = booted {
            // `on_boot_failed` releases no count here, because `on_started`
            // never ran; what it does is arm idle eviction, so a worker that
            // failed to boot does not sit in the warm set with no timer on it
            // waiting to be pushed out by LRU pressure that may never come.
            // Synthesize a log event as well as failing: without a terminal
            // event of some kind the chat bubble stays in "running" forever,
            // and a boot failure is exactly when a user most needs to see why.
            self.emit(CallEvent::Log {
                level: LogLevel::Error,
                message: format!("worker boot failed: {}", err.message),
            });
            return Settle::BootFailed(err);
        }

        self.callbacks.on_started();
        self.worker.send(PoolToWorker::Call {
            call_id: self.call_id.clone(),
            tool_name: self.spec.tool_name.clone(),
            fn_export: self.spec.fn_export.clone(),
            arguments: self.spec.arguments_json.clone(),
            chat_context: self.spec.chat_context.clone(),
        });
        self.timeout.arm();

        loop {
            let deadline = self.timeout.deadline();
            tokio::select! {
                event = incoming.recv() => {
                    let Some(event) = event else {
                        // The worker's driver stopped without an exit event.
                        return Settle::Kill(AppError::new(
                            ErrorCode::InternalError,
                            "tool worker stopped",
                        ));
                    };
                    if let Some(settle) = self.on_worker_event(event) {
                        return settle;
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        // Every controller was dropped. Nobody can answer a
                        // prompt or read the result, so the call is abandoned.
                        return Settle::Kill(AppError::new(
                            ErrorCode::InternalError,
                            "tool call abandoned",
                        ));
                    };
                    if let Some(settle) = self.on_command(command) {
                        return settle;
                    }
                }
                () = sleep_until(deadline) => {
                    if let Some(settle) = self.on_budget_expired() {
                        return settle;
                    }
                }
                () = sleep_until(self.cancel_deadline) => {
                    // The cancel frame was never acknowledged, so the worker is
                    // wedged or CPU bound and the cooperative path is not
                    // coming. Kill it, or `done` never settles at all.
                    return Settle::Kill(AppError::new(
                        ErrorCode::InternalError,
                        "tool call cancelled",
                    ));
                }
            }
        }
    }

    // --- the budget ---------------------------------------------------------

    fn begin_user_wait(&mut self) {
        self.open_user_waits += 1;
        self.timeout.pause();
    }

    fn end_user_wait(&mut self) {
        self.open_user_waits = self.open_user_waits.saturating_sub(1);
        if self.open_user_waits == 0 && !self.timeout.armed() && !self.cancelled {
            self.timeout.arm();
        }
    }

    fn on_budget_expired(&mut self) -> Option<Settle> {
        self.timeout.expire();
        if self.open_user_waits > 0 {
            // Defensive: with the refcount the budget should never run out
            // while a prompt is open. `expire` has cleared the anchor, so the
            // next `end_user_wait` re-arms with whatever remains.
            return None;
        }
        // A cooperative `cancel` frame is useless against a CPU-bound or wedged
        // tool: it never reaches the worker's event loop. Kill the process so
        // it stops burning a core, rather than leaving it warm for the five
        // minutes until idle eviction.
        Some(Settle::Kill(AppError::new(
            ErrorCode::InternalError,
            "tool call timed out",
        )))
    }

    // --- what the worker says -----------------------------------------------

    fn on_worker_event(&mut self, event: WorkerEvent) -> Option<Settle> {
        match event {
            WorkerEvent::StderrLog { line } => {
                // Deliberately NOT filtered by call id: stderr is the worker's,
                // not any one call's, and the alternative is silently dropping
                // the diagnostics a failing tool produces.
                self.emit(CallEvent::StderrLog { line });
                None
            }
            WorkerEvent::WorkerExited { .. } => {
                // No pre-boot guard here, deliberately: this loop is only
                // entered once boot has SUCCEEDED, so an exit before the call
                // started is reported by `wait_for_boot` and never reaches this
                // arm. A guard for it would read as a control and be incapable
                // of firing.
                Some(Settle::Kill(AppError::new(
                    ErrorCode::InternalError,
                    if self.cancelled {
                        "tool call cancelled"
                    } else {
                        "tool worker exited"
                    },
                )))
            }
            WorkerEvent::PermissionPrompt {
                request_id,
                permission,
                resource,
                api_name,
                declared,
                reason,
            } => {
                // The same budget pause as askUser. An auto-denied prompt never
                // reaches here: the handle settles it in about a second,
                // against the running budget, because nobody was asked.
                self.begin_user_wait();
                self.pending_permission.insert(request_id.clone());
                self.emit(CallEvent::PermissionRequest {
                    request_id,
                    permission,
                    resource,
                    api_name,
                    declared,
                    reason,
                });
                None
            }
            WorkerEvent::Frame(frame) => self.on_frame(frame),
        }
    }

    fn on_frame(&mut self, frame: WorkerToPool) -> Option<Settle> {
        // A frame for somebody else's call. One worker can hold more than one
        // call, and a frame delivered to the wrong one settles the wrong call.
        if frame.call_id().is_some_and(|id| id != self.call_id) {
            return None;
        }
        match frame {
            WorkerToPool::Ready | WorkerToPool::Booted { .. } | WorkerToPool::BootFailed { .. } => {
                None
            }
            WorkerToPool::Progress {
                progress,
                label,
                description,
                ..
            } => {
                self.emit(CallEvent::Progress {
                    progress,
                    label,
                    description,
                });
                None
            }
            WorkerToPool::Log { level, message, .. } => {
                self.emit(CallEvent::Log { level, message });
                None
            }
            WorkerToPool::Display { content, .. } => {
                match bound_display_content(content) {
                    Ok(content) => self.emit(CallEvent::Display { content }),
                    Err(reason) => {
                        tracing::warn!(
                            target: "loca::extensions",
                            extension = self.spec.extension_id,
                            tool = self.spec.tool_name,
                            "dropping an oversize display payload: {reason}",
                        );
                        self.emit(CallEvent::Log {
                            level: LogLevel::Warn,
                            message: format!("display dropped: {reason}"),
                        });
                    }
                }
                None
            }
            WorkerToPool::AskUserRequest {
                request_id,
                questions,
                ..
            } => {
                self.on_ask_user(request_id, questions);
                None
            }
            WorkerToPool::ScheduleRequest {
                request_id, draft, ..
            } => {
                self.on_schedule_request(request_id, draft);
                None
            }
            WorkerToPool::ModuleRequest {
                request_id,
                module,
                op,
                args,
                ..
            } => {
                self.on_module_request(request_id, module, op, args);
                None
            }
            WorkerToPool::ToolResult { result, .. } => Some(Settle::Ok(result)),
            WorkerToPool::ToolError { error, .. } => Some(if self.cancelled {
                // The consumer already saw `tool_cancelled` and moved on; the
                // worker's late error is bookkeeping. Reporting the same
                // "cancelled" keeps the error class consistent for whoever is
                // awaiting `done`.
                Settle::Failed(AppError::new(
                    ErrorCode::InternalError,
                    "tool call cancelled",
                ))
            } else {
                Settle::Failed(AppError::new(ErrorCode::ProviderError, error))
            }),
        }
    }

    fn on_ask_user(&mut self, request_id: String, questions: Vec<Value>) {
        // The frame passed the structural screen, but the question shapes are
        // extension supplied. A question the client's schema would reject must
        // not pause the call on a form that never renders, so it is ANSWERED
        // empty and the tool's await unwinds. See D-054 for why that means the
        // payload arrives untyped.
        let typed: Option<Vec<AskUserQuestion>> = if questions.is_empty() {
            None
        } else {
            questions
                .into_iter()
                .map(|q| serde_json::from_value::<AskUserQuestion>(q).ok())
                .collect()
        };
        let Some(questions) = typed else {
            tracing::warn!(
                target: "loca::extensions",
                extension = self.spec.extension_id,
                tool = self.spec.tool_name,
                "invalid ask_user_request; answering empty",
            );
            self.emit(CallEvent::Log {
                level: LogLevel::Warn,
                message: "askUser request had invalid questions; answered empty".to_string(),
            });
            self.worker.send(PoolToWorker::AskUserResponse {
                call_id: self.call_id.clone(),
                request_id,
                answers: Vec::new(),
            });
            return;
        };
        self.begin_user_wait();
        self.pending_ask.insert(request_id.clone());
        self.emit(CallEvent::AskUserRequest {
            request_id,
            questions,
        });
    }

    fn on_schedule_request(&mut self, request_id: String, draft: Value) {
        // Same reasoning as askUser: a draft the client, or the insert path,
        // would reject must unblock the tool rather than strand the call.
        let Ok(draft) = serde_json::from_value::<ScheduledPromptDraft>(draft) else {
            tracing::warn!(
                target: "loca::extensions",
                extension = self.spec.extension_id,
                tool = self.spec.tool_name,
                "invalid schedule_request draft; rejecting",
            );
            self.emit(CallEvent::Log {
                level: LogLevel::Warn,
                message: "schedule proposal had an invalid draft; rejected".to_string(),
            });
            self.worker.send(PoolToWorker::ScheduleConfirmResponse {
                call_id: self.call_id.clone(),
                request_id,
                accepted: false,
                draft: None,
            });
            return;
        };
        self.begin_user_wait();
        lock(&self.pending_schedule).insert(request_id.clone());
        self.emit(CallEvent::ScheduleRequest { request_id, draft });
    }

    fn on_module_request(
        &mut self,
        request_id: String,
        module: ModuleName,
        op: String,
        args: Value,
    ) {
        let Some(modules) = self.modules.clone() else {
            // No broker wired. Answering the error is what matters: the tool is
            // awaiting this reply and silence is a hang.
            self.worker.send(PoolToWorker::ModuleResponse {
                call_id: self.call_id.clone(),
                request_id,
                ok: false,
                result: None,
                error: Some("core modules are unavailable".to_string()),
            });
            return;
        };
        let call = ModuleCall {
            extension_id: self.spec.extension_id.clone(),
            tool_name: self.spec.tool_name.clone(),
            call_id: self.call_id.clone(),
            module,
            op,
            args,
        };
        let prompter: Arc<dyn ModulePrompter> = Arc::new(DriverPrompter {
            commands: self.commands.clone(),
        });
        let commands = self.commands.clone();
        // Concurrently, because a module call can be slow (an LLM completion, a
        // transcription) and the tool may have several in flight. The reply is
        // routed back through the driver rather than written from here, so it
        // cannot land on a worker this call no longer owns.
        tokio::spawn(async move {
            let outcome = modules.dispatch(call, prompter).await;
            let _ = commands.send(Command::ModuleDone {
                request_id,
                outcome: Box::new(outcome),
            });
        });
    }

    fn on_module_done(&mut self, request_id: String, outcome: Result<Value>) {
        let (ok, result, error) = match outcome {
            Ok(value) => (true, Some(value), None),
            // The error re-enters extension code, where a third-party tool can
            // read it and send it anywhere. Scrub it exactly as a log line is
            // scrubbed: a provider error naming a URL, a path or a token is the
            // ordinary case, not the exotic one.
            Err(err) => (false, None, Some(scrub_secrets(&err.message))),
        };
        self.worker.send(PoolToWorker::ModuleResponse {
            call_id: self.call_id.clone(),
            request_id,
            ok,
            result,
            error,
        });
    }

    // --- what the caller says -----------------------------------------------

    fn on_command(&mut self, command: Command) -> Option<Settle> {
        match command {
            Command::Cancel => self.on_cancel(),
            Command::RespondAskUser {
                request_id,
                answers,
            } => {
                // Dropped whole when the id is unknown: see the module header.
                if !self.pending_ask.remove(&request_id) {
                    return None;
                }
                self.worker.send(PoolToWorker::AskUserResponse {
                    call_id: self.call_id.clone(),
                    request_id,
                    answers,
                });
                // Resume with what REMAINS, not a fresh full window. A slow
                // human answer must not extend the tool's compute budget.
                self.end_user_wait();
                None
            }
            Command::RespondPermission { request_id, allow } => {
                if let Some(reply) = self.broker_prompts.remove(&request_id) {
                    let _ = reply.send(allow);
                } else if self.pending_permission.remove(&request_id) {
                    self.worker.answer_prompt(&request_id, allow);
                } else {
                    return None;
                }
                self.end_user_wait();
                None
            }
            Command::RespondSchedule {
                request_id,
                accepted,
                draft,
            } => {
                if !lock(&self.pending_schedule).remove(&request_id) {
                    return None;
                }
                self.worker.send(PoolToWorker::ScheduleConfirmResponse {
                    call_id: self.call_id.clone(),
                    request_id,
                    accepted,
                    draft: draft.map(|d| *d),
                });
                self.end_user_wait();
                None
            }
            Command::BrokerPrompt {
                request_id,
                prompt,
                reply,
            } => {
                self.begin_user_wait();
                self.broker_prompts.insert(request_id.clone(), reply);
                self.emit(CallEvent::PermissionRequest {
                    request_id,
                    permission: prompt.permission,
                    resource: prompt.resource,
                    api_name: None,
                    declared: prompt.declared,
                    reason: prompt.reason,
                });
                None
            }
            Command::ModuleDone {
                request_id,
                outcome,
            } => {
                self.on_module_done(request_id, *outcome);
                None
            }
        }
    }

    fn on_cancel(&mut self) -> Option<Settle> {
        if self.cancelled {
            return None;
        }
        self.cancelled = true;
        self.emit(CallEvent::ToolCancelled);
        // Release every broker prompt as refused, so the module request
        // resolves and the tool's await unwinds instead of parking.
        for reply in std::mem::take(&mut self.broker_prompts).into_values() {
            let _ = reply.send(false);
        }
        self.worker.send(PoolToWorker::Cancel {
            call_id: self.call_id.clone(),
        });
        self.timeout.disarm();
        self.cancel_deadline = Some(tokio::time::Instant::now() + self.budgets.drain_timeout);
        None
    }

    fn emit(&self, event: CallEvent) {
        let _ = self.events.send(event);
    }
}

/// Resolve when a [`Command::Cancel`] arrives, ignoring anything else.
///
/// Only used while boot is outstanding: no prompt can be open yet, and a
/// response naming a request id that does not exist is dropped whole anyway.
async fn wait_for_cancel(commands: &mut mpsc::UnboundedReceiver<Command>) {
    while let Some(command) = commands.recv().await {
        if matches!(command, Command::Cancel) {
            return;
        }
    }
    std::future::pending().await
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- the doubles --------------------------------------------------------

    #[derive(Default)]
    struct RecordingWorker {
        sent: Mutex<Vec<PoolToWorker>>,
        answered: Mutex<Vec<(String, bool)>>,
        listeners: Mutex<Vec<mpsc::UnboundedSender<WorkerEvent>>>,
        boot: Mutex<Option<String>>,
        /// Boot that never resolves, so the boot race itself is testable.
        boot_hangs: Mutex<bool>,
    }

    impl RecordingWorker {
        fn feed(&self, event: WorkerEvent) {
            lock(&self.listeners).retain(|tx| tx.send(event.clone()).is_ok());
        }

        fn frame(&self, frame: WorkerToPool) {
            self.feed(WorkerEvent::Frame(frame));
        }

        fn sent(&self) -> Vec<PoolToWorker> {
            lock(&self.sent).iter().cloned().collect()
        }

        fn sent_kinds(&self) -> Vec<String> {
            self.sent()
                .iter()
                .map(|f| {
                    serde_json::to_value(f).unwrap()["kind"]
                        .as_str()
                        .unwrap_or("?")
                        .to_string()
                })
                .collect()
        }
    }

    #[async_trait]
    impl CallWorker for RecordingWorker {
        fn subscribe(&self) -> mpsc::UnboundedReceiver<WorkerEvent> {
            let (tx, rx) = mpsc::unbounded_channel();
            lock(&self.listeners).push(tx);
            rx
        }
        fn send(&self, frame: PoolToWorker) {
            lock(&self.sent).push(frame);
        }
        fn answer_prompt(&self, request_id: &str, allow: bool) {
            lock(&self.answered).push((request_id.to_string(), allow));
        }
        async fn wait_for_boot(&self, _budget: Duration) -> Result<()> {
            if *lock(&self.boot_hangs) {
                std::future::pending::<()>().await;
            }
            match lock(&self.boot).clone() {
                Some(error) => Err(AppError::new(ErrorCode::InternalError, error)),
                None => Ok(()),
            }
        }
    }

    #[derive(Default)]
    struct Counts {
        started: AtomicUsize,
        settled: AtomicUsize,
        killed: AtomicUsize,
        boot_failed: AtomicUsize,
    }

    impl PoolCallbacks for Counts {
        fn on_started(&self) {
            self.started.fetch_add(1, Ordering::SeqCst);
        }
        fn on_settled(&self) {
            self.settled.fetch_add(1, Ordering::SeqCst);
        }
        fn on_killed(&self) {
            self.killed.fetch_add(1, Ordering::SeqCst);
        }
        fn on_boot_failed(&self) {
            self.boot_failed.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Counts {
        fn get(&self) -> (usize, usize, usize, usize) {
            (
                self.started.load(Ordering::SeqCst),
                self.settled.load(Ordering::SeqCst),
                self.killed.load(Ordering::SeqCst),
                self.boot_failed.load(Ordering::SeqCst),
            )
        }
    }

    /// A broker stand-in. `prompt` makes it ask before answering, which is the
    /// only way the call's broker-prompt path is reachable.
    struct FakeModules {
        prompt: Option<ModulePrompt>,
        outcome: Mutex<Option<Result<Value>>>,
        approved: Mutex<Option<bool>>,
    }

    impl FakeModules {
        fn returning(value: Value) -> Arc<Self> {
            Arc::new(Self {
                prompt: None,
                outcome: Mutex::new(Some(Ok(value))),
                approved: Mutex::new(None),
            })
        }
        fn failing(message: &str) -> Arc<Self> {
            Arc::new(Self {
                prompt: None,
                outcome: Mutex::new(Some(Err(AppError::new(ErrorCode::ProviderError, message)))),
                approved: Mutex::new(None),
            })
        }
        fn prompting() -> Arc<Self> {
            Arc::new(Self {
                prompt: Some(ModulePrompt {
                    permission: PermissionKind::Read,
                    resource: "memories".to_string(),
                    declared: false,
                    reason: Some("to read your notes".to_string()),
                }),
                outcome: Mutex::new(Some(Ok(Value::from(1)))),
                approved: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl ModuleDispatch for FakeModules {
        async fn dispatch(
            &self,
            _call: ModuleCall,
            prompter: Arc<dyn ModulePrompter>,
        ) -> Result<Value> {
            if let Some(prompt) = self.prompt.clone() {
                let allowed = prompter.ask(prompt).await;
                *lock(&self.approved) = Some(allowed);
                if !allowed {
                    return Err(AppError::new(ErrorCode::Forbidden, "permission refused"));
                }
            }
            lock(&self.outcome).take().unwrap_or(Ok(Value::Null))
        }
    }

    // --- the fixture --------------------------------------------------------

    struct Fixture {
        worker: Arc<RecordingWorker>,
        counts: Arc<Counts>,
        events: mpsc::UnboundedReceiver<CallEvent>,
        call: CallController,
    }

    impl Fixture {
        /// The call's outcome, bounded.
        ///
        /// Every way a test in this module can hang ends here, waiting on a
        /// settle that is never coming, so this is where the bound belongs: a
        /// wrapper each test has to remember is the same forgettable hazard as
        /// the `advance` one above, and the test that forgets is exactly the
        /// test that hangs.
        ///
        /// A hanging test is strictly worse than a failing one. `cargo test`
        /// has no per-test timeout, the whole crate shares one test binary, and
        /// a wedged run takes every other package's suite down with it, which
        /// is not hypothetical: it cost the fleet four stacked runs and an
        /// orphaned binary.
        ///
        /// The budget is wall clock and enormous next to what these tests need,
        /// which run on a virtual clock and finish in milliseconds, so it can
        /// only fire on a real deadlock. It rides the same runtime deliberately:
        /// a separate thread would defeat `start_paused`, and this works because
        /// a deadlocked driver leaves the runtime idle, which is precisely when
        /// a paused clock auto-advances to this timer.
        async fn outcome(&mut self) -> Result<Value> {
            match tokio::time::timeout(Duration::from_secs(600), self.call.done()).await {
                Ok(result) => result,
                Err(_) => panic!("the call never settled"),
            }
        }

        fn drain(&mut self) -> Vec<CallEvent> {
            let mut out = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                out.push(event);
            }
            out
        }
    }

    fn spec() -> ToolCallStart {
        ToolCallStart {
            extension_id: "demo".to_string(),
            tool_name: "search".to_string(),
            fn_export: "default".to_string(),
            arguments_json: r#"{"q":"cats"}"#.to_string(),
            chat_context: ChatContext {
                user_message: "find cats".to_string(),
                session_id: Some("s1".to_string()),
                locale: None,
            },
        }
    }

    /// The budgets the product actually ships, not a set restated here.
    ///
    /// Restating them made `CallBudgets::default()` unpinned: every timing test
    /// below asserted against its own copy of 60s / 2s / 10s, so doubling any
    /// of the real defaults changed what users get and broke nothing. Taking
    /// the real values means the same tests now guard them.
    fn budgets() -> CallBudgets {
        CallBudgets::default()
    }

    fn start(worker: Arc<RecordingWorker>, modules: Option<Arc<dyn ModuleDispatch>>) -> Fixture {
        let counts = Arc::new(Counts::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let call = start_call(
            "call-1".to_string(),
            Arc::clone(&worker) as Arc<dyn CallWorker>,
            spec(),
            tx,
            Arc::clone(&counts) as Arc<dyn PoolCallbacks>,
            modules,
            budgets(),
        );
        Fixture {
            worker,
            counts,
            events: rx,
            call,
        }
    }

    fn plain() -> Fixture {
        start(Arc::new(RecordingWorker::default()), None)
    }

    /// A real sleep, not a `yield_now`. Under a paused runtime the clock only
    /// advances when every task is idle, and a message crossing two channels
    /// needs the driver polled more than once; a yield does not guarantee that
    /// and the resulting failures look like the behaviour under test.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    /// Move the paused clock, settling on BOTH sides of the jump.
    ///
    /// The trailing settle is so the driver has processed whatever the jump
    /// woke. The LEADING one is the subtle half: `advance` jumps in a single
    /// hop, so any deadline the driver has not armed yet gets anchored at the
    /// far side of the jump and the budget is measured from the wrong instant.
    /// A caller that cancels and advances in the same breath then waits on a
    /// kill that was scheduled two seconds after the clock already moved, and
    /// the test does not fail: it HANGS, taking the whole crate's test binary
    /// and every other agent's runs with it.
    ///
    /// That happened, so the settle lives in the helper rather than at the call
    /// sites: two callers needed it, one had it, and there is no way to tell
    /// them apart by reading. Anything that genuinely needs to observe the
    /// un-settled state calls `tokio::time::advance` directly and says why.
    async fn advance(secs: u64) {
        settle().await;
        tokio::time::advance(Duration::from_secs(secs)).await;
        settle().await;
    }

    fn result_frame() -> WorkerToPool {
        WorkerToPool::ToolResult {
            call_id: "call-1".to_string(),
            result: Value::from("found"),
        }
    }

    // --- the happy path -----------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn the_call_frame_carries_the_tool_its_export_and_its_arguments() {
        // The worker runtime reads these by name. A wrong export runs the wrong
        // function, and wrong arguments run the right one on nothing.
        let f = plain();
        settle().await;
        let sent = f.worker.sent();
        assert_eq!(sent.len(), 1);
        let value = serde_json::to_value(&sent[0]).unwrap();
        assert_eq!(value["kind"], "call");
        assert_eq!(value["callId"], "call-1");
        assert_eq!(value["toolName"], "search");
        assert_eq!(value["fnExport"], "default");
        assert_eq!(value["arguments"], r#"{"q":"cats"}"#);
        assert_eq!(value["chatContext"]["userMessage"], "find cats");
        assert_eq!(f.counts.get(), (1, 0, 0, 0));
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_result_settles_the_call_and_leaves_the_worker_reusable() {
        let mut f = plain();
        settle().await;
        f.worker.frame(result_frame());
        let out = f.outcome().await.unwrap();
        assert_eq!(out, Value::from("found"));
        // settled, NOT killed: the worker answered and is healthy.
        assert_eq!(f.counts.get(), (1, 1, 0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_error_settles_the_call_without_discarding_the_worker() {
        // A tool that threw is not a worker that is broken. Killing it here
        // would respawn a Deno process on every failing tool call.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::ToolError {
            call_id: "call-1".to_string(),
            error: "no results".to_string(),
        });
        let err = f.outcome().await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ProviderError);
        assert_eq!(err.message, "no results");
        assert_eq!(f.counts.get(), (1, 1, 0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_frame_for_a_different_call_on_the_same_worker_is_ignored() {
        // One worker can hold more than one call. A result routed to the wrong
        // one settles a call that is still running and drops the real answer.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::ToolResult {
            call_id: "someone-else".to_string(),
            result: Value::from("wrong"),
        });
        settle().await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the call settled on a foreign frame"
        );
        f.worker.frame(result_frame());
        assert_eq!(f.outcome().await.unwrap(), Value::from("found"));
    }

    #[tokio::test(start_paused = true)]
    async fn stderr_reaches_the_call_even_though_it_names_no_call() {
        // The one event deliberately not filtered by call id. Filtering it out
        // silently discards the diagnostics of every failing tool.
        let mut f = plain();
        settle().await;
        f.worker.feed(WorkerEvent::StderrLog {
            line: "TypeError: x is not a function".to_string(),
        });
        settle().await;
        let events = f.drain();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, CallEvent::StderrLog { line } if line.contains("TypeError"))),
            "stderr was dropped: {events:?}"
        );
        f.call.cancel();
    }

    // --- boot ---------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_boot_failure_reports_a_terminal_event_and_never_runs_the_tool() {
        // Without the synthesized log the chat bubble stays in "running"
        // forever, because no worker frame is ever coming.
        let worker = Arc::new(RecordingWorker::default());
        *lock(&worker.boot) = Some("entry not found".to_string());
        let mut f = start(worker, None);
        let err = f.outcome().await.unwrap_err();
        assert!(err.message.contains("entry not found"), "{}", err.message);
        assert_eq!(f.counts.get(), (0, 0, 0, 1));
        assert!(
            f.worker.sent().is_empty(),
            "a call frame went to a worker that never booted"
        );
        let events = f.drain();
        assert!(
            events.iter().any(|e| matches!(
                e,
                CallEvent::Log {
                    level: LogLevel::Error,
                    ..
                }
            )),
            "no terminal event for the bubble: {events:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_during_boot_settles_at_once_rather_than_waiting_it_out() {
        // The boot budget is ten seconds. A user who cancels must not watch
        // out a spawn they already abandoned.
        let worker = Arc::new(RecordingWorker::default());
        *lock(&worker.boot_hangs) = true;
        let mut f = start(worker, None);
        settle().await;
        f.call.cancel();
        // Bounded, because the failure being guarded against is a call that
        // waits out a boot that never finishes. Without the bound this test
        // hangs the suite rather than failing it, which reads as broken
        // infrastructure instead of the bug.
        let err = tokio::time::timeout(Duration::from_secs(30), f.outcome())
            .await
            .expect("the cancel waited on a boot that never came")
            .unwrap_err();
        assert_eq!(err.message, "cancelled before boot");
        assert!(
            f.worker.sent().is_empty(),
            "the tool ran after being cancelled"
        );
    }

    // --- the budget ---------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_call_that_outruns_its_budget_kills_its_worker() {
        // A cooperative cancel cannot reach a CPU-bound isolate, so the only
        // way to stop it burning a core is to kill the process.
        let mut f = plain();
        settle().await;
        advance(61).await;
        // Before the await, for the reason given in the resume test: awaiting
        // first lets the clock run on to any later deadline, so a doubled
        // budget would pass this too.
        assert_eq!(
            f.counts.get(),
            (1, 0, 1, 0),
            "the wedged worker was kept warm"
        );
        assert_eq!(
            f.outcome().await.unwrap_err().message,
            "tool call timed out"
        );
    }

    #[test]
    fn the_shipped_budgets_are_the_pools_own() {
        // The three numbers the timing tests above are written around. Pinned
        // against the pool's config rather than restated, because the call and
        // the pool holding different values for the same budget is invisible
        // until a call outlives the worker it is running on.
        let budgets = CallBudgets::default();
        assert_eq!(budgets.call_timeout, Duration::from_secs(60));
        assert_eq!(budgets.drain_timeout, Duration::from_secs(2));
        assert_eq!(budgets.boot_timeout, Duration::from_secs(10));
        assert_eq!(
            budgets.call_timeout,
            super::super::pool::DEFAULT_POOL_CONFIG.call_timeout
        );
        assert_eq!(
            budgets.drain_timeout,
            super::super::pool::DEFAULT_POOL_CONFIG.drain_timeout
        );
        assert_eq!(budgets.boot_timeout, super::super::BOOT_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_is_sixty_seconds_and_not_less() {
        let mut f = plain();
        settle().await;
        advance(59).await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the call died before its budget"
        );
        f.worker.frame(result_frame());
        assert!(f.outcome().await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn an_hour_on_an_askuser_form_does_not_burn_the_tools_budget() {
        // The whole point of the pausable countdown. Without the pause the
        // call dies under a form the user is still filling in, and the tool is
        // blamed for a timeout it spent no time on.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![serde_json::json!({ "question": "which?" })],
        });
        settle().await;
        advance(3_600).await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the call timed out while waiting on the user"
        );

        f.call.respond_ask_user("ask-1", vec![]);
        settle().await;
        assert!(
            f.worker
                .sent_kinds()
                .contains(&"ask_user_response".to_string()),
            "the answer never reached the worker"
        );
        f.worker.frame(result_frame());
        assert!(f.outcome().await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_resumes_with_what_remains_rather_than_starting_over() {
        // 50 seconds of compute, an hour of human, then 11 more seconds of
        // compute. A resume that restarted the window would survive this, and
        // a tool could extend its budget indefinitely by prompting.
        let mut f = plain();
        settle().await;
        advance(50).await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![serde_json::json!({ "question": "which?" })],
        });
        settle().await;
        advance(3_600).await;
        f.call.respond_ask_user("ask-1", vec![]);
        settle().await;
        advance(11).await;
        // Asserted on the callbacks HERE, before awaiting the outcome. Awaiting
        // it first would auto-advance the paused clock to whatever the next
        // timer is, so the call times out eventually under any budget at all
        // and the assertion cannot see a resume that restarted the window.
        assert_eq!(
            f.counts.get(),
            (1, 0, 1, 0),
            "the resumed budget outlived the 10s that remained"
        );
        assert_eq!(
            f.outcome().await.unwrap_err().message,
            "tool call timed out"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_stays_paused_until_the_last_of_two_prompts_settles() {
        // A flag per prompt kind lets the first answer re-arm the timer while
        // the second is still on screen, and the call dies under it. The
        // refcount is what stops that.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![serde_json::json!({ "question": "a?" })],
        });
        f.worker.feed(WorkerEvent::PermissionPrompt {
            request_id: "perm-1".to_string(),
            permission: PermissionKind::Read,
            resource: "/tmp/x".to_string(),
            api_name: None,
            declared: true,
            reason: None,
        });
        settle().await;
        f.call.respond_ask_user("ask-1", vec![]);
        settle().await;
        advance(3_600).await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the budget resumed while a second prompt was still open"
        );
        f.call.respond_permission("perm-1", true);
        settle().await;
        advance(61).await;
        assert_eq!(
            f.outcome().await.unwrap_err().message,
            "tool call timed out"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_response_naming_an_unknown_request_is_dropped_whole() {
        // Stale, replayed or forged. Acting on it resumes the budget while the
        // real prompt is still open, so the call dies under a live form; and
        // forwarding its answers would answer a question nobody asked.
        let f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![serde_json::json!({ "question": "a?" })],
        });
        settle().await;
        f.call.respond_ask_user("ask-99", vec![]);
        settle().await;
        assert!(
            !f.worker
                .sent_kinds()
                .contains(&"ask_user_response".to_string()),
            "a response for an unknown request reached the worker"
        );
        advance(3_600).await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the budget resumed on an unknown response"
        );
        f.call.cancel();
    }

    // --- the two payloads that must be answered rather than dropped ---------

    #[tokio::test(start_paused = true)]
    async fn an_unrenderable_question_is_answered_empty_rather_than_shown() {
        // `diff` with no `after` matches no AskUserQuestion variant, so the
        // client could not render it. Pausing the call on a form that never
        // appears strands the tool for its whole budget; answering empty lets
        // its await unwind. D-054.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![serde_json::json!({
                "kind": "diff", "question": "ok?", "before": "a",
            })],
        });
        settle().await;

        let response = f
            .worker
            .sent()
            .into_iter()
            .find_map(|frame| match frame {
                PoolToWorker::AskUserResponse {
                    answers,
                    request_id,
                    ..
                } => Some((request_id, answers)),
                _ => None,
            })
            .expect("the tool was never answered");
        assert_eq!(response.0, "ask-1");
        assert!(response.1.is_empty());

        let events = f.drain();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, CallEvent::AskUserRequest { .. })),
            "an unrenderable form was sent to the client: {events:?}"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            CallEvent::Log {
                level: LogLevel::Warn,
                ..
            }
        )));

        // And the budget was never paused, so a tool cannot buy itself
        // unlimited time by sending questions nobody can render.
        advance(61).await;
        assert_eq!(
            f.outcome().await.unwrap_err().message,
            "tool call timed out"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_question_set_is_answered_rather_than_forwarded() {
        // A form with no questions renders as nothing to answer, so nobody
        // would ever respond and the call would sit until its budget ran out.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![],
        });
        settle().await;
        assert!(f
            .worker
            .sent_kinds()
            .contains(&"ask_user_response".to_string()));
        assert!(!f
            .drain()
            .iter()
            .any(|e| matches!(e, CallEvent::AskUserRequest { .. })));
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_valid_question_set_is_forwarded_to_the_user_untouched() {
        // The positive control. Without it, a call that answered EVERY askUser
        // empty would pass both tests above and no user would ever see a form.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::AskUserRequest {
            call_id: "call-1".to_string(),
            request_id: "ask-1".to_string(),
            questions: vec![serde_json::json!({
                "kind": "choice", "question": "which?",
                "options": [{ "label": "a", "value": "a" }],
            })],
        });
        settle().await;
        assert!(
            !f.worker
                .sent_kinds()
                .contains(&"ask_user_response".to_string()),
            "a valid form was auto-answered instead of shown"
        );
        let events = f.drain();
        assert!(
            events.iter().any(
                |e| matches!(e, CallEvent::AskUserRequest { questions, .. } if questions.len() == 1)
            ),
            "the form never reached the user: {events:?}"
        );
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn an_invalid_schedule_draft_is_rejected_rather_than_stranding_the_tool() {
        let f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::ScheduleRequest {
            call_id: "call-1".to_string(),
            request_id: "sched-1".to_string(),
            draft: serde_json::json!({ "title": "" }),
        });
        settle().await;
        let rejected = f.worker.sent().into_iter().any(|frame| {
            matches!(frame, PoolToWorker::ScheduleConfirmResponse { accepted, .. } if !accepted)
        });
        assert!(rejected, "the tool's schedule await was never settled");
        assert!(!f.call.has_pending_schedule("sched-1"));
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_schedule_confirmation_is_answerable_once_and_carries_the_users_edits() {
        // `has_pending_schedule` is what chat checks before persisting a draft:
        // a replayed response is otherwise indistinguishable from the real one,
        // and the prompt would be scheduled twice.
        let f = plain();
        settle().await;
        let draft = serde_json::json!({
            "instruction": "water the plants",
            "runMissed": true,
            "schedule": { "kind": "interval", "everyMinutes": 60 },
            "title": "plants",
        });
        f.worker.frame(WorkerToPool::ScheduleRequest {
            call_id: "call-1".to_string(),
            request_id: "sched-1".to_string(),
            draft: draft.clone(),
        });
        settle().await;
        assert!(f.call.has_pending_schedule("sched-1"));
        assert!(!f.call.has_pending_schedule("sched-2"));

        let edited: ScheduledPromptDraft = serde_json::from_value(draft).unwrap();
        f.call.respond_schedule("sched-1", true, Some(edited));
        settle().await;
        assert!(
            !f.call.has_pending_schedule("sched-1"),
            "the confirmation stayed answerable"
        );

        let accepted = f.worker.sent().into_iter().any(|frame| {
            matches!(&frame, PoolToWorker::ScheduleConfirmResponse { accepted, draft, .. }
                if *accepted && draft.is_some())
        });
        assert!(accepted, "the user's edited draft never reached the tool");

        // A replay is dropped whole, so nothing is scheduled a second time.
        let before = f.worker.sent().len();
        f.call.respond_schedule("sched-1", true, None);
        settle().await;
        assert_eq!(
            f.worker.sent().len(),
            before,
            "a replayed confirmation was acted on"
        );
        f.call.cancel();
    }

    // --- display ------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn an_oversize_display_is_dropped_with_a_reason_rather_than_persisted() {
        // Accepted display content is rewritten into the session file on every
        // later message and rebroadcast to every client, so the bound is not
        // cosmetic.
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::Display {
            call_id: "call-1".to_string(),
            content: DisplayContent::Image {
                alt: None,
                data_b64: "A".repeat(8_000_001),
                mime: "image/png".to_string(),
            },
        });
        settle().await;
        let events = f.drain();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, CallEvent::Display { .. })),
            "an oversize payload was persisted"
        );
        assert!(
            events.iter().any(|e| matches!(e, CallEvent::Log { message, .. } if message.contains("display dropped"))),
            "the tool was not told why: {events:?}"
        );
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn an_ordinary_display_reaches_chat() {
        let mut f = plain();
        settle().await;
        f.worker.frame(WorkerToPool::Display {
            call_id: "call-1".to_string(),
            content: DisplayContent::Markdown {
                markdown: "# hi".to_string(),
            },
        });
        settle().await;
        assert!(f
            .drain()
            .iter()
            .any(|e| matches!(e, CallEvent::Display { .. })));
        f.call.cancel();
    }

    // --- the module broker --------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_module_result_is_returned_to_the_tool_that_awaited_it() {
        let worker = Arc::new(RecordingWorker::default());
        let f = start(
            Arc::clone(&worker),
            Some(FakeModules::returning(serde_json::json!({ "n": 7 })) as Arc<dyn ModuleDispatch>),
        );
        settle().await;
        worker.frame(WorkerToPool::ModuleRequest {
            call_id: "call-1".to_string(),
            request_id: "mreq-1".to_string(),
            module: ModuleName::Memories,
            op: "list".to_string(),
            args: Value::Null,
        });
        settle().await;
        let response = worker
            .sent()
            .into_iter()
            .find_map(|frame| match frame {
                PoolToWorker::ModuleResponse {
                    ok,
                    result,
                    request_id,
                    ..
                } => Some((request_id, ok, result)),
                _ => None,
            })
            .expect("the tool's module await was never answered");
        assert_eq!(response.0, "mreq-1");
        assert!(response.1);
        assert_eq!(response.2, Some(serde_json::json!({ "n": 7 })));
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_module_error_reaches_the_tool_scrubbed() {
        // This is where a CORE-side error message becomes text a third-party
        // tool can read and send anywhere. A provider error naming a bearer
        // token is the ordinary case, and the failure is invisible: the tool
        // gets a slightly more detailed error and nothing looks wrong.
        let worker = Arc::new(RecordingWorker::default());
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEF";
        let f = start(
            Arc::clone(&worker),
            Some(
                FakeModules::failing(&format!("upstream refused: Bearer {secret}"))
                    as Arc<dyn ModuleDispatch>,
            ),
        );
        settle().await;
        worker.frame(WorkerToPool::ModuleRequest {
            call_id: "call-1".to_string(),
            request_id: "mreq-1".to_string(),
            module: ModuleName::Llm,
            op: "complete".to_string(),
            args: Value::Null,
        });
        settle().await;
        let error = worker
            .sent()
            .into_iter()
            .find_map(|frame| match frame {
                PoolToWorker::ModuleResponse {
                    ok: false, error, ..
                } => error,
                _ => None,
            })
            .expect("the module error never came back");
        assert!(
            !error.contains(secret),
            "a credential reached extension code: {error}"
        );
        assert!(error.contains("REDACTED"), "{error}");
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_module_permission_prompt_is_answered_by_the_user_not_the_terminal() {
        // A broker prompt reuses `tool.permission_request` rather than defining
        // its own frame, so the ONLY thing separating it from a Deno terminal
        // prompt is the `mod-` request-id prefix. Route it wrong and the
        // answer is typed at a terminal nobody is prompting on, while the
        // broker waits forever.
        let worker = Arc::new(RecordingWorker::default());
        let modules = FakeModules::prompting();
        let mut f = start(
            Arc::clone(&worker),
            Some(Arc::clone(&modules) as Arc<dyn ModuleDispatch>),
        );
        settle().await;
        worker.frame(WorkerToPool::ModuleRequest {
            call_id: "call-1".to_string(),
            request_id: "mreq-1".to_string(),
            module: ModuleName::Memories,
            op: "write".to_string(),
            args: Value::Null,
        });
        settle().await;

        let request_id = f
            .drain()
            .into_iter()
            .find_map(|event| match event {
                CallEvent::PermissionRequest { request_id, .. } => Some(request_id),
                _ => None,
            })
            .expect("the broker's prompt never reached the user");
        assert!(
            request_id.starts_with("mod-"),
            "unroutable id: {request_id}"
        );

        // The budget is paused for it exactly as it is for a terminal prompt.
        advance(3_600).await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the budget ran while the broker waited"
        );

        f.call.respond_permission(&request_id, true);
        settle().await;
        assert_eq!(
            *lock(&modules.approved),
            Some(true),
            "the broker never saw the answer"
        );
        assert!(
            lock(&worker.answered).is_empty(),
            "a module prompt was answered on the worker's terminal"
        );
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_terminal_permission_prompt_is_answered_on_the_terminal() {
        // The other side of the same routing decision.
        let mut f = plain();
        settle().await;
        f.worker.feed(WorkerEvent::PermissionPrompt {
            request_id: "perm-1".to_string(),
            permission: PermissionKind::Net,
            resource: "example.com:443".to_string(),
            api_name: Some("fetch()".to_string()),
            declared: true,
            reason: Some("to search".to_string()),
        });
        settle().await;
        assert!(f
            .drain()
            .iter()
            .any(|e| matches!(e, CallEvent::PermissionRequest { .. })));
        f.call.respond_permission("perm-1", true);
        settle().await;
        assert_eq!(
            *lock(&f.worker.answered),
            vec![("perm-1".to_string(), true)]
        );
        f.call.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn a_broker_prompt_still_open_when_the_call_dies_is_released_as_refused() {
        // Otherwise the broker's task parks forever holding whatever it was
        // about to do, and its permission is neither granted nor denied.
        let worker = Arc::new(RecordingWorker::default());
        let modules = FakeModules::prompting();
        let mut f = start(
            Arc::clone(&worker),
            Some(Arc::clone(&modules) as Arc<dyn ModuleDispatch>),
        );
        settle().await;
        worker.frame(WorkerToPool::ModuleRequest {
            call_id: "call-1".to_string(),
            request_id: "mreq-1".to_string(),
            module: ModuleName::Memories,
            op: "write".to_string(),
            args: Value::Null,
        });
        settle().await;
        worker.feed(WorkerEvent::WorkerExited { code: Some(9) });
        assert!(f.outcome().await.is_err());
        settle().await;
        assert_eq!(
            *lock(&modules.approved),
            Some(false),
            "the broker was left waiting on a dead call"
        );
    }

    // --- cancel and exit ----------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn cancel_tells_the_ui_at_once_and_the_worker_cooperatively() {
        let mut f = plain();
        settle().await;
        f.call.cancel();
        settle().await;
        assert!(f
            .drain()
            .iter()
            .any(|e| matches!(e, CallEvent::ToolCancelled)));
        assert!(f.worker.sent_kinds().contains(&"cancel".to_string()));
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "the worker was killed before it had a chance"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_acknowledges_a_cancel_within_the_drain_is_kept() {
        // The drain has to be a deadline the driver races, not a sleep it sits
        // in: a worker that answers during the window would otherwise be killed
        // for having answered.
        let mut f = plain();
        settle().await;
        f.call.cancel();
        tokio::time::advance(Duration::from_millis(500)).await;
        f.worker.frame(WorkerToPool::ToolError {
            call_id: "call-1".to_string(),
            error: "aborted".to_string(),
        });
        let err = f.outcome().await.unwrap_err();
        assert_eq!(
            err.message, "tool call cancelled",
            "the error class changed after a cancel"
        );
        assert_eq!(
            f.counts.get(),
            (1, 1, 0, 0),
            "a cooperative worker was killed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_ignores_a_cancel_is_killed_after_the_drain_budget() {
        let mut f = plain();
        settle().await;
        f.call.cancel();
        advance(3).await;
        assert_eq!(
            f.outcome().await.unwrap_err().message,
            "tool call cancelled"
        );
        assert_eq!(
            f.counts.get(),
            (1, 0, 1, 0),
            "a wedged worker survived a cancel"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_drain_budget_is_two_seconds() {
        let mut f = plain();
        settle().await;
        f.call.cancel();
        // Let the driver process the cancel BEFORE moving the clock. `advance`
        // jumps in one hop, so a deadline the driver has not armed yet is
        // anchored at the far side of the jump and the whole budget is
        // measured from the wrong instant: both halves of this test then pass
        // for a reason that has nothing to do with two seconds.
        settle().await;
        tokio::time::advance(Duration::from_millis(1_900)).await;
        settle().await;
        assert_eq!(
            f.counts.get(),
            (1, 0, 0, 0),
            "killed before the drain budget elapsed"
        );
        f.worker.frame(WorkerToPool::ToolError {
            call_id: "call-1".to_string(),
            error: "aborted".to_string(),
        });
        assert!(f.outcome().await.is_err());
        assert_eq!(f.counts.get(), (1, 1, 0, 0));

        // The upper edge as well. With only the lower one, doubling the budget
        // passes: nothing asks whether the kill arrived on time, and a
        // cancelled worker goes on burning a core for twice as long.
        let g = plain();
        settle().await;
        g.call.cancel();
        settle().await;
        tokio::time::advance(Duration::from_millis(2_100)).await;
        settle().await;
        assert_eq!(
            g.counts.get(),
            (1, 0, 1, 0),
            "the drain budget outlived two seconds"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_exit_settles_the_call_instead_of_waiting_out_the_budget() {
        // A crash, an OOM, the answer give-up kill, or a permissions refresh.
        // Waiting for the 60s budget on a process that is already gone is a
        // minute of a spinner for nothing.
        let mut f = plain();
        settle().await;
        f.worker.feed(WorkerEvent::WorkerExited { code: Some(1) });
        let err = f.outcome().await.unwrap_err();
        assert_eq!(err.message, "tool worker exited");
        assert_eq!(f.counts.get(), (1, 0, 1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn an_exit_after_a_cancel_reports_the_cancel_rather_than_the_crash() {
        let mut f = plain();
        settle().await;
        f.call.cancel();
        settle().await;
        f.worker.feed(WorkerEvent::WorkerExited { code: None });
        let err = f.outcome().await.unwrap_err();
        assert_eq!(err.message, "tool call cancelled");
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_cancel_is_a_no_op() {
        let mut f = plain();
        settle().await;
        f.call.cancel();
        f.call.cancel();
        f.call.cancel();
        settle().await;
        let cancels = f
            .worker
            .sent_kinds()
            .into_iter()
            .filter(|k| k == "cancel")
            .count();
        assert_eq!(cancels, 1, "the worker was told to cancel {cancels} times");
        let bubbles = f
            .drain()
            .into_iter()
            .filter(|e| matches!(e, CallEvent::ToolCancelled))
            .count();
        assert_eq!(bubbles, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_outcome_can_only_be_taken_once() {
        let mut f = plain();
        settle().await;
        f.worker.frame(result_frame());
        assert!(f.outcome().await.is_ok());
        assert!(f.outcome().await.is_err(), "a second await hung or lied");
    }
}

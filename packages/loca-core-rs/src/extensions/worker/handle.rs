//! One running worker: its protocol channel, its terminal, and the permission
//! prompts that come off it.
//!
//! Ports the runtime half of `packages/loca-core/src/extensions/worker-handle.ts`.
//! The spawn half is [`super::spawn_plan`].
//!
//! # What this owns that nothing else can
//!
//! A Deno permission prompt arrives as TEXT on a terminal, at an arbitrary
//! moment, with no call attached to it. Turning that into either an auto-denial
//! or a question the user sees is this module's whole job, and both of its
//! failure modes are bad in different ways: a prompt never answered hangs the
//! call until the give-up kill, and a prompt answered `y` on the user's behalf
//! is a permission granted by nobody.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Duration, Instant};

use crate::error::{AppError, ErrorCode, Result};
use crate::extensions::prompt::matcher::{decide_prompt, PromptAction, PromptPolicy};
use crate::extensions::prompt::parser::{PromptParser, PromptParserEvent};
use crate::extensions::sandbox::EnvLookup;

use super::protocol::{parse_worker_frame, PoolToWorker, WorkerEvent, WorkerToPool};

/// Deno's prompter calls `clear_stdin`, which flushes stdin until it has been
/// quiescent for about 100ms BEFORE it reads the answer. An answer written any
/// sooner is flushed away rather than read, and the prompt sits there.
const ANSWER_INITIAL_DELAY: Duration = Duration::from_millis(300);

/// Keep re-sending until the Granted/Denied confirmation is parsed. The first
/// write can still land inside a flush window that the initial delay only
/// usually clears.
const ANSWER_RETRY: Duration = Duration::from_millis(600);

/// If the confirmation never arrives (a prompt-format change, a wedged
/// terminal), KILL THE WORKER. Failing closed here is what stops a Deno wording
/// change from turning into a tool call that hangs for the full call timeout
/// with nothing in the log to say why.
const ANSWER_GIVEUP: Duration = Duration::from_secs(10);

/// An oversized partial frame with no newline is dropped rather than buffered.
/// Protocol frames are normally tiny; this is headroom for a large tool result.
///
/// Bounds a frame on whichever transport carries it: the worker's stdout on
/// unix, the loopback control socket under a ConPTY. One constant, because the
/// bound is a property of the protocol rather than of the pipe.
pub(crate) const MAX_PROTOCOL_FRAME_BYTES: usize = 16_000_000;

/// A runaway stderr line is truncated with a marker. Log lines, so a far smaller
/// bound than the protocol's. Different failure, different fix, so the two are
/// separate constants rather than one shared one.
const MAX_STDERR_LINE_BYTES: usize = 1_000_000;

const TRUNCATION_MARKER: &str = " …[truncated]";

/// The terminal a worker's prompts arrive on.
///
/// A trait rather than the concrete [`crate::extensions::pty::PtySession`] for
/// one reason: the prompt handshake is the most security-critical state machine
/// in this package and it is driven entirely by timing. Behind this seam it can
/// be tested against a recording terminal on a paused clock, which is both exact
/// and instant; against a real `deno` it would be neither, and the live probe
/// covers that end separately.
pub trait Terminal: Send + Sync + 'static {
    /// Worker-protocol bytes. Echo-tracked on unix.
    fn write(&self, bytes: &[u8]);
    /// A permission-prompt answer. NOT echo-tracked.
    fn answer(&self, bytes: &[u8]);
    fn kill(&self);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Terminal for crate::extensions::pty::PtySession {
    fn write(&self, bytes: &[u8]) {
        Self::write(self, bytes);
    }
    fn answer(&self, bytes: &[u8]) {
        Self::answer(self, bytes);
    }
    fn kill(&self) {
        Self::kill(self);
    }
}

/// What the driver is fed. Either the worker's protocol stream or its terminal.
#[derive(Debug)]
pub enum Incoming {
    /// A line off the protocol channel (stdout on unix, the control socket on
    /// Windows).
    ProtocolLine(String),
    /// Terminal text: the worker's stderr and Deno's prompt.
    Terminal(Vec<u8>),
    /// The process is gone.
    Exited { code: Option<i32> },
}

enum Command {
    Send(Box<PoolToWorker>),
    Answer { request_id: String, allow: bool },
    Shutdown(oneshot::Sender<()>),
}

/// The subscriber list, shared between the handle and its driver.
///
/// Shared state rather than a `Subscribe` command on purpose: a command would
/// be processed by the driver's `select!` in a race with the first event, and a
/// caller that subscribed and then fed the worker could miss the answer to its
/// own frame. That is a bug that reproduces one run in ten and looks like a
/// worker hang.
type Listeners = Arc<Mutex<Vec<mpsc::UnboundedSender<WorkerEvent>>>>;

/// How far a worker has got through starting up.
///
/// A LATCH rather than an event, because a warm worker booted minutes ago and
/// its `booted` frame is long gone. A call arriving on it must not wait ten
/// seconds for a frame that will never come again, which is what a plain
/// "await the next boot event" gets wrong and what makes every reused worker
/// look like a boot timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootState {
    Waiting,
    Booted,
    /// The worker said `boot_failed`, or died before it said anything. Carries
    /// the message the call surfaces, so the user sees why rather than a
    /// generic timeout.
    Failed(String),
}

/// Distinguishes one worker process from the next.
///
/// The pool needs it for an identity check: a settle belonging to a worker that
/// has already been replaced must not tear down its replacement. Comparing
/// handles by value cannot express that, and comparing by extension id would
/// make every replacement look like the worker it replaced.
static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);

/// A handle to one worker. Cheap to clone; every clone drives the same process.
#[derive(Clone)]
pub struct WorkerHandle {
    // `Debug` is hand-written below: a derived one would print the prompt
    // policy, which carries every declared path an extension asked for.
    id: u64,
    pub extension_id: String,
    commands: mpsc::UnboundedSender<Command>,
    listeners: Listeners,
    in_flight: Arc<AtomicUsize>,
    prompt_answered_by_user: Arc<AtomicBool>,
    last_activity: Arc<Mutex<Instant>>,
    boot: watch::Receiver<BootState>,
    spawned_at: Instant,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("id", &self.id)
            .field("extension_id", &self.extension_id)
            .field("in_flight", &self.in_flight_calls())
            .finish()
    }
}

impl WorkerHandle {
    /// Start the driver over an already-spawned worker.
    ///
    /// `terminal` is `None` in pipe mode, where there is nothing to prompt on
    /// and nothing to answer with.
    pub fn start(
        extension_id: String,
        incoming: mpsc::UnboundedReceiver<Incoming>,
        terminal: Option<Arc<dyn Terminal>>,
        policy: Option<Arc<PromptPolicy>>,
        env: Arc<dyn EnvLookup + Send + Sync>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let prompt_answered_by_user = Arc::new(AtomicBool::new(false));
        let last_activity = Arc::new(Mutex::new(Instant::now()));
        let listeners: Listeners = Arc::new(Mutex::new(Vec::new()));
        let (boot_tx, boot_rx) = watch::channel(BootState::Waiting);
        let driver = Driver {
            extension_id: extension_id.clone(),
            boot: boot_tx,
            terminal,
            policy,
            env,
            listeners: Arc::clone(&listeners),
            parser: PromptParser::new(),
            send_queue: Vec::new(),
            prompt: None,
            answering: None,
            in_flight: Arc::clone(&in_flight),
            prompt_answered_by_user: Arc::clone(&prompt_answered_by_user),
            last_activity: Arc::clone(&last_activity),
        };
        tokio::spawn(driver.run(incoming, rx));
        Self {
            id: NEXT_WORKER_ID.fetch_add(1, Ordering::SeqCst),
            extension_id,
            commands: tx,
            listeners,
            in_flight,
            prompt_answered_by_user,
            last_activity,
            boot: boot_rx,
            spawned_at: Instant::now(),
        }
    }

    /// Resolve once this worker has booted.
    ///
    /// Returns immediately for a warm worker, because [`BootState`] is a latch
    /// rather than an event. `Err` for a worker that reported `boot_failed`,
    /// died before booting, or has said nothing at all within `budget`.
    pub async fn wait_for_boot(&self, budget: Duration) -> Result<()> {
        let mut boot = self.boot.clone();
        let settled = async {
            loop {
                match &*boot.borrow_and_update() {
                    BootState::Booted => return Ok(()),
                    BootState::Failed(error) => {
                        return Err(AppError::new(
                            ErrorCode::InternalError,
                            format!("worker boot failed: {error}"),
                        ))
                    }
                    BootState::Waiting => {}
                }
                if boot.changed().await.is_err() {
                    // The driver stopped without recording a verdict. Distinct
                    // wording from the exit path below on purpose: this one
                    // means an internal channel closed, which is a Core bug,
                    // where "exited before booting" means the extension's
                    // process died, which is an extension problem. One message
                    // for both sends every reader to the wrong layer, and it
                    // also makes the exit path's own latch untestable, because
                    // deleting it would look identical from out here.
                    return Err(AppError::new(
                        ErrorCode::InternalError,
                        "worker boot failed: the worker driver stopped without a verdict",
                    ));
                }
            }
        };
        match tokio::time::timeout(budget, settled).await {
            Ok(result) => result,
            Err(_) => Err(AppError::new(
                ErrorCode::InternalError,
                "worker boot timeout",
            )),
        }
    }

    /// Every frame and synthesized event, from now on. One receiver per caller.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<WorkerEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.listeners).push(tx);
        rx
    }

    pub fn send(&self, frame: PoolToWorker) {
        let _ = self.commands.send(Command::Send(Box::new(frame)));
    }

    /// Answer a forwarded permission prompt. A no-op unless `request_id` is the
    /// prompt currently pending, so a stale response arriving after a settle
    /// cannot answer the NEXT prompt.
    pub fn answer_prompt(&self, request_id: &str, allow: bool) {
        let _ = self.commands.send(Command::Answer {
            request_id: request_id.to_string(),
            allow,
        });
    }

    /// This worker process's identity, stable for its lifetime and never reused.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn in_flight_calls(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    pub fn call_started(&self) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
    }

    pub fn call_settled(&self) {
        // Saturating: a settle path that ran twice must not wrap to
        // `usize::MAX`, which would make the worker look permanently busy and
        // exempt from eviction forever.
        let _ = self
            .in_flight
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            });
    }

    /// True once a user-forwarded prompt settled during this worker's lifetime.
    ///
    /// Deno caches a permission verdict for the PROCESS lifetime while the
    /// user's answer was scoped to one call, so the pool must retire a worker
    /// this is true of rather than returning it to the warm set.
    pub fn prompt_answered_by_user(&self) -> bool {
        self.prompt_answered_by_user.load(Ordering::SeqCst)
    }

    /// Stand in for a prompt the user answered, so the pool's retirement rule
    /// can be tested without a terminal and a policy behind it.
    #[cfg(test)]
    pub(crate) fn mark_prompt_answered_for_test(&self) {
        self.prompt_answered_by_user.store(true, Ordering::SeqCst);
    }

    pub fn idle_since(&self) -> Instant {
        *lock(&self.last_activity)
    }

    pub fn spawned_at(&self) -> Instant {
        self.spawned_at
    }

    /// Send `shutdown`, then wait for the driver to finish, bounded by `drain`.
    pub async fn terminate(&self, drain: Duration) {
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Command::Shutdown(tx)).is_err() {
            return;
        }
        let _ = tokio::time::timeout(drain, rx).await;
    }
}

/// A prompt that has been seen and not yet settled.
struct Pending {
    request_id: String,
    /// True only when it was actually FORWARDED to the user. An auto-denied
    /// prompt must not retire the worker: nothing was granted by anyone.
    forwarded: bool,
    seen_at: Instant,
}

/// An answer being written until the confirmation shows up.
struct Answering {
    payload: Vec<u8>,
    started: Instant,
    next_write: Instant,
}

struct Driver {
    extension_id: String,
    boot: watch::Sender<BootState>,
    terminal: Option<Arc<dyn Terminal>>,
    policy: Option<Arc<PromptPolicy>>,
    env: Arc<dyn EnvLookup + Send + Sync>,
    listeners: Listeners,
    parser: PromptParser,
    /// Frames held while a prompt is pending: Deno's `clear_stdin` would flush
    /// anything written to the terminal before it reads the answer, so a frame
    /// sent during that window is destroyed rather than delivered.
    send_queue: Vec<String>,
    prompt: Option<Pending>,
    answering: Option<Answering>,
    in_flight: Arc<AtomicUsize>,
    prompt_answered_by_user: Arc<AtomicBool>,
    last_activity: Arc<Mutex<Instant>>,
}

impl Driver {
    async fn run(
        mut self,
        mut incoming: mpsc::UnboundedReceiver<Incoming>,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        let mut shutdown_ack: Option<oneshot::Sender<()>> = None;
        loop {
            // Only armed while an answer is outstanding. `sleep_until` on a
            // tokio Instant, so a paused clock in a test moves it (D-038).
            let tick = self.answering.as_ref().map(|a| a.next_write);
            tokio::select! {
                event = incoming.recv() => match event {
                    Some(Incoming::ProtocolLine(line)) => self.on_protocol_line(&line),
                    Some(Incoming::Terminal(bytes)) => self.on_terminal(&bytes),
                    Some(Incoming::Exited { code }) => {
                        // Fail any waiter BEFORE the exit event. A call still
                        // waiting on boot has no listener attached yet, so the
                        // exit event alone would leave it waiting out the full
                        // boot budget for a process that is already gone.
                        self.fail_boot("the worker exited before it booted");
                        self.emit(WorkerEvent::WorkerExited { code });
                        break;
                    }
                    None => break,
                },
                command = commands.recv() => match command {
                    Some(Command::Send(frame)) => self.on_send(*frame),
                    Some(Command::Answer { request_id, allow }) => {
                        self.on_answer_command(&request_id, allow);
                    }
                    Some(Command::Shutdown(ack)) => {
                        self.on_shutdown();
                        shutdown_ack = Some(ack);
                        break;
                    }
                    None => break,
                },
                () = sleep_until(tick) => self.on_answer_tick(),
            }
        }
        if let Some(ack) = shutdown_ack {
            let _ = ack.send(());
        }
    }

    // --- outbound -----------------------------------------------------------

    fn on_send(&mut self, frame: PoolToWorker) {
        self.touch();
        let Ok(mut line) = serde_json::to_string(&frame) else {
            tracing::warn!(
                target: "loca::extensions",
                "[{}] dropping an unserializable frame", self.extension_id,
            );
            return;
        };
        line.push('\n');
        if self.prompt.is_some() {
            self.send_queue.push(line);
            return;
        }
        self.write_line(&line);
    }

    fn write_line(&self, line: &str) {
        if let Some(terminal) = &self.terminal {
            terminal.write(line.as_bytes());
        }
    }

    fn on_shutdown(&mut self) {
        // Stop any answer-retry loop first, so it cannot keep firing (and
        // re-issuing a kill) for up to the give-up budget after the worker is
        // already gone.
        self.prompt = None;
        self.answering = None;
        self.on_send(PoolToWorker::Shutdown);
    }

    // --- the protocol stream ------------------------------------------------

    fn on_protocol_line(&mut self, line: &str) {
        self.touch();
        if line.trim().is_empty() {
            return;
        }
        match parse_worker_frame(line) {
            // The three lifecycle frames are consumed here rather than
            // emitted: they belong to the worker, not to any call, and a call
            // that saw them would have to filter them back out.
            Some(WorkerToPool::Ready) => {}
            Some(WorkerToPool::Booted { .. }) => {
                let _ = self.boot.send(BootState::Booted);
            }
            Some(WorkerToPool::BootFailed { error, .. }) => self.fail_boot(&error),
            Some(frame) => self.emit(WorkerEvent::Frame(frame)),
            None => tracing::warn!(
                target: "loca::extensions",
                "[{}] dropping an invalid frame: {}",
                self.extension_id,
                truncate(line, 200),
            ),
        }
    }

    // --- the terminal -------------------------------------------------------

    fn on_terminal(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes).into_owned();
        for event in self.parser.feed(&text) {
            self.on_prompt_event(event);
        }
    }

    fn on_prompt_event(&mut self, event: PromptParserEvent) {
        match event {
            PromptParserEvent::StderrLine { line } => {
                let line = if line.len() > MAX_STDERR_LINE_BYTES {
                    format!(
                        "{}{TRUNCATION_MARKER}",
                        truncate(&line, MAX_STDERR_LINE_BYTES)
                    )
                } else {
                    line
                };
                self.emit(WorkerEvent::StderrLog { line });
            }
            PromptParserEvent::Prompt {
                permission,
                resource,
                api_name,
            } => self.on_prompt(&permission, &resource, api_name),
            PromptParserEvent::Settled { .. } => self.on_settled(),
        }
    }

    fn on_prompt(&mut self, permission: &str, resource: &str, api_name: Option<String>) {
        let request_id = format!("perm-{}", uuid::Uuid::new_v4());
        self.prompt = Some(Pending {
            request_id: request_id.clone(),
            forwarded: false,
            seen_at: Instant::now(),
        });

        let decision = self.policy.as_ref().and_then(|policy| {
            decide_prompt(permission, resource, &policy.as_context(self.env.as_ref()))
        });
        let shown = if resource.is_empty() {
            "(all)"
        } else {
            resource
        };

        // Fail closed, path one: a prompt kind that cannot be declared, granted
        // or rendered (Deno also prompts for `import`), or a policy that says
        // deny.
        let Some(decision) = decision.filter(|d| d.action == PromptAction::Forward) else {
            self.emit(WorkerEvent::StderrLog {
                line: format!("auto-denied {permission} access to {shown} (permission policy)"),
            });
            self.start_answer(false);
            return;
        };

        // Fail closed, path two: a Deno prompt blocks the worker's WHOLE
        // isolate, and the synthesized frame carries no call id, so with more
        // than one call sharing this worker the prompt cannot be attributed to
        // either. Forwarding it would show one user a prompt caused by another
        // user's call. The affected call sees NotCapable, exactly as in pipe
        // mode.
        if self.in_flight.load(Ordering::SeqCst) > 1 {
            self.emit(WorkerEvent::StderrLog {
                line: format!(
                    "auto-denied {permission} access to {shown} \
                     (concurrent calls share this worker; cannot attribute the prompt)"
                ),
            });
            self.start_answer(false);
            return;
        }

        if let Some(pending) = &mut self.prompt {
            pending.forwarded = true;
        }
        self.emit(WorkerEvent::PermissionPrompt {
            request_id,
            permission: decision.permission_kind,
            resource: resource.to_string(),
            api_name,
            declared: decision.declared,
            reason: decision.reason,
        });
    }

    fn on_settled(&mut self) {
        if let Some(pending) = self.prompt.take() {
            if pending.forwarded {
                // Only a prompt the USER answered retires the worker. Deno
                // caches the verdict for the process lifetime while the answer
                // was scoped to one call, so reusing this process would carry
                // the grant into the next call silently.
                self.prompt_answered_by_user.store(true, Ordering::SeqCst);
            }
        }
        self.answering = None;
        // The prompt window is over, so the frames held back during it can go.
        for line in std::mem::take(&mut self.send_queue) {
            self.write_line(&line);
        }
    }

    fn on_answer_command(&mut self, request_id: &str, allow: bool) {
        // A response for a prompt that has already settled must not answer
        // whatever prompt is pending now.
        let matches = self
            .prompt
            .as_ref()
            .is_some_and(|p| p.request_id == request_id);
        if !matches {
            return;
        }
        self.start_answer(allow);
    }

    fn start_answer(&mut self, allow: bool) {
        let seen_at = self.prompt.as_ref().map(|p| p.seen_at);
        let now = Instant::now();
        // The initial delay is measured from when the PROMPT appeared, not from
        // now: a user who took ten seconds to answer has already waited out
        // Deno's flush window, and re-imposing it would add 300ms to every
        // forwarded prompt for nothing.
        let first = seen_at
            .map(|seen| seen + ANSWER_INITIAL_DELAY)
            .unwrap_or(now + ANSWER_INITIAL_DELAY)
            .max(now);
        self.answering = Some(Answering {
            payload: if allow {
                b"y\n".to_vec()
            } else {
                b"n\n".to_vec()
            },
            started: now,
            next_write: first,
        });
    }

    fn on_answer_tick(&mut self) {
        let Some(answering) = &mut self.answering else {
            return;
        };
        if self.prompt.is_none() {
            self.answering = None;
            return;
        }
        if Instant::now().duration_since(answering.started) > ANSWER_GIVEUP {
            // The confirmation never came: a prompt-format change, or a wedged
            // terminal. Fail closed by killing the worker; the in-flight call
            // then settles through the exit path rather than hanging.
            self.answering = None;
            self.emit(WorkerEvent::StderrLog {
                line: "permission prompt answer was not accepted; killing worker".to_string(),
            });
            if let Some(terminal) = &self.terminal {
                terminal.kill();
            }
            return;
        }
        let payload = answering.payload.clone();
        answering.next_write = Instant::now() + ANSWER_RETRY;
        if let Some(terminal) = &self.terminal {
            terminal.answer(&payload);
        }
    }

    // --- plumbing -----------------------------------------------------------

    /// Latch a boot failure, unless the worker had already booted: a process
    /// that ran for an hour and then died did not fail to BOOT, and reporting
    /// it that way would send every later caller looking at the wrong thing.
    fn fail_boot(&mut self, error: &str) {
        self.boot.send_if_modified(|state| {
            if *state == BootState::Waiting {
                *state = BootState::Failed(error.to_string());
                return true;
            }
            false
        });
    }

    fn emit(&mut self, event: WorkerEvent) {
        // A closed receiver is a caller that went away, which is ordinary: the
        // pool drops its subscription when a call settles.
        lock(&self.listeners).retain(|tx| tx.send(event.clone()).is_ok());
    }

    fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `sleep_until`, or never when there is no deadline. A `select!` branch has to
/// be a future either way, and `pending()` is the one that is never ready.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Read a worker's protocol stream into [`Incoming::ProtocolLine`] events.
///
/// Its own reader rather than D14's generic drain: that one caps a line at 1 MB,
/// which is right for stderr and wrong for a protocol frame carrying a large
/// tool result.
pub async fn pump_protocol<R>(reader: R, extension_id: String, tx: mpsc::UnboundedSender<Incoming>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.len() > MAX_PROTOCOL_FRAME_BYTES {
            // An oversized frame with no newline: drop it to bound memory. The
            // in-flight call fails through the ordinary timeout path.
            tracing::warn!(
                target: "loca::extensions",
                "[{extension_id}] dropping an oversized stdout frame ({} bytes)",
                buf.len(),
            );
            continue;
        }
        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
        if line.is_empty() {
            continue;
        }
        if tx.send(Incoming::ProtocolLine(line)).is_err() {
            break;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::contract::api::{Grant, GrantState, PermissionDecl, UndeclaredPolicy};
    use crate::extensions::sandbox::{permission_key, permission_kind_of, PathTemplates};
    use crate::extensions::worker::protocol::WorkerToPool;

    use super::*;

    /// A terminal that records instead of existing.
    ///
    /// Every assertion below about what was NOT written depends on this
    /// recording what WAS, so each test that asserts an absence also asserts a
    /// presence somewhere in the same terminal (D-050).
    #[derive(Default)]
    struct Recorder {
        writes: Mutex<Vec<Vec<u8>>>,
        answers: Mutex<Vec<Vec<u8>>>,
        kills: AtomicUsize,
    }

    impl Recorder {
        fn answers(&self) -> Vec<String> {
            self.answers
                .lock()
                .unwrap()
                .iter()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect()
        }
        fn writes(&self) -> Vec<String> {
            self.writes
                .lock()
                .unwrap()
                .iter()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect()
        }
        fn kills(&self) -> usize {
            self.kills.load(Ordering::SeqCst)
        }
    }

    impl Terminal for Arc<Recorder> {
        fn write(&self, bytes: &[u8]) {
            self.writes.lock().unwrap().push(bytes.to_vec());
        }
        fn answer(&self, bytes: &[u8]) {
            self.answers.lock().unwrap().push(bytes.to_vec());
        }
        fn kill(&self) {
            self.kills.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn read_decl(path: &str) -> PermissionDecl {
        PermissionDecl::Read {
            optional: None,
            path: path.to_string(),
            reason: "to read".to_string(),
        }
    }

    fn policy(decls: Vec<PermissionDecl>, state: GrantState) -> Arc<PromptPolicy> {
        let grants = decls
            .iter()
            .map(|decl| Grant {
                granted_at_ms: 0,
                permission_key: permission_key(decl),
                permission_kind: permission_kind_of(decl),
                state,
                tool_id: "t1".to_string(),
            })
            .collect();
        Arc::new(PromptPolicy {
            required: decls,
            grants,
            undeclared_policy: UndeclaredPolicy::Deny,
            templates: PathTemplates {
                home: PathBuf::from("/home/u"),
                downloads: PathBuf::from("/home/u/Downloads"),
                models: PathBuf::from("/models"),
                sessions: PathBuf::from("/sessions"),
                extension: PathBuf::from("/ext/demo"),
            },
        })
    }

    struct Harness {
        handle: WorkerHandle,
        incoming: mpsc::UnboundedSender<Incoming>,
        events: mpsc::UnboundedReceiver<WorkerEvent>,
        terminal: Arc<Recorder>,
    }

    fn harness(policy: Option<Arc<PromptPolicy>>) -> Harness {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let terminal = Arc::new(Recorder::default());
        let handle = WorkerHandle::start(
            "demo".to_string(),
            incoming_rx,
            Some(Arc::new(Arc::clone(&terminal))),
            policy,
            Arc::new(BTreeMap::<String, String>::new()),
        );
        let events = handle.subscribe();
        Harness {
            handle,
            incoming: incoming_tx,
            events,
            terminal,
        }
    }

    /// A complete prompt block as Deno prints it, with the unterminated `Allow?`
    /// line the parser completes on.
    fn prompt_block(kind: &str, resource: &str) -> Vec<u8> {
        format!(
            "┏ ⚠️  Deno requests {kind} access to \"{resource}\".\n\
             ┠─ Requested by `Deno.readFileSync()` API.\n\
             ┗ Allow? [y/n/A] (y = yes, allow) > "
        )
        .into_bytes()
    }

    fn granted_line() -> Vec<u8> {
        b"\n\xe2\x9c\x85 Granted read access.\n".to_vec()
    }

    /// Drain whatever events are ready, letting the driver run first.
    ///
    /// A `sleep` rather than a `yield_now`: under a paused clock the runtime
    /// auto-advances only when every task is idle, so waiting on one is a
    /// guarantee that the driver has processed everything queued. A bare yield
    /// polls the driver once, which is not enough for a message that has to
    /// traverse two channels, and the tests then fail for a reason that has
    /// nothing to do with the behaviour under test.
    async fn drain(events: &mut mpsc::UnboundedReceiver<WorkerEvent>) -> Vec<WorkerEvent> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let mut out = Vec::new();
        while let Ok(event) = events.try_recv() {
            out.push(event);
        }
        out
    }

    // --- boot -------------------------------------------------------------

    fn line(frame: serde_json::Value) -> Incoming {
        Incoming::ProtocolLine(frame.to_string())
    }

    #[tokio::test(start_paused = true)]
    async fn a_warm_worker_does_not_wait_for_a_boot_frame_that_already_came() {
        // The reason boot is a LATCH. A warm worker booted minutes ago and its
        // `booted` frame is long gone, so a second call awaiting the next boot
        // event waits out the full ten-second budget and then reports a boot
        // timeout for a worker that is running perfectly.
        let h = harness(None);
        h.incoming
            .send(line(
                serde_json::json!({ "kind": "booted", "extensionId": "demo" }),
            ))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;

        for _ in 0..3 {
            let started = Instant::now();
            h.handle
                .wait_for_boot(Duration::from_secs(10))
                .await
                .unwrap();
            assert_eq!(
                started.elapsed(),
                Duration::ZERO,
                "a warm worker was made to wait for boot again"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_dies_before_booting_fails_its_waiter_at_once() {
        // Without this the call sits out the whole boot budget waiting on a
        // process that is already gone, which is ten seconds of spinner for a
        // failure that is already known.
        let h = harness(None);
        h.incoming.send(Incoming::Exited { code: Some(1) }).unwrap();
        let started = Instant::now();
        let err = h
            .handle
            .wait_for_boot(Duration::from_secs(10))
            .await
            .unwrap_err();
        // The EXACT message, not a substring both paths share. The exit
        // path's own latch is otherwise untestable: dropping the driver's
        // sender fails the waiter too, so a loose assertion passes with the
        // latch deleted and the diagnosis quietly moves to the wrong layer.
        assert_eq!(
            err.message, "worker boot failed: the worker exited before it booted",
            "a dead extension process was reported as a Core bug"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the waiter sat out the boot budget for a dead process"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_boot_failure_carries_the_workers_own_reason() {
        // The user needs to see WHY, and the worker is the only thing that
        // knows: a generic timeout sends the reader looking at the wrong layer.
        let h = harness(None);
        h.incoming
            .send(line(serde_json::json!({
                "kind": "boot_failed", "extensionId": "demo",
                "error": "Cannot find module ./entry.ts",
            })))
            .unwrap();
        let err = h
            .handle
            .wait_for_boot(Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("Cannot find module"),
            "{}",
            err.message
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_exit_long_after_a_successful_boot_is_not_a_boot_failure() {
        // A worker that ran for an hour and then crashed did not fail to BOOT.
        // Rewriting the latch on the way out makes every later reader diagnose
        // a startup problem that never happened.
        let h = harness(None);
        h.incoming
            .send(line(
                serde_json::json!({ "kind": "booted", "extensionId": "demo" }),
            ))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        h.incoming.send(Incoming::Exited { code: Some(9) }).unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(
            h.handle
                .wait_for_boot(Duration::from_secs(10))
                .await
                .is_ok(),
            "a late crash was reported as a boot failure"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_says_nothing_at_all_gives_up_after_its_budget() {
        let h = harness(None);
        let started = Instant::now();
        let err = h
            .handle
            .wait_for_boot(Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(err.message, "worker boot timeout");
        assert_eq!(started.elapsed(), Duration::from_secs(10));
        // And the budget the pool actually ships is that one, not some other
        // number this test happened to pass in.
        assert_eq!(
            crate::extensions::worker::BOOT_TIMEOUT,
            Duration::from_secs(10)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_three_lifecycle_frames_never_reach_a_call() {
        // They belong to the worker, not to any call. A call that saw them
        // would have to filter them back out, and the one that forgot would
        // route a `booted` into a call's frame handler.
        let mut h = harness(None);
        for frame in [
            serde_json::json!({ "kind": "ready" }),
            serde_json::json!({ "kind": "booted", "extensionId": "demo" }),
            serde_json::json!({ "kind": "boot_failed", "extensionId": "demo", "error": "x" }),
        ] {
            h.incoming.send(line(frame)).unwrap();
        }
        // A frame that IS a call's business, so this test also proves the
        // channel works rather than merely observing silence.
        h.incoming
            .send(line(serde_json::json!({
                "kind": "log", "callId": "c1", "level": "info", "message": "hi",
            })))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert_eq!(
            events.len(),
            1,
            "lifecycle frames leaked to a call: {events:?}"
        );
        assert!(matches!(
            &events[0],
            WorkerEvent::Frame(WorkerToPool::Log { .. })
        ));
    }

    fn prompts(events: &[WorkerEvent]) -> Vec<&WorkerEvent> {
        events
            .iter()
            .filter(|e| matches!(e, WorkerEvent::PermissionPrompt { .. }))
            .collect()
    }

    fn logs(events: &[WorkerEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                WorkerEvent::StderrLog { line } => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    // ---- the instrument itself ---------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_declared_ask_prompt_is_forwarded_to_the_user() {
        // The positive control for every "was not forwarded" assertion below. If
        // the parser, the driver or the subscription were broken, all of those
        // would pass for the wrong reason.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        let forwarded = prompts(&events);
        assert_eq!(forwarded.len(), 1, "no prompt reached the user: {events:?}");
        let WorkerEvent::PermissionPrompt {
            resource,
            declared,
            reason,
            request_id,
            ..
        } = forwarded[0]
        else {
            unreachable!()
        };
        assert_eq!(resource, "/tmp/x");
        assert!(declared);
        assert_eq!(reason.as_deref(), Some("to read"));
        assert!(request_id.starts_with("perm-"));
        // And nothing was answered on the user's behalf.
        assert!(h.terminal.answers().is_empty(), "it answered by itself");
    }

    // ---- fail closed, path one ---------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn an_undeclared_prompt_is_auto_denied_and_never_shown() {
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("write", "/etc/passwd")))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert!(
            prompts(&events).is_empty(),
            "an undeclared prompt was shown"
        );
        assert!(logs(&events).iter().any(|l| l.contains("auto-denied")));

        tokio::time::advance(ANSWER_INITIAL_DELAY * 2).await;
        drain(&mut h.events).await;
        assert_eq!(h.terminal.answers(), vec!["n\n"], "it did not deny");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_permission_kind_is_auto_denied() {
        // Deno also prompts for `import`, which no loca.json can declare. The
        // matcher returns no decision at all and this is the path that turns
        // that into an `n`.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("import", "npm:evil")))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert!(prompts(&events).is_empty());
        tokio::time::advance(ANSWER_INITIAL_DELAY * 2).await;
        drain(&mut h.events).await;
        assert_eq!(h.terminal.answers(), vec!["n\n"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_with_no_policy_at_all_is_auto_denied() {
        // Pipe mode spawns with --no-prompt so this cannot happen there, but a
        // handle built without a policy must not fall open if it ever does.
        let mut h = harness(None);
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert!(prompts(&events).is_empty());
        tokio::time::advance(ANSWER_INITIAL_DELAY * 2).await;
        drain(&mut h.events).await;
        assert_eq!(h.terminal.answers(), vec!["n\n"]);
    }

    // ---- fail closed, path two ---------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_prompt_is_auto_denied_when_two_calls_share_the_worker() {
        // A Deno prompt blocks the whole isolate and carries no call id, so with
        // two calls in flight it cannot be attributed. Forwarding it would show
        // one user a prompt caused by another user's call.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert!(
            prompts(&events).is_empty(),
            "an unattributable prompt was forwarded"
        );
        assert!(logs(&events).iter().any(|l| l.contains("cannot attribute")));

        // The same prompt with one call in flight IS forwarded, which is what
        // makes the assertion above about concurrency rather than about the
        // prompt being unforwardable in general.
        h.handle.call_settled();
        h.incoming.send(Incoming::Terminal(granted_line())).unwrap();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert_eq!(prompts(&events).len(), 1);
    }

    // ---- the answer handshake ----------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn an_answer_waits_out_denos_stdin_flush_before_it_is_written() {
        // Deno's prompter flushes stdin until it has been quiet for ~100ms
        // BEFORE reading the answer, so an immediate write is eaten and the
        // prompt sits there until the give-up kill.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        let WorkerEvent::PermissionPrompt { request_id, .. } = prompts(&events)[0] else {
            unreachable!()
        };
        h.handle.answer_prompt(request_id, true);

        tokio::time::advance(ANSWER_INITIAL_DELAY / 2).await;
        drain(&mut h.events).await;
        assert!(
            h.terminal.answers().is_empty(),
            "the answer was written into Deno's flush window",
        );

        tokio::time::advance(ANSWER_INITIAL_DELAY).await;
        drain(&mut h.events).await;
        assert_eq!(h.terminal.answers(), vec!["y\n"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unconfirmed_answer_is_retried_and_then_kills_the_worker() {
        // No Granted/Denied confirmation ever arrives. The retry keeps trying,
        // and the give-up fails CLOSED by killing the worker rather than leaving
        // the call to hang.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        let WorkerEvent::PermissionPrompt { request_id, .. } = prompts(&events)[0] else {
            unreachable!()
        };
        h.handle.answer_prompt(request_id, true);

        // Stepped, not one jump. `advance` moves the clock in a single hop, and
        // a timer the driver RE-ARMS during that hop is scheduled from the new
        // now, so a single `advance(initial + retry * 3)` produces exactly one
        // write and the retry loop looks broken when it is not.
        tokio::time::advance(ANSWER_INITIAL_DELAY).await;
        drain(&mut h.events).await;
        for _ in 0..3 {
            tokio::time::advance(ANSWER_RETRY).await;
            drain(&mut h.events).await;
        }
        let retries = h.terminal.answers().len();
        assert!(retries >= 3, "it stopped retrying after {retries}");
        assert_eq!(h.terminal.kills(), 0, "it gave up before its budget");
        assert!(
            h.terminal.answers().iter().all(|a| a == "y\n"),
            "it changed its answer mid-retry: {:?}",
            h.terminal.answers(),
        );

        // Keep ticking until the give-up budget is spent. Bounded so a failure
        // is a failure rather than a hang.
        let mut events = Vec::new();
        for _ in 0..64 {
            if h.terminal.kills() > 0 {
                break;
            }
            tokio::time::advance(ANSWER_RETRY).await;
            events.extend(drain(&mut h.events).await);
        }
        assert_eq!(h.terminal.kills(), 1, "the give-up never fired");
        assert!(logs(&events).iter().any(|l| l.contains("killing worker")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_settled_prompt_stops_the_retry_loop() {
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        let WorkerEvent::PermissionPrompt { request_id, .. } = prompts(&events)[0] else {
            unreachable!()
        };
        h.handle.answer_prompt(request_id, true);
        tokio::time::advance(ANSWER_INITIAL_DELAY * 2).await;
        drain(&mut h.events).await;
        let after_first = h.terminal.answers().len();
        assert_eq!(after_first, 1);

        h.incoming.send(Incoming::Terminal(granted_line())).unwrap();
        drain(&mut h.events).await;
        tokio::time::advance(ANSWER_RETRY * 10).await;
        drain(&mut h.events).await;
        assert_eq!(
            h.terminal.answers().len(),
            after_first,
            "the retry loop kept writing after the prompt settled",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stale_response_cannot_answer_the_next_prompt() {
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        let events = drain(&mut h.events).await;
        let WorkerEvent::PermissionPrompt { request_id, .. } = prompts(&events)[0] else {
            unreachable!()
        };
        let stale = request_id.clone();
        h.incoming.send(Incoming::Terminal(granted_line())).unwrap();
        drain(&mut h.events).await;

        // A second prompt, and the user's answer to the FIRST arrives late.
        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/y")))
            .unwrap();
        drain(&mut h.events).await;
        h.handle.answer_prompt(&stale, true);
        tokio::time::advance(ANSWER_INITIAL_DELAY * 2).await;
        drain(&mut h.events).await;
        assert!(
            h.terminal.answers().is_empty(),
            "a stale response answered a different prompt: {:?}",
            h.terminal.answers(),
        );
    }

    // ---- retirement --------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn only_a_user_answered_prompt_marks_the_worker_for_retirement() {
        // Deno caches the verdict for the process lifetime; the user's answer
        // was scoped to one call. An auto-denied prompt granted nothing, so it
        // must NOT retire a warm worker (that would be a free denial of service
        // on any tool whose policy denies something).
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("write", "/etc/passwd")))
            .unwrap();
        drain(&mut h.events).await;
        h.incoming.send(Incoming::Terminal(granted_line())).unwrap();
        drain(&mut h.events).await;
        assert!(
            !h.handle.prompt_answered_by_user(),
            "an auto-denied prompt retired the worker",
        );

        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        drain(&mut h.events).await;
        h.incoming.send(Incoming::Terminal(granted_line())).unwrap();
        drain(&mut h.events).await;
        assert!(
            h.handle.prompt_answered_by_user(),
            "a forwarded prompt did not retire the worker",
        );
    }

    // ---- the send queue ----------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn frames_are_held_while_a_prompt_is_pending_and_flushed_after_it() {
        // Deno's clear_stdin flushes the terminal before reading the answer, so
        // a frame written during the prompt window is destroyed rather than
        // delivered, and the call it belonged to hangs.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.handle.send(PoolToWorker::Cancel {
            call_id: "before".to_string(),
        });
        drain(&mut h.events).await;
        assert_eq!(
            h.terminal.writes().len(),
            1,
            "a frame outside a prompt window"
        );

        h.incoming
            .send(Incoming::Terminal(prompt_block("read", "/tmp/x")))
            .unwrap();
        drain(&mut h.events).await;
        h.handle.send(PoolToWorker::Cancel {
            call_id: "during".to_string(),
        });
        drain(&mut h.events).await;
        assert_eq!(
            h.terminal.writes().len(),
            1,
            "a frame was written into the prompt's flush window",
        );

        h.incoming.send(Incoming::Terminal(granted_line())).unwrap();
        drain(&mut h.events).await;
        let writes = h.terminal.writes();
        assert_eq!(writes.len(), 2, "the queued frame was never flushed");
        assert!(writes[1].contains("during"));
    }

    // ---- the protocol stream -----------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_forged_frame_on_the_protocol_stream_reaches_no_listener() {
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.incoming
            .send(Incoming::ProtocolLine(
                r#"{"kind":"permission_prompt","requestId":"perm-x","permission":"read","resource":"/etc/shadow","declared":true}"#
                    .to_string(),
            ))
            .unwrap();
        // The positive control in the same stream: a legitimate frame does
        // arrive, so the absence above is about the forgery and not about the
        // pump being dead.
        h.incoming
            .send(Incoming::ProtocolLine(
                r#"{"kind":"tool_result","callId":"c1","result":42}"#.to_string(),
            ))
            .unwrap();
        let events = drain(&mut h.events).await;
        assert!(prompts(&events).is_empty(), "a forged prompt was delivered");
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(matches!(
            events[0],
            WorkerEvent::Frame(WorkerToPool::ToolResult { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn the_exit_settles_the_worker_once() {
        let mut h = harness(None);
        h.incoming.send(Incoming::Exited { code: Some(3) }).unwrap();
        let events = drain(&mut h.events).await;
        assert!(matches!(
            events.as_slice(),
            [WorkerEvent::WorkerExited { code: Some(3) }]
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_runaway_stderr_line_is_truncated() {
        let mut h = harness(None);
        let mut line = vec![b'x'; MAX_STDERR_LINE_BYTES + 10];
        line.push(b'\n');
        h.incoming.send(Incoming::Terminal(line)).unwrap();
        let events = drain(&mut h.events).await;
        let logs = logs(&events);
        assert_eq!(logs.len(), 1);
        assert!(logs[0].ends_with(TRUNCATION_MARKER));
        assert!(logs[0].len() <= MAX_STDERR_LINE_BYTES + TRUNCATION_MARKER.len());
    }

    #[tokio::test(start_paused = true)]
    async fn terminate_sends_shutdown_and_returns() {
        let h = harness(None);
        h.handle.terminate(Duration::from_secs(2)).await;
        let writes = h.terminal.writes();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].contains(r#""kind":"shutdown""#));
    }

    #[tokio::test(start_paused = true)]
    async fn terminate_stops_an_answer_loop_that_would_outlive_the_worker() {
        // Without this the retry keeps firing for up to the give-up budget after
        // the worker is already gone, and then issues a kill against a process
        // that no longer exists.
        let mut h = harness(Some(policy(vec![read_decl("/tmp")], GrantState::Ask)));
        h.handle.call_started();
        h.incoming
            .send(Incoming::Terminal(prompt_block("write", "/etc/passwd")))
            .unwrap();
        drain(&mut h.events).await;
        h.handle.terminate(Duration::from_secs(2)).await;
        // The shutdown frame must be WRITTEN, not queued. Clearing the pending
        // prompt is what makes that true: `send` holds frames back while a
        // prompt is pending, so a terminate that left the prompt in place would
        // put its own shutdown in the queue and then never flush it.
        assert!(
            h.terminal
                .writes()
                .iter()
                .any(|w| w.contains(r#""kind":"shutdown""#)),
            "the shutdown frame was swallowed by the prompt queue: {:?}",
            h.terminal.writes(),
        );
        let answers_at_shutdown = h.terminal.answers().len();
        tokio::time::advance(ANSWER_GIVEUP * 2).await;
        tokio::task::yield_now().await;
        assert_eq!(h.terminal.answers().len(), answers_at_shutdown);
        assert_eq!(h.terminal.kills(), 0, "it killed a worker already gone");
    }
}

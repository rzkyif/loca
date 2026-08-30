//! The Windows ConPTY backend. No termios and no echo cancellation: the worker
//! protocol rides the loopback control socket ([`super::super::socket`]), and
//! the pseudoconsole carries only the prompt text out and the y/n answer in.
//! `portable-pty` attaches ALL the child's std handles to the console, which is
//! fine precisely because the protocol is elsewhere.
//!
//! There is no Windows CI; the parts testable elsewhere (the DSR scan, the
//! console newline translation) live in [`super`].

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::{mpsc, oneshot};

use crate::error::{internal, Result};

use super::{console_newlines, scan_dsr_queries, PtyEvent, PtySpawn, Spawned, DSR_REPLY};

/// Wide, so ConPTY never wraps an `Allow?` line (which would break the parser's
/// tail pattern and hang the prompt).
const CONSOLE_COLS: u16 = 8192;
const CONSOLE_ROWS: u16 = 50;

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// A live pseudoconsole. Cheap to clone; every clone writes to the same console.
#[derive(Clone)]
pub struct PtySession {
    writer: Writer,
    kill: mpsc::UnboundedSender<()>,
}

impl PtySession {
    /// Same as [`PtySession::answer`] here: console input takes only the prompt
    /// answer. Both exist so the caller stays platform-agnostic.
    pub fn write(&self, bytes: &[u8]) {
        self.write_console(bytes);
    }

    pub fn answer(&self, bytes: &[u8]) {
        self.write_console(bytes);
    }

    pub fn kill(&self) {
        let _ = self.kill.send(());
    }

    fn write_console(&self, bytes: &[u8]) {
        let bytes = console_newlines(bytes);
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    }
}

pub fn spawn(spec: PtySpawn) -> Result<Spawned> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: CONSOLE_ROWS,
            cols: CONSOLE_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| internal(format!("openpty (conpty) failed: {e}")))?;

    let mut builder = CommandBuilder::new(&spec.program);
    builder.args(&spec.args);
    builder.env_clear();
    for (key, value) in &spec.env {
        builder.env(key, value);
    }
    if let Some(cwd) = &spec.cwd {
        builder.cwd(cwd);
    }

    let mut child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| internal(format!("{}: could not spawn: {e}", spec.label)))?;
    // Drop the slave so the master read ends when the child's handles close.
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| internal(format!("clone reader failed: {e}")))?;
    let writer: Writer = Arc::new(Mutex::new(
        pair.master
            .take_writer()
            .map_err(|e| internal(format!("take writer failed: {e}")))?,
    ));
    // Dropping the master tears down the pseudoconsole, and ConPTY never EOFs
    // its output pipe on child exit by itself, so the waiter owns it: alive
    // while the child runs, dropped right after so the reader can hit EOF.
    let master = pair.master;

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
    let (drained_tx, drained_rx) = oneshot::channel();

    // conhost blocks the whole session on its first cursor-position query, so
    // the DSR reply happens right here.
    let reader_tx = event_tx.clone();
    let dsr_writer = Arc::clone(&writer);
    std::thread::Builder::new()
        .name(format!("conpty-reader {}", spec.label))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            let mut tail: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if reader_tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                        let (queries, new_tail) = scan_dsr_queries(&tail, &buf[..n]);
                        tail = new_tail;
                        for _ in 0..queries {
                            if let Ok(mut w) = dsr_writer.lock() {
                                let _ = w.write_all(DSR_REPLY);
                                let _ = w.flush();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = drained_tx.send(());
        })
        .map_err(|e| internal(format!("{}: could not start the reader: {e}", spec.label)))?;

    // `child.wait()` blocks, so the kill request arrives over a channel.
    let pid = child.process_id();
    let killer = Arc::new(Mutex::new(child.clone_killer()));
    let kill_watcher = Arc::clone(&killer);
    tokio::spawn(async move {
        if kill_rx.recv().await.is_some() {
            if let Ok(mut killer) = kill_watcher.lock() {
                let _ = killer.kill();
            }
        }
    });

    tokio::spawn(async move {
        let code = tokio::task::spawn_blocking(move || {
            let code = match child.wait() {
                Ok(status) => Some(status.exit_code() as i32),
                Err(_) => None,
            };
            drop(master);
            code
        })
        .await
        .unwrap_or(None);
        let _ = tokio::time::timeout(super::WINDOWS_READER_DRAIN_GRACE, drained_rx).await;
        let _ = event_tx.send(PtyEvent::Exited { code });
    });

    Ok(Spawned {
        session: PtySession {
            writer,
            kill: kill_tx,
        },
        events: event_rx,
        stdout: None,
        pid,
    })
}

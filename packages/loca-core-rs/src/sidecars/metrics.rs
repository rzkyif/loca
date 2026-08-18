//! Per-process CPU and RSS, for the Services settings field.
//!
//! Replaces the npm `pidusage`. Both compute CPU as a delta between samples,
//! which is why the sampler is long-lived rather than constructed per request:
//! a fresh `System` has no previous sample to subtract and reports 0%. The
//! settings field polls every couple of seconds while it is on screen, so
//! consecutive calls give a real number.
//!
//! Only the core's own processes are ever sampled. The route picks the pids
//! (the published sidecar pids plus this process) and never takes one from a
//! request, so this is not an arbitrary-process probe.

use std::collections::BTreeMap;
use std::sync::Mutex;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// One process's live resource usage.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub rss_mb: f64,
    pub cpu_pct: f64,
}

pub struct Metrics {
    sys: Mutex<System>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(System::new()),
        }
    }

    /// Sample each pid. A pid that has already exited is simply absent from the
    /// result rather than failing the batch, so one dead child never blanks the
    /// rest of the rows.
    pub fn sample(&self, pids: &[u32]) -> BTreeMap<u32, Sample> {
        let wanted: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
        let mut sys = self.sys.lock().unwrap_or_else(|e| e.into_inner());
        // `remove_dead_processes` touches only the pids named here, so a pid
        // that has exited is dropped from the cache rather than reported from a
        // stale entry, which would be worse than reporting nothing.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&wanted),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        pids.iter()
            .filter_map(|&pid| {
                let process = sys.process(Pid::from_u32(pid))?;
                Some((
                    pid,
                    Sample {
                        rss_mb: process.memory() as f64 / 1024.0 / 1024.0,
                        cpu_pct: f64::from(process.cpu_usage()),
                    },
                ))
            })
            .collect()
    }
}

//! This (client) process's own resource usage, for the Services settings
//! field's "Main Application" row. Measures only the current process; it never
//! takes a PID from the caller.

use crate::error::AppResult;
use serde::Serialize;
use sysinfo::{Pid, ProcessesToUpdate, System};

#[derive(Serialize)]
pub struct SelfMetrics {
    pid: u32,
    #[serde(rename = "rssMb")]
    rss_mb: f64,
    #[serde(rename = "cpuPct")]
    cpu_pct: f32,
}

/// Resident memory (MB) + CPU usage (%) of this process. CPU% needs two samples
/// spaced apart, so this refreshes, waits the minimum interval, then refreshes
/// again. Sync command: Tauri runs it off the UI thread, so the short sleep is
/// harmless and the frontend polls it on a 2s cadence.
#[tauri::command]
pub fn get_self_metrics() -> AppResult<SelfMetrics> {
    let pid = std::process::id();
    let sys_pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[sys_pid]), true);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_processes(ProcessesToUpdate::Some(&[sys_pid]), true);
    let (rss_mb, cpu_pct) = sys
        .process(sys_pid)
        .map(|p| (p.memory() as f64 / 1024.0 / 1024.0, p.cpu_usage()))
        .unwrap_or((0.0, 0.0));
    Ok(SelfMetrics {
        pid,
        rss_mb,
        cpu_pct,
    })
}

/// True when this process was launched by the OS login entry: the autostart
/// plugin registers that entry with an `--autostart` argument. Drives the
/// `launch` value the frontend reports to the greeting trigger.
#[tauri::command]
pub fn was_autostarted() -> bool {
    std::env::args().any(|a| a == "--autostart")
}

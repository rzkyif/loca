//! Windows orphan cleanup: a Job Object every spawned child is assigned to.
//!
//! The job is created with `KILL_ON_JOB_CLOSE` and the core holds the only
//! handle, so when the core dies, gracefully or via `taskkill /F`, the kernel
//! terminates every assigned process. It is the only way to guarantee
//! orphan-free cleanup on Windows when the core never reaches its shutdown path.
//!
//! Windows-only by necessity: systemd kills the unit's cgroup and launchd the
//! job's process group, while Windows Task Scheduler does neither. See the
//! process-group note in [`super::process`].
//!
//! Best effort throughout: a failure means a child could outlive a hard core
//! kill, which is worth a warning and never worth failing a spawn over.

/// A handle to the kill-on-close job. Constructed once per [`super::Manager`];
/// a no-op on every platform except Windows.
pub(super) struct JobObject {
    /// The raw `HANDLE`, stored as an integer so this struct stays `Send` and
    /// `Sync` without an unsafe impl. Zero means "no job", which covers both
    /// the non-Windows build and a failed creation.
    #[cfg_attr(not(windows), allow(dead_code))]
    handle: isize,
}

impl JobObject {
    #[cfg(not(windows))]
    pub(super) fn new() -> Self {
        Self { handle: 0 }
    }

    #[cfg(windows)]
    pub(super) fn new() -> Self {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // SAFETY: a null security descriptor and a null name are both
        // documented as "default, unnamed", and the returned handle is checked
        // before use.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            tracing::warn!(
                target: "loca::sidecars",
                "could not create the job object ({}); children may survive a hard core kill",
                std::io::Error::last_os_error(),
            );
            return Self { handle: 0 };
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a correctly sized, fully initialized struct of the
        // class named, and the handle is the one just created.
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            tracing::warn!(
                target: "loca::sidecars",
                "could not set kill-on-close on the job object ({}); children may survive a hard core kill",
                std::io::Error::last_os_error(),
            );
            // SAFETY: closing a job with no assigned process kills nothing.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Self { handle: 0 };
        }

        tracing::info!(target: "loca::sidecars", "job object created; children will be tracked");
        Self {
            handle: handle as isize,
        }
    }

    #[cfg(not(windows))]
    pub(super) fn track(&self, _pid: u32, _label: &str) {}

    #[cfg(windows)]
    pub(super) fn track(&self, pid: u32, label: &str) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        if self.handle == 0 {
            return;
        }
        // SAFETY: the pid belongs to a child we just spawned and have not
        // reaped, so it cannot have been reused.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            tracing::warn!(
                target: "loca::sidecars",
                "{label}: could not open pid {pid} to track it ({})",
                std::io::Error::last_os_error(),
            );
            return;
        }
        // SAFETY: both handles are open and owned by this process.
        let assigned = unsafe { AssignProcessToJobObject(self.handle as _, process) };
        if assigned == 0 {
            tracing::warn!(
                target: "loca::sidecars",
                "{label}: could not track pid {pid} ({}); it may survive a hard core kill",
                std::io::Error::last_os_error(),
            );
        }
        // SAFETY: our own duplicate; the job holds its own reference.
        unsafe { CloseHandle(process) };
    }
}

// Deliberately no `Drop`: closing the job handle terminates every assigned
// process at once, racing the graceful shutdown. The core exiting closes it as
// its last act, which is the ordering wanted.

//! Platform process-tree control.
//!
//! * Unix: the child is the leader of its own process group
//!   (`process_group(0)`, v1's `detached: true`), so signalling `-pgid`
//!   reaches `setup_env.py`'s `worker.py`, dataloader workers, and so on.
//! * Windows: the child is spawned with `CREATE_NO_WINDOW` (v1's
//!   `windowsHide: true`) and put in a Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Terminating the job ends the whole
//!   tree; and if GradApp itself dies, the OS closes the job handle and the
//!   tree goes with it (v1 had to shell out to `taskkill /T /F`).

use tokio::process::{Child, Command};

use crate::StopSignal;

/// Per-spawn tree handle. Dropping it does not kill anything on Unix; on
/// Windows dropping closes the job, which kills the tree (by design).
pub(crate) struct Tree {
    #[cfg(unix)]
    pgid: Option<i32>,
    #[cfg(windows)]
    job: Option<win::Job>,
}

pub(crate) fn prepare(cmd: &mut Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = cmd;
}

impl Tree {
    pub(crate) fn attach(child: &Child) -> Tree {
        #[cfg(unix)]
        {
            Tree {
                pgid: child.id().map(|p| p as i32),
            }
        }
        #[cfg(windows)]
        {
            let job = match win::Job::new() {
                Ok(job) => match job.assign(child) {
                    Ok(()) => Some(job),
                    Err(e) => {
                        tracing::warn!("could not put the child in a job object: {e}");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!("could not create a job object: {e}");
                    None
                }
            };
            Tree { job }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Tree {}
        }
    }

    /// Ask politely. Returns false when there is no polite way (Windows:
    /// a windowless console process cannot receive Ctrl+C), in which case
    /// the caller should go straight to [`Tree::kill`].
    pub(crate) fn signal(&self, sig: StopSignal) -> bool {
        #[cfg(unix)]
        {
            let Some(pgid) = self.pgid else { return false };
            let s = match sig {
                StopSignal::Terminate => libc::SIGTERM,
                StopSignal::Interrupt => libc::SIGINT,
            };
            // SAFETY: plain syscall; a negative pid addresses the process group.
            unsafe { libc::kill(-pgid, s) == 0 }
        }
        #[cfg(not(unix))]
        {
            let _ = sig;
            false
        }
    }

    /// Kill the whole tree now. Safe to call after the leader exited (sweeps
    /// stragglers that still hold the group / job).
    pub(crate) fn kill(&self, child: &mut Child) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            // SAFETY: as above. ESRCH (group already empty) is fine.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        // The direct child, in case the group/job could not be set up.
        let _ = child.start_kill();
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::ptr::null;

    use tokio::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub(crate) struct Job(HANDLE);

    // SAFETY: a job handle is a kernel object handle, usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub(crate) fn new() -> io::Result<Job> {
            // SAFETY: documented Win32 calls with valid arguments; the handle is
            // owned by `Job` and closed in Drop.
            unsafe {
                let handle = CreateJobObjectW(null(), null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Job(handle);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        /// There is a short window between spawn and assignment in which a
        /// grandchild would escape the job; Python needs far longer than that
        /// to start one, so in practice every descendant lands in the job.
        pub(crate) fn assign(&self, child: &Child) -> io::Result<()> {
            let Some(process) = child.raw_handle() else {
                return Err(io::Error::other("child already reaped"));
            };
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.0, process) };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }

        pub(crate) fn terminate(&self) {
            // SAFETY: valid job handle.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle. Closing the last handle of a
            // KILL_ON_JOB_CLOSE job ends any process still in it.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

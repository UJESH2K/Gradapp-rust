//! How gm-ports starts child processes.
//!
//! GradApp is a GUI program on Windows. A console program it starts (uv,
//! python) gets a console window of its own unless told otherwise: a black
//! window flashes up for every install step, and if the person closes it the
//! step dies with STATUS_CONTROL_C_EXIT (0xC000013A, exit -1073741510). That
//! is what a 2.0.0 install on Windows showed. Every command here is created
//! through [`hidden`].

use std::ffi::OsStr;
use tokio::process::Command;

/// A command that never opens a console window.
pub(crate) fn hidden(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Free bytes on the volume holding `path` (or its nearest existing parent).
pub(crate) fn free_bytes(path: &std::path::Path) -> Option<u64> {
    let mut p = path;
    loop {
        if p.exists() {
            return fs4::available_space(p).ok();
        }
        p = p.parent()?;
    }
}

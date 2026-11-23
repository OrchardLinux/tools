//! Handing open files to the commands a session runs.
//!
//! A child process inherits an open file as a numbered descriptor, and the
//! number travels in an environment variable. What the file gives its
//! holders (a lock, or content no other user can reach) is then shared by
//! the whole session and gone with its last process.
#[cfg(not(unix))]
use orchard_cache::Code;
use orchard_cache::{Error, Result};
use std::fs::{File, Metadata};
use std::process::Command;

/// Lets `command` inherit `file` and tells it the descriptor in `var`.
pub fn pass(file: &File, command: &mut Command, var: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: `fd` is our own open file; clearing its close-on-exec
        // flag changes nothing but what child processes inherit.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } != 0 {
            return Err(Error::from(std::io::Error::last_os_error()).context("session file"));
        }
        command.env(var, fd.to_string());
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (file, command, var);
        Err(Error::new(
            Code::Unsupported,
            "sessions for scripts need macOS or Linux",
        ))
    }
}

/// The file a surrounding session left open for us under the descriptor
/// named in `var`, if `is_it` recognises it.
pub fn take(var: &str, is_it: impl FnOnce(&Metadata) -> bool) -> Option<File> {
    let fd = std::env::var(var).ok()?.parse().ok()?;
    adopt(fd, is_it)
}

#[cfg(unix)]
pub(crate) fn adopt(fd: i32, is_it: impl FnOnce(&Metadata) -> bool) -> Option<File> {
    use std::mem::ManuallyDrop;
    use std::os::fd::FromRawFd;
    // SAFETY: the number comes from the environment, so it may be any
    // descriptor of ours or none. It is only looked at and duplicated, and
    // never closed: `ManuallyDrop` keeps it open for our own child processes.
    let original = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    if !is_it(&original.metadata().ok()?) {
        return None;
    }
    // The duplicate shares the open file: its lock, and its content.
    original.try_clone().ok()
}

#[cfg(not(unix))]
pub(crate) fn adopt(_fd: i32, _is_it: impl FnOnce(&Metadata) -> bool) -> Option<File> {
    None
}

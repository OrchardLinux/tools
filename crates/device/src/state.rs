//! What a session's commands share: the deadline, and what the device has
//! reported about itself.
//!
//! The state lives in a file that has no name: it is created, opened and
//! removed at once, so only processes that inherit the open file can reach
//! it, and it is gone with the last of them. Nothing about a session stays
//! on disk.
use crate::inherit;
use orchard_cache::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Names the inherited state file's descriptor for commands in a session.
const FD_VAR: &str = "ORCHARD_SESSION_STATE_FD";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// When the session's time is up, in milliseconds since 1970, and the
    /// limit that was set, for the message when it runs out.
    pub deadline_unix_ms: Option<u64>,
    pub limit_ms: Option<u64>,
    /// What the device has reported about itself during the session.
    pub ecid: Option<String>,
    pub chip: Option<String>,
    pub board_id: Option<String>,
    #[serde(default)]
    pub serial_number: Option<String>,
}

/// The open state file of a session that runs commands.
#[derive(Debug)]
pub struct Shared(File);

impl Shared {
    /// Makes the file for a session that is about to run a command.
    pub fn create() -> Result<Shared> {
        Self::make().map(|(file, _)| Shared(file))
    }

    /// Creates the nameless state file and returns it with the path it briefly
    /// had, which `make` has already unlinked. The name carries a per-process
    /// counter as well as the time, so concurrent creates never collide on it.
    fn make() -> Result<(File, PathBuf)> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("orchard2-session-{}-{seq}-{nanos}", std::process::id());
        let path = std::env::temp_dir().join(name);
        let mut options = File::options();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).context(path.display())?;
        std::fs::remove_file(&path).context(path.display())?;
        Ok((file, path))
    }

    /// The file of the session this process was started in.
    pub fn inherited() -> Option<Shared> {
        inherit::take(FD_VAR, is_ours).map(Shared)
    }

    pub fn hand_to(&self, command: &mut std::process::Command) -> Result<()> {
        inherit::pass(&self.0, command, FD_VAR)
    }

    /// The state as last written; empty when nothing has been.
    pub fn read(&self) -> State {
        let mut file = &self.0;
        let mut text = String::new();
        let read = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.read_to_string(&mut text));
        match read {
            Ok(_) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => State::default(),
        }
    }

    pub fn write(&self, state: &State) -> Result<()> {
        let mut file = &self.0;
        let text = serde_json::to_vec(state).expect("a state serializes");
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(&text))
            .context("session state")
    }
}

/// A state file is a plain file of this user's that has no name any more.
#[cfg(unix)]
fn is_ours(file: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid only reads the process's user id.
    let user = unsafe { libc::getuid() };
    file.is_file() && file.nlink() == 0 && file.uid() == user
}

#[cfg(not(unix))]
fn is_ours(_file: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_replaced_whole_and_leaves_no_file() {
        let (file, path) = Shared::make().unwrap();
        let shared = Shared(file);
        assert_eq!(shared.read(), State::default());
        let long = State {
            ecid: Some("00000000000000a1".into()),
            chip: Some("8010".into()),
            deadline_unix_ms: Some(u64::MAX),
            ..State::default()
        };
        shared.write(&long).unwrap();
        assert_eq!(shared.read(), long);
        shared.write(&State::default()).unwrap();
        assert_eq!(shared.read(), State::default());

        // `make` unlinks the file before returning it; this exact path is gone,
        // and checking it (not a shared prefix) cannot race another session.
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_a_nameless_file_of_ours_is_taken_as_state() {
        use std::os::fd::AsRawFd;
        let shared = Shared::create().unwrap();
        shared
            .write(&State {
                chip: Some("8000".into()),
                ..State::default()
            })
            .unwrap();
        let again = Shared(inherit::adopt(shared.0.as_raw_fd(), is_ours).unwrap());
        assert_eq!(again.read().chip.as_deref(), Some("8000"));

        let named = tempfile::NamedTempFile::new().unwrap();
        assert!(inherit::adopt(named.as_file().as_raw_fd(), is_ours).is_none());
    }
}

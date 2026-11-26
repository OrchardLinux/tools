//! One piece of work on a device at a time: the boot commands' sessions
//! and the installer's and launcher's runs all take this lock.
//!
//! The lock is a file named after the device's USB location, which stays
//! the same while the device changes modes. The operating system releases
//! it when the last process holding it ends, however it ends, so no lock is
//! ever left behind and lock files are never deleted.
//!
//! A device belongs to the machine, so the files are shared by all its
//! users and writable by all of them. They therefore hold nothing a session
//! relies on: only a note saying who has the device, for the message to
//! whoever is refused.
use crate::inherit;
use crate::{Code, Context, Error, Result};
use std::fs::{self, File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Names the inherited lock's file descriptor for commands run in a session.
const FD_VAR: &str = "ORCHARD_SESSION_FD";
/// Names the location of the device the inherited lock is for.
const LOCATION_VAR: &str = "ORCHARD_SESSION_LOCATION";

/// Holds a device until dropped.
#[derive(Debug)]
pub struct Lock {
    file: File,
    location: String,
    inherited: bool,
    /// The lock's file, while this process offers the lock to its own work.
    offered: Option<PathBuf>,
}

/// The locks this process offers to work it starts itself: a window that
/// holds a device hands it to the session that then works on it.
static OFFERED: std::sync::Mutex<Vec<(PathBuf, File)>> = std::sync::Mutex::new(Vec::new());

/// The location of the device this process's surrounding session holds.
pub fn session_location() -> Option<String> {
    std::env::var(LOCATION_VAR).ok().filter(|l| !l.is_empty())
}

impl Lock {
    /// Takes the lock for the device at `location`, or fails at once when
    /// other work holds it. Inside a session for that device, the session's
    /// own lock is used.
    pub fn acquire(location: &str) -> Result<Lock> {
        Lock::acquire_in(&default_dir(), location)
    }

    /// The same, with the lock files in `dir`.
    pub fn acquire_in(dir: &Path, location: &str) -> Result<Lock> {
        fs::create_dir_all(dir).context(dir.display())?;
        open_to_all(dir);
        // Locations are hex numbers or bus and port numbers; anything else
        // must not become a path.
        let name: String = location
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let path = dir.join(format!("{name}.lock"));

        let from_process = OFFERED
            .lock()
            .expect("offered locks")
            .iter()
            .find(|(offered, _)| *offered == path)
            .and_then(|(_, file)| file.try_clone().ok());
        let from_session = from_process.or_else(|| {
            session_location()
                .filter(|held| held == location)
                .and_then(|_| inherit::take(FD_VAR, |file| same_file(file, &path)))
        });
        let inherited = from_session.is_some();
        let file = match from_session {
            Some(file) => file,
            None => open(&path)?,
        };
        // On an inherited file this confirms the lock rather than taking it.
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(in_use(location, &file)),
            Err(TryLockError::Error(e)) => return Err(Error::from(e).context(path.display())),
        }
        let lock = Lock {
            file,
            location: location.to_owned(),
            inherited,
            offered: None,
        };
        if !inherited {
            lock.note(&format!("pid {}: {}", std::process::id(), program()));
        }
        Ok(lock)
    }

    /// Lets work this process starts later on the same device continue
    /// this lock, as a command run in a session continues the session's,
    /// for as long as this lock is held.
    pub fn offer(&mut self, dir: Option<&Path>) -> Result<()> {
        let name: String = self.location.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let path = dir.map_or_else(default_dir, Path::to_owned).join(format!("{name}.lock"));
        let copy = self.file.try_clone().context(path.display())?;
        OFFERED.lock().expect("offered locks").push((path.clone(), copy));
        self.offered = Some(path);
        Ok(())
    }

    /// Whether the lock was taken by a surrounding session, which this
    /// process continues.
    pub fn inherited(&self) -> bool {
        self.inherited
    }

    /// Sets `command` up to inherit this lock: its orchard2 commands on the
    /// same device then continue this session.
    pub fn hand_to(&self, command: &mut std::process::Command) -> Result<()> {
        inherit::pass(&self.file, command, FD_VAR)?;
        command.env(LOCATION_VAR, &self.location);
        Ok(())
    }

    /// Replaces the note in the lock file. A file created by another user
    /// may not be writable; the lock holds all the same.
    fn note(&self, text: &str) {
        let mut file = &self.file;
        let _ = file
            .set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(text.as_bytes()));
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(path) = self.offered.take() {
            OFFERED.lock().expect("offered locks").retain(|(offered, _)| *offered != path);
        }
        // The one who took the lock takes its note away again.
        if !self.inherited {
            self.note("");
        }
    }
}

/// This program and its command, such as `orchard2 session`.
fn program() -> String {
    let mut args = std::env::args();
    let name = args.next().unwrap_or_default();
    let name = Path::new(&name)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    match args.find(|arg| !arg.starts_with('-')) {
        Some(command) if is_word(&command) => format!("{name} {command}"),
        _ => name,
    }
}

/// A command name, as opposed to a flag's value that happens to come first.
fn is_word(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

/// Opens the lock file, creating it writable for every user.
fn open(path: &Path) -> Result<File> {
    let both = || File::options().read(true).write(true).open(path);
    let file = match both() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let created = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path);
            #[cfg(unix)]
            if created.is_ok() {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o666));
            }
            created
        }
        // Another user's file that could not be opened up: reading is
        // enough to lock it.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => File::open(path),
        opened => opened,
    };
    file.context(path.display())
}

/// Whether an open file is the one at `path`.
#[cfg(unix)]
fn same_file(open: &fs::Metadata, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).is_ok_and(|named| (open.dev(), open.ino()) == (named.dev(), named.ino()))
}

#[cfg(not(unix))]
fn same_file(_open: &fs::Metadata, _path: &Path) -> bool {
    false
}

/// How a device that other work holds is described, in the refusal and
/// where it is listed; who holds it follows where that is known.
pub const IN_USE: &str = "in use by another orchard2";

/// Who holds the device a refusal is about, as its note names them:
/// `(pid 12: orchard2 load)`, or nothing.
pub fn holder(refusal: &Error) -> &str {
    refusal.message.split_once(IN_USE).map_or("", |(_, who)| who.trim())
}

fn in_use(location: &str, mut file: &File) -> Error {
    // The note is anyone's to write, so only a short, plain one is shown.
    let mut note = String::new();
    let read = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.take(200).read_to_string(&mut note));
    let plain = |c: char| c.is_ascii_alphanumeric() || " :-_.".contains(c);
    let holder = match read {
        Ok(n) if n > 0 && note.chars().all(plain) => format!(" ({note})"),
        _ => String::new(),
    };
    Error::new(
        Code::Locked,
        format!("the device at {location} is {IN_USE}{holder}"),
    )
    .hint("wait for it to finish, or stop it")
}

fn default_dir() -> PathBuf {
    if cfg!(unix) {
        PathBuf::from("/tmp/orchard2-locks")
    } else {
        std::env::temp_dir().join("orchard2-locks")
    }
}

/// Lets every user create lock files in `dir`. Only its owner can, and for
/// anyone else it has been done already.
fn open_to_all(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o1777));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offered_lock_is_continued_by_the_process_and_by_nobody_after() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let mut held = Lock::acquire_in(dir, "0x7").unwrap();
        assert_eq!(Lock::acquire_in(dir, "0x7").unwrap_err().code, Code::Locked);
        held.offer(Some(dir)).unwrap();
        let continued = Lock::acquire_in(dir, "0x7").unwrap();
        assert!(continued.inherited());
        // The device stays held while either has it, and the note stays the holder's.
        drop(continued);
        assert!(!fs::read_to_string(dir.join("0x7.lock")).unwrap().is_empty());
        drop(held);
        assert!(!Lock::acquire_in(dir, "0x7").unwrap().inherited());
    }

    #[test]
    fn one_holder_per_device() {
        let dir = tempfile::tempdir().unwrap();
        let held = Lock::acquire_in(dir.path(), "0x100000").unwrap();
        let second = Lock::acquire_in(dir.path(), "0x100000").unwrap_err();
        assert_eq!(second.code, Code::Locked);
        // Another device is free, and so is this one once released.
        Lock::acquire_in(dir.path(), "0x200000").unwrap();
        drop(held);
        Lock::acquire_in(dir.path(), "0x100000").unwrap();
    }

    #[test]
    fn a_location_cannot_leave_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = Lock::acquire_in(dir.path(), "../001-2.3").unwrap();
        assert!(dir.path().join("---001-2-3.lock").exists());
    }

    #[test]
    fn the_holder_is_named_while_it_holds_and_not_after() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0x100000.lock");
        let held = Lock::acquire_in(dir.path(), "0x100000").unwrap();
        assert!(!held.inherited());
        let note = fs::read_to_string(&path).unwrap();
        assert!(note.starts_with(&format!("pid {}: ", std::process::id())));

        let refused = Lock::acquire_in(dir.path(), "0x100000").unwrap_err();
        assert!(refused.message.ends_with(&format!("({note})")));
        assert_eq!(holder(&refused), format!("({note})"));

        drop(held);
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn a_note_someone_else_wrote_is_not_repeated() {
        let dir = tempfile::tempdir().unwrap();
        let _held = Lock::acquire_in(dir.path(), "0x100000").unwrap();
        fs::write(dir.path().join("0x100000.lock"), "x\x1b[2J\nsecret=1").unwrap();
        let refused = Lock::acquire_in(dir.path(), "0x100000").unwrap_err();
        assert!(refused.message.ends_with("another orchard2"));
        assert_eq!(holder(&refused), "");
    }

    #[test]
    fn command_names() {
        assert!(is_word("session"));
        assert!(is_word("clear-status"));
        assert!(!is_word("/tmp/cache"));
        assert!(!is_word("0x100000"));
    }

    #[cfg(unix)]
    #[test]
    fn an_inherited_file_carries_the_lock() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let held = Lock::acquire_in(dir.path(), "0x100000").unwrap();
        let path = dir.path().join("0x100000.lock");

        // What a command run by the session does with the descriptor.
        let passed_on = inherit::adopt(held.file.as_raw_fd(), |f| same_file(f, &path)).unwrap();
        passed_on.try_lock().unwrap();
        // The lock now lives as long as either of the two.
        drop(held);
        let outsider = Lock::acquire_in(dir.path(), "0x100000").unwrap_err();
        assert_eq!(outsider.code, Code::Locked);
        drop(passed_on);
        Lock::acquire_in(dir.path(), "0x100000").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_descriptor_that_is_not_the_lock_file_is_left_alone() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let _held = Lock::acquire_in(dir.path(), "0x100000").unwrap();
        let path = dir.path().join("0x100000.lock");
        let mut other = tempfile::tempfile().unwrap();
        assert!(inherit::adopt(other.as_raw_fd(), |f| same_file(f, &path)).is_none());
        // Still open and ours.
        other.write_all(b"still open").unwrap();
    }
}

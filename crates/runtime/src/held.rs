//! The devices a run holds, so that only one piece of work touches a
//! device (`orchard_cache::lock`, shared with the boot commands).
//!
//! Nothing is started on a device before it is in DFU mode, so that is when
//! it is taken: before the flow hears of it there. It is kept for the
//! flow's device at its USB port, through its modes and while it is briefly
//! gone between them, and let go when the flow has dropped it. A window
//! that is only open holds nothing.
use orchard_cache::lock::{self, Lock};
use orchard_cache::Code;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Default)]
pub struct Held {
    /// Where the lock files are; None: the lock's own place.
    dir: Option<PathBuf>,
    locks: BTreeMap<String, Lock>,
    /// Ports whose lock could not be used, each reported once.
    unusable: BTreeSet<String>,
}

/// What one look at USB gave.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// Ports with a device in DFU mode that other work holds, and who holds
    /// it where that is known (`(pid 12: orchard load)`).
    pub in_use: Vec<(String, String)>,
    /// Locks that could not be used; the run goes on without them.
    pub problems: Vec<String>,
}

impl Held {
    #[cfg(test)]
    fn in_dir(dir: &std::path::Path) -> Held {
        Held { dir: Some(dir.to_owned()), ..Held::default() }
    }

    /// Takes the devices now in DFU mode (`dfu`: their ports), keeps the
    /// one the flow works on (`working_at`: its port), and lets go of every
    /// other.
    pub fn look(&mut self, dfu: &[&str], working_at: Option<&str>) -> Look {
        self.locks.retain(|port, _| dfu.contains(&port.as_str()) || working_at == Some(port.as_str()));
        let mut look = Look::default();
        for port in dfu {
            if self.locks.contains_key(*port) {
                continue;
            }
            let taken = match &self.dir {
                Some(dir) => Lock::acquire_in(dir, port),
                None => Lock::acquire(port),
            };
            match taken {
                Ok(mut lock) => {
                    // The session that works on the device continues it.
                    let _ = lock.offer(self.dir.as_deref());
                    self.locks.insert((*port).to_owned(), lock);
                }
                Err(e) if e.code == Code::Locked => {
                    look.in_use.push(((*port).to_owned(), lock::holder(&e).to_owned()));
                }
                // No lock to be had, such as a directory that is not ours to
                // write: the device is worked on without one.
                Err(e) => {
                    if self.unusable.insert((*port).to_owned()) {
                        look.problems.push(format!("device lock: {e}"));
                    }
                }
            }
        }
        look
    }

    /// Takes the device the flow took up without a DFU sighting (an install
    /// system or the companion found running). Who else holds it is only
    /// reported: the flow works on it already.
    pub fn adopt(&mut self, port: &str) -> Option<String> {
        if self.locks.contains_key(port) || self.unusable.contains(port) {
            return None;
        }
        let taken = match &self.dir {
            Some(dir) => Lock::acquire_in(dir, port),
            None => Lock::acquire(port),
        };
        match taken {
            Ok(mut lock) => {
                let _ = lock.offer(self.dir.as_deref());
                self.locks.insert(port.to_owned(), lock);
                None
            }
            Err(e) => {
                self.unusable.insert(port.to_owned());
                Some(format!("device lock: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orchard-held-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Whether other work could take the device: asked as another process
    /// would, since this one continues a lock it holds itself.
    fn free(dir: &std::path::Path, port: &str) -> bool {
        // No lock file yet: nobody has ever held it.
        match std::fs::File::options().read(true).write(true).open(dir.join(format!("{port}.lock"))) {
            Ok(file) => file.try_lock().is_ok(),
            Err(_) => true,
        }
    }

    /// Free within a moment: another test's child holds a copy of a
    /// released lock between its fork and its exec.
    fn freed(dir: &std::path::Path, port: &str) -> bool {
        (0..200).any(|_| {
            free(dir, port) || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        })
    }

    #[test]
    fn a_device_is_held_from_dfu_through_its_modes_and_let_go_after() {
        let dir = scratch("modes");
        let mut held = Held::in_dir(&dir);
        // Connected, running iOS: a window that is only open holds nothing.
        assert_eq!(held.look(&[], None), Look::default());
        assert!(freed(&dir, "0x1"));
        // In DFU mode: taken before the flow hears of it.
        assert_eq!(held.look(&["0x1"], None), Look::default());
        assert!(!free(&dir, "0x1"));
        // The flow works on it: kept in Pongo and m1n1, and while it is
        // gone from USB between two modes.
        for _ in 0..3 {
            assert_eq!(held.look(&[], Some("0x1")), Look::default());
            assert!(!free(&dir, "0x1"));
        }
        // The flow dropped it and it is not in DFU mode: let go.
        held.look(&[], None);
        assert!(freed(&dir, "0x1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_device_other_work_holds_is_named_with_its_holder_and_taken_when_free() {
        let dir = scratch("in-use");
        let other = Lock::acquire_in(&dir, "0x1").unwrap();
        let note = std::fs::read_to_string(dir.join("0x1.lock")).unwrap();
        let mut held = Held::in_dir(&dir);
        let look = held.look(&["0x1", "0x2"], None);
        assert_eq!(look.in_use, [("0x1".to_owned(), format!("({note})"))]);
        assert!(look.problems.is_empty());
        // The free one was taken all the same.
        assert!(!free(&dir, "0x2"));
        drop(other);
        // Taken as soon as the other's lock is gone.
        let taken = (0..200).any(|_| {
            held.look(&["0x1", "0x2"], None) == Look::default() || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        });
        assert!(taken && !free(&dir, "0x1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_device_found_running_is_taken_when_the_flow_takes_it_up() {
        let dir = scratch("adopt");
        let mut held = Held::in_dir(&dir);
        assert_eq!(held.adopt("0x1"), None);
        assert!(!free(&dir, "0x1"));
        assert_eq!(held.look(&[], Some("0x1")), Look::default());
        assert!(!free(&dir, "0x1"));
        // Held by other work: said once, and the run goes on.
        let other = Lock::acquire_in(&dir, "0x2").unwrap();
        assert!(held.adopt("0x2").is_some_and(|said| said.contains("in use")));
        assert_eq!(held.adopt("0x2"), None);
        drop(other);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

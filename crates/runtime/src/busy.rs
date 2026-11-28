//! `--busy-cpu` (`ORCHARD_BUSY_CPU=1`), Linux: the exploit tool runs on one
//! CPU core of its own while every other core is kept busy, until the
//! exploit is through ("Checkmate") or the tool ends.
//!
//! On some computers checkm8 only succeeds while the computer is under load.
//! Measured on one (Ryzen 5 3600, palera1n 2.4, an iPhone 8 Plus): idle, 0 of
//! 6 starts got in; with the tool on one core and the others busy, 7 of 7,
//! within 1 to 8 seconds. Keeping the CPU out of its sleep states or at its
//! highest clock did not do it, and a busy loop on the tool's own core made
//! it worse.
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// The core for the tool (the last this process may use) and the cores to
/// keep busy (all others). None where the switch is off, the system has no
/// way to place work on a core, or there is only one core.
pub fn plan() -> Option<(usize, Vec<usize>)> {
    if !crate::flags::busy_cpu() {
        return None;
    }
    let mut cores = allowed();
    if cores.len() < 2 {
        eprintln!("busy-cpu: does nothing on this system");
        return None;
    }
    let tool = cores.pop()?;
    Some((tool, cores))
}

#[cfg(target_os = "linux")]
fn allowed() -> Vec<usize> {
    // SAFETY: a zeroed cpu_set_t is an empty set; the call fills ours.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Vec::new();
        }
        (0..libc::CPU_SETSIZE as usize).filter(|c| libc::CPU_ISSET(*c, &set)).collect()
    }
}

#[cfg(not(target_os = "linux"))]
fn allowed() -> Vec<usize> {
    Vec::new()
}

/// Puts the calling thread (or, between fork and exec, the new process) on
/// one core.
#[cfg(target_os = "linux")]
fn stay_on(core: usize) -> bool {
    // SAFETY: only a system call on a set of our own.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(core, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

#[cfg(not(target_os = "linux"))]
fn stay_on(_core: usize) -> bool {
    false
}

/// Makes `command`, and what it starts, run on `core` only.
pub fn pin(command: &mut Command, core: usize) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure makes one system call and touches no memory
        // the parent shares.
        unsafe {
            command.pre_exec(move || {
                stay_on(core);
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (command, core);
}

/// The cores kept busy while the exploit runs.
pub struct Busy {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Busy {
    /// One busy thread on each of `cores`, until "Checkmate" shows in the
    /// tool's `log` or `finish`.
    pub fn start(log: PathBuf, cores: &[usize]) -> Busy {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads: Vec<JoinHandle<()>> = cores
            .iter()
            .map(|&core| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    stay_on(core);
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        for _ in 0..1 << 16 {
                            n = std::hint::black_box(n.wrapping_add(1));
                        }
                    }
                })
            })
            .collect();
        let flag = stop.clone();
        threads.push(std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if crate::replug::after_checkmate(&log).is_some() {
                    orchard_cache::runlog::line("orchard", "busy CPU: the exploit is through, the busy cores stop");
                    flag.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }));
        Busy { stop, threads }
    }

    pub fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busy_cores_stop_at_checkmate_and_when_finished() {
        let log = std::env::temp_dir().join(format!("orchard-busy-{}", std::process::id()));
        std::fs::write(&log, "UaF race: heuristic strategy failed\n").unwrap();
        let busy = Busy::start(log.clone(), &[0, 1]);
        std::thread::sleep(Duration::from_millis(150));
        assert!(!busy.stop.load(Ordering::Relaxed));
        std::fs::write(&log, "Entered initial checkm8 state after 76 steps\nCheckmate!\n").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(busy.stop.load(Ordering::Relaxed), "stopped by the log");
        busy.finish();

        std::fs::write(&log, "UaF race: heuristic strategy failed\n").unwrap();
        Busy::start(log.clone(), &[0]).finish();
        let _ = std::fs::remove_file(&log);
    }
}

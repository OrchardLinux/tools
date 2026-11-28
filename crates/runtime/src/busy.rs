//! `--busy-cpu` (`ORCHARD_BUSY_CPU=1`), Linux: the exploit tool runs on one
//! CPU core of its own while every other core is kept busy, until the tool
//! ends: through the exploit and the device's return in download mode, to
//! PongoOS. The cores stay busy past "Checkmate" because the one run in
//! which they stopped there was also the one in which the device did not
//! come back; every measured run that came back had them busy throughout
//! or not at all.
//!
//! On some computers checkm8 only succeeds while the computer is under load.
//! Measured on one (Ryzen 5 3600, palera1n 2.4, an iPhone 8 Plus): idle, 0 of
//! 6 starts got in; with the tool on one core and the others busy, 7 of 7,
//! within 1 to 8 seconds. Keeping the CPU out of its sleep states or at its
//! highest clock did not do it, and a busy loop on the tool's own core made
//! it worse.
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

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

/// The cores kept busy while the exploit tool runs.
pub struct Busy {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Busy {
    /// One busy thread on each of `cores`, until `finish`.
    pub fn start(cores: &[usize]) -> Busy {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = cores
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
        Busy { stop, threads }
    }

    pub fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
        orchard_cache::runlog::line("orchard", "busy CPU: the tool has ended, the busy cores stop");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busy_cores_run_until_finished() {
        let busy = Busy::start(&[0, 1]);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(busy.threads.iter().all(|t| !t.is_finished()));
        busy.finish();
    }
}

//! Following the device after checkm8, from palera1n's log: whether it came
//! back, and (Linux) resetting its USB port when it does not (orchard's
//! `apple/replug.py`, from OrchardLinux/cli#11). The replug dialog
//! (chain.rs) uses the same reading and timing.
//!
//! After "Checkmate!" the device drops off USB and must come back in download
//! mode. On some Linux hosts it never does until the cable is replugged at
//! the device within palera1n's 20-second window; disabling and re-enabling
//! the hub port forces the re-detection (on a J71b the device came back 4 s
//! after a reset where three bare attempts never did). The port's `disable`
//! control is root-only; the udev rule of the USB access setup makes it
//! writable for the USB group while a device is in DFU. Without it, and on
//! macOS, nothing happens.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// palera1n 2.4 prints "Checkmate!", then a line when the device drops off.
// 3.0 prints "Checkmate?" and no drop line, so its wait starts there.
const CHECKMATE_2: &str = "Checkmate!";
const CHECKMATE_3: &str = "Checkmate?";
const DISCONNECTED: &str = "DFU mode device disconnected";
const RETURNED: [&str; 3] = ["Entered download mode", "Download mode device found", "Found download mode device"];

/// How long to wait before and between resets.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// After palera1n 2.4's drop line.
    pub settle: Duration,
    /// After palera1n 3.0's "Checkmate?", which comes before the drop. Also
    /// when the replug dialog shows where nothing resets the port (macOS):
    /// where the device comes back by itself, this keeps the dialog from
    /// flashing; not yet measured on such a host.
    pub settle_3: Duration,
    pub off: Duration,
    pub wait: Duration,
    pub attempts: u32,
}

pub const TIMING: Timing = Timing {
    settle: Duration::from_millis(2500),
    settle_3: Duration::from_secs(5),
    off: Duration::from_millis(500),
    wait: Duration::from_secs(5),
    attempts: 3,
};

/// The writable `disable` control of the port a device is on, from its
/// location (`BUS-PORT.PORT…`, the sysfs device name). None on macOS, whose
/// locations are IOKit numbers, and where the control is not writable.
pub fn port_control(location: &str, sysfs: &Path) -> Option<PathBuf> {
    if location.starts_with("0x") || location.is_empty() || location.contains('/') {
        return None;
    }
    let control = sysfs.join(location).join("port").canonicalize().ok()?.join("disable");
    let writable = std::fs::OpenOptions::new().write(true).open(&control).is_ok();
    writable.then_some(control)
}

pub(crate) fn after_checkmate(log: &Path) -> Option<String> {
    let text = String::from_utf8_lossy(&std::fs::read(log).ok()?).into_owned();
    text.find(CHECKMATE_2).or_else(|| text.find(CHECKMATE_3)).map(|i| text[i..].to_owned())
}

/// The device came back after checkm8.
pub(crate) fn returned(log: &Path) -> bool {
    after_checkmate(log).is_some_and(|t| RETURNED.iter().any(|r| t.contains(r)))
}

/// Whether the wait for the device starts, and how long it is.
pub(crate) fn dropped(log: &Path, timing: Timing) -> Option<Duration> {
    let text = after_checkmate(log)?;
    if text.starts_with(CHECKMATE_3) {
        Some(timing.settle_3)
    } else {
        text.contains(DISCONNECTED).then_some(timing.settle)
    }
}

/// Waits up to `limit` for `condition`; false when stopped first.
fn await_until(stop: &AtomicBool, limit: Duration, condition: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if condition() {
            return true;
        }
        if stop.load(Ordering::Relaxed) || Instant::now() >= end {
            return condition();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Watches palera1n's log until stopped; resets the port when the device
/// dropped after checkm8 and did not come back. Returns the number of resets.
/// The port is always enabled again.
pub fn watch(log: &Path, control: &Path, stop: &AtomicBool, timing: Timing) -> u32 {
    let settle = loop {
        if stop.load(Ordering::Relaxed) {
            return 0;
        }
        if let Some(settle) = dropped(log, timing) {
            break settle;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if await_until(stop, settle, || returned(log)) {
        return 0;
    }
    let mut resets = 0;
    for _ in 0..timing.attempts {
        if stop.load(Ordering::Relaxed) || std::fs::write(control, "1").is_err() {
            return resets;
        }
        std::thread::sleep(timing.off);
        if std::fs::write(control, "0").is_err() {
            return resets;
        }
        resets += 1;
        if await_until(stop, timing.wait, || returned(log)) {
            return resets;
        }
    }
    resets
}

/// A watcher running alongside palera1n.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<u32>,
    /// Set when it stopped trying (the device back, or the resets used up).
    pub done: Arc<AtomicBool>,
}

impl Watcher {
    pub fn start(log: PathBuf, control: PathBuf) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (flag, finished) = (stop.clone(), done.clone());
        let thread = std::thread::spawn(move || {
            let resets = watch(&log, &control, &flag, TIMING);
            finished.store(true, Ordering::Relaxed);
            resets
        });
        Watcher { stop, thread, done }
    }

    /// Stops watching; the number of resets it made.
    pub fn finish(self) -> u32 {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Timing = Timing {
        settle: Duration::from_millis(200),
        settle_3: Duration::from_millis(200),
        off: Duration::from_millis(10),
        wait: Duration::from_millis(300),
        attempts: 3,
    };

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("orchard-replug-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_device_that_returns_by_itself_is_left_alone() {
        let d = dir("returns");
        let (log, control) = (d.join("log"), d.join("disable"));
        std::fs::write(&log, "Checkmate!\nDFU mode device disconnected\nEntered download mode\n").unwrap();
        std::fs::write(&control, "0").unwrap();
        assert_eq!(watch(&log, &control, &AtomicBool::new(false), FAST), 0);
    }

    #[test]
    fn a_device_that_stays_away_gets_its_port_reset_until_it_returns() {
        let d = dir("reset");
        let (log, control) = (d.join("log"), d.join("disable"));
        std::fs::write(&log, "Checkmate!\nDFU mode device disconnected\n").unwrap();
        std::fs::write(&control, "0").unwrap();
        let (l, c) = (log.clone(), control.clone());
        let t = std::thread::spawn(move || watch(&l, &c, &AtomicBool::new(false), FAST));
        std::thread::sleep(Duration::from_millis(350));
        std::fs::write(&log, "Checkmate!\nDFU mode device disconnected\nDownload mode device found\n").unwrap();
        assert_eq!(t.join().unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&control).unwrap(), "0", "the port is enabled again");
    }

    #[test]
    fn it_gives_up_after_three_resets() {
        let d = dir("giveup");
        let (log, control) = (d.join("log"), d.join("disable"));
        std::fs::write(&log, "Checkmate!\nDFU mode device disconnected\n").unwrap();
        std::fs::write(&control, "0").unwrap();
        assert_eq!(watch(&log, &control, &AtomicBool::new(false), FAST), 3);
    }

    #[test]
    fn palera1n_3_logs_are_followed_from_checkmate() {
        let d = dir("p3");
        let (log, control) = (d.join("log"), d.join("disable"));
        std::fs::write(&log, "Checkmate?\nFound download mode device\n").unwrap();
        std::fs::write(&control, "0").unwrap();
        assert_eq!(watch(&log, &control, &AtomicBool::new(false), FAST), 0);
        std::fs::write(&log, "Found DFU mode device\nCheckmate?\n").unwrap();
        assert_eq!(watch(&log, &control, &AtomicBool::new(false), FAST), 3);
    }

    #[test]
    fn macos_locations_have_no_control() {
        assert_eq!(port_control("0x14100000", Path::new("/sys/bus/usb/devices")), None);
        assert_eq!(port_control("../x", Path::new("/sys/bus/usb/devices")), None);
    }
}

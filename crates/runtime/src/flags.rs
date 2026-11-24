//! Switches from the environment, to try variants on a tester's computer
//! without a new build. Unset means the built-in behaviour; each one set is
//! logged when the installer or launcher starts.
//!
//! - `ORCHARD_PORT_RESET_SECONDS`: seconds after "Checkmate" before the USB
//!   port reset (Linux); 0 turns the reset off.
//! - `ORCHARD_PALERA1N`: a palera1n executable to run instead of the pinned one.
//! - `ORCHARD_KEEP_AWAKE=1`: keeps the CPU out of its deep sleep states while
//!   the exploit runs (Linux, `/dev/cpu_dma_latency`, which must be writable:
//!   root's by default), for hosts where checkm8 only succeeds under load.
use std::path::PathBuf;
use std::time::Duration;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// None: the built-in wait. Some(zero): no port reset.
pub fn port_reset() -> Option<Duration> {
    var("ORCHARD_PORT_RESET_SECONDS")?.parse::<f64>().ok().filter(|s| *s >= 0.0).map(Duration::from_secs_f64)
}

pub fn palera1n() -> Option<PathBuf> {
    var("ORCHARD_PALERA1N").map(PathBuf::from)
}

pub fn keep_awake() -> bool {
    var("ORCHARD_KEEP_AWAKE").is_some_and(|v| v != "0")
}

/// While held, the CPU stays out of deep sleep states (Linux PM QoS: a 0 µs
/// latency request lasts as long as the file stays open). None, with the
/// reason logged, where that isn't possible.
pub struct Awake(#[allow(dead_code)] std::fs::File);

pub fn stay_awake() -> Option<Awake> {
    if !keep_awake() {
        return None;
    }
    if !cfg!(target_os = "linux") {
        eprintln!("flags: ORCHARD_KEEP_AWAKE does nothing on this system");
        return None;
    }
    use std::io::Write;
    let opened = std::fs::OpenOptions::new().write(true).open("/dev/cpu_dma_latency");
    match opened.and_then(|mut f| f.write_all(&0i32.to_ne_bytes()).map(|_| f)) {
        Ok(file) => {
            eprintln!("flags: keeping the CPU awake while the exploit runs");
            Some(Awake(file))
        }
        Err(e) => {
            eprintln!("flags: ORCHARD_KEEP_AWAKE: /dev/cpu_dma_latency: {e} (sudo chmod o+w /dev/cpu_dma_latency allows it until a restart)");
            None
        }
    }
}

/// One line per switch that is set (and understood), for the log.
pub fn log_active() {
    for name in ["ORCHARD_PORT_RESET_SECONDS", "ORCHARD_PALERA1N", "ORCHARD_KEEP_AWAKE"] {
        if let Some(value) = var(name) {
            let understood = name != "ORCHARD_PORT_RESET_SECONDS" || port_reset().is_some();
            eprintln!("flags: {name}={value}{}", if understood { "" } else { " (not understood, ignored)" });
        }
    }
}

//! Switches from the environment, to try variants on a tester's computer
//! without a new build. Unset means the built-in behaviour; each one set is
//! logged when the installer or launcher starts.
//!
//! - `ORCHARD_PORT_RESET_SECONDS`: seconds after "Checkmate" before the USB
//!   port reset (Linux); 0 turns the reset off.
//! - `ORCHARD_PALERA1N`: a palera1n executable to run instead of the pinned one.
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

/// One line per switch that is set (and understood), for the log.
pub fn log_active() {
    for name in ["ORCHARD_PORT_RESET_SECONDS", "ORCHARD_PALERA1N"] {
        if let Some(value) = var(name) {
            let understood = name != "ORCHARD_PORT_RESET_SECONDS" || port_reset().is_some();
            eprintln!("flags: {name}={value}{}", if understood { "" } else { " (not understood, ignored)" });
        }
    }
}

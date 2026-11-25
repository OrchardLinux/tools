//! Switches from the environment, to try variants on a tester's computer
//! without a new build. Unset means the built-in behaviour; each one set is
//! logged when the installer or launcher starts.
//!
//! - `ORCHARD_PORT_RESET_SECONDS`: seconds after "Checkmate" before the USB
//!   port reset (Linux); 0 turns the reset off.
//! - `ORCHARD_PALERA1N_BETA=0`: runs palera1n v2.4 (pinned, checked like the
//!   beta) instead of the 3.0 beta.
//! - `ORCHARD_ENTRY=palera1n` or `checkra1n`: the tool that runs the exploit
//!   and starts Pongo, instead of the one the board's data names. What
//!   follows from Pongo is the same.
//! - `ORCHARD_KEEP_AWAKE=1`: keeps the CPU out of its deep sleep states while
//!   the exploit runs (Linux, `/dev/cpu_dma_latency`, which must be writable:
//!   root's by default), for hosts where checkm8 only succeeds under load.
use std::time::Duration;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// None: the built-in wait. Some(zero): no port reset.
pub fn port_reset() -> Option<Duration> {
    var("ORCHARD_PORT_RESET_SECONDS")?.parse::<f64>().ok().filter(|s| *s >= 0.0).map(Duration::from_secs_f64)
}

/// The palera1n to run on this computer: the 3.0 beta, or v2.4 with
/// ORCHARD_PALERA1N_BETA=0.
pub fn palera1n_pin() -> Option<(&'static orchard_flow::assets::Pin, &'static str)> {
    use orchard_flow::assets::{palera1n_2_pin, palera1n_pin, PALERA1N_2_RELEASE, PALERA1N_RELEASE};
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    if var("ORCHARD_PALERA1N_BETA").as_deref() == Some("0") {
        palera1n_2_pin(os, arch).map(|p| (p, PALERA1N_2_RELEASE))
    } else {
        palera1n_pin(os, arch).map(|p| (p, PALERA1N_RELEASE))
    }
}

/// The tool that takes a device from DFU mode to Pongo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Palera1n,
    /// The unpatched official checkra1n: for the boards palera1n does not
    /// list (A7), and those that start better with it.
    Checkra1n,
}

impl Entry {
    /// The entry a name stands for. The names are closed here: the board
    /// data says which of them a board prefers, never what is run.
    fn named(name: &str) -> Option<Entry> {
        match name {
            "palera1n" => Some(Entry::Palera1n),
            "checkra1n" => Some(Entry::Checkra1n),
            _ => None,
        }
    }
}

/// The entry for a board: ORCHARD_ENTRY when it names one, else the first
/// the board's data prefers (`[boot] entries`) that this tool has, else
/// palera1n.
pub fn entry(preferred: &[String]) -> Entry {
    entry_from(var("ORCHARD_ENTRY").as_deref(), preferred)
}

fn entry_from(asked: Option<&str>, preferred: &[String]) -> Entry {
    asked
        .and_then(Entry::named)
        .or_else(|| preferred.iter().find_map(|name| Entry::named(name)))
        .unwrap_or(Entry::Palera1n)
}

/// The checkra1n to run on this computer.
pub fn checkra1n_pin() -> Option<(&'static orchard_flow::assets::Pin, &'static str)> {
    use orchard_flow::assets::{checkra1n_pin, CHECKRA1N_RELEASE};
    checkra1n_pin(std::env::consts::OS, std::env::consts::ARCH).map(|p| (p, CHECKRA1N_RELEASE))
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
    for name in ["ORCHARD_PORT_RESET_SECONDS", "ORCHARD_PALERA1N_BETA", "ORCHARD_ENTRY", "ORCHARD_KEEP_AWAKE"] {
        if let Some(value) = var(name) {
            let understood = match name {
                "ORCHARD_PORT_RESET_SECONDS" => port_reset().is_some(),
                "ORCHARD_ENTRY" => Entry::named(&value).is_some(),
                _ => true,
            };
            eprintln!("flags: {name}={value}{}", if understood { "" } else { " (not understood, ignored)" });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_names_its_entry_and_the_environment_goes_first() {
        let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert_eq!(entry_from(None, &[]), Entry::Palera1n);
        assert_eq!(entry_from(None, &names(&["checkra1n"])), Entry::Checkra1n);
        // A name this tool has no entry for is passed over, never run.
        assert_eq!(entry_from(None, &names(&["pico-checkm8", "kdfu"])), Entry::Palera1n);
        assert_eq!(entry_from(None, &names(&["kdfu", "checkra1n", "palera1n"])), Entry::Checkra1n);
        assert_eq!(entry_from(Some("palera1n"), &names(&["checkra1n"])), Entry::Palera1n);
        assert_eq!(entry_from(Some("checkra1n"), &[]), Entry::Checkra1n);
        assert_eq!(entry_from(Some("1"), &names(&["checkra1n"])), Entry::Checkra1n);
    }
}

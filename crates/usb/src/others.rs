//! Other Apple devices on USB besides the one being worked on, in any mode.
//!
//! One device shows up as one USB device at a time, whatever it runs (iOS,
//! recovery, DFU, Pongo, m1n1, an Orchard system), so two at once are two
//! devices. A lone device is the target when its ECID says so; without an
//! ECID (iOS before A12, whose UDID is a hash; m1n1; an installed system)
//! when it sits at the target's USB port, which stays the same while the
//! device changes modes on the same cable.
use crate::{Device, Mode};

/// The device being worked on, as far as it is known.
#[derive(Debug, Clone, Copy, Default)]
pub struct Target<'a> {
    pub ecid: Option<&'a str>,
    /// Where it was last seen.
    pub location: Option<&'a str>,
}

/// An ECID in one spelling: lowercase hex without leading zeros.
pub fn ecid_key(ecid: &str) -> String {
    let trimmed = ecid.trim().trim_start_matches("0x").trim_start_matches('0').to_ascii_lowercase();
    if trimmed.is_empty() { "0".into() } else { trimmed }
}

/// Whether this listed device is an iPhone, iPad or iPod (or one of
/// Orchard's systems on one), not some other USB gadget.
pub fn apple_device(device: &Device) -> bool {
    !(device.mode == Mode::Linux && device.system.is_none())
}

/// The listed Apple devices that aren't `target`. With no target yet, every
/// one of them when there are several.
pub fn others<'a>(listing: &'a [Device], target: Option<Target>) -> Vec<&'a Device> {
    let devices: Vec<&Device> = listing.iter().filter(|d| apple_device(d)).collect();
    let Some(target) = target else {
        return if devices.len() > 1 { devices } else { Vec::new() };
    };
    let lone = devices.len() == 1;
    let is_target = |d: &Device| match (d.identity.ecid.as_deref(), target.ecid) {
        (Some(a), Some(b)) => ecid_key(a) == ecid_key(b),
        _ => match target.location {
            Some(at) => d.location == at,
            // Nothing to tell by: a lone device is taken to be the target.
            None => lone,
        },
    };
    let mut out: Vec<&Device> = devices.iter().copied().filter(|d| !is_target(d)).collect();
    // Two that both pass for the target (no ECID, same port can't happen
    // twice): all but one are others.
    let matching = devices.len() - out.len();
    if matching > 1 {
        out.extend(devices.iter().copied().filter(|d| is_target(d)).skip(1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, System};

    fn device(mode: Mode, location: &str, ecid: Option<&str>) -> Device {
        Device {
            mode,
            system: None,
            location: location.into(),
            product: None,
            identity: Identity { ecid: ecid.map(str::to_owned), ..Identity::default() },
        }
    }

    const ECID: &str = "000011223344556677";
    fn target() -> Option<Target<'static>> {
        Some(Target { ecid: Some("11223344556677"), location: Some("0x1100000") })
    }

    #[test]
    fn the_target_in_each_of_its_modes_is_alone() {
        for mode in [Mode::Dfu, Mode::Recovery, Mode::Pongo] {
            assert!(others(&[device(mode, "0x1100000", Some(ECID))], target()).is_empty());
        }
        // No ECID (iOS before A12, m1n1): known by its port.
        for mode in [Mode::Normal, Mode::M1n1] {
            assert!(others(&[device(mode, "0x1100000", None)], target()).is_empty());
        }
        let mut install = device(Mode::Linux, "0x1100000", Some("11223344556677"));
        install.system = Some(System::Install);
        assert!(others(&[install], target()).is_empty());
        // Plugged into another port in DFU: its ECID still says it's the one.
        assert!(others(&[device(Mode::Dfu, "0x1400000", Some(ECID))], target()).is_empty());
        // Gone for a moment between modes: nothing else either.
        assert!(others(&[], target()).is_empty());
    }

    #[test]
    fn a_second_device_with_a_hash_udid() {
        // The target in DFU, an iPhone running iOS beside it.
        let listing = [device(Mode::Dfu, "0x1100000", Some(ECID)), device(Mode::Normal, "0x1400000", None)];
        assert_eq!(others(&listing, target()).len(), 1);
        // The target off USB after checkm8, the iPhone alone: not a restart.
        let alone = [device(Mode::Normal, "0x1400000", None)];
        assert_eq!(others(&alone, target()).len(), 1);
    }

    #[test]
    fn a_second_device_with_an_ecid_udid() {
        let alone = [device(Mode::Normal, "0x1100000", Some("99887766554433"))];
        assert_eq!(others(&alone, target()).len(), 1);
    }

    #[test]
    fn before_a_device_is_chosen_two_are_too_many() {
        assert!(others(&[device(Mode::Normal, "a", None)], None).is_empty());
        let two = [device(Mode::Dfu, "a", Some(ECID)), device(Mode::Normal, "b", None)];
        assert_eq!(others(&two, None).len(), 2);
    }

    #[test]
    fn stock_linux_gadgets_dont_count() {
        let listing = [device(Mode::Dfu, "0x1100000", Some(ECID)), device(Mode::Linux, "0x1400000", None)];
        assert!(others(&listing, target()).is_empty());
    }

    #[test]
    fn without_a_known_port_a_lone_device_is_the_target() {
        let t = Some(Target { ecid: Some("11223344556677"), location: None });
        assert!(others(&[device(Mode::Normal, "x", None)], t).is_empty());
        let two = [device(Mode::Normal, "x", None), device(Mode::M1n1, "y", None)];
        assert_eq!(others(&two, t).len(), 2);
    }
}

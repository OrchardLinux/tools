//! Picking exactly one device from what is connected.
use crate::deadline::Deadline;
use crate::usb;
use orchard_cache::{Code, Error, Result};
use orchard_usb::{Device, Discovery, Mode};
use std::time::Duration;

/// What the user asked for. Empty fields match anything.
#[derive(Debug, Clone, Default)]
pub struct Selector {
    pub ecid: Option<String>,
    pub location: Option<String>,
    pub mode: Option<Mode>,
}

impl Selector {
    pub fn matches(&self, d: &Device) -> bool {
        let ecid = self.ecid.as_deref().map(normalize_ecid);
        ecid.is_none_or(|e| d.identity.ecid.as_deref().map(normalize_ecid) == Some(e))
            && self.location.as_ref().is_none_or(|l| *l == d.location)
            && self.mode.is_none_or(|m| m == d.mode)
    }
}

/// All devices that match.
pub fn matching(discovery: &mut impl Discovery, selector: &Selector) -> Result<Vec<Device>> {
    let devices = discovery.list().map_err(usb::error)?;
    Ok(devices
        .into_iter()
        .filter(|d| selector.matches(d))
        .collect())
}

/// The one device that matches, or an error that says what to narrow down.
pub fn one(discovery: &mut impl Discovery, selector: &Selector) -> Result<Device> {
    let mut found = matching(discovery, selector)?;
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(Error::new(Code::NoDevice, "no matching device connected")),
        n => Err(Error::new(Code::Ambiguous, format!("{n} devices match"))
            .hint("choose one with --ecid or --location")),
    }
}

/// The selector for a command that starts a system on a device: without a
/// device flag, a device that already runs Linux is not what it means.
/// When exactly one other device matches, the selector is narrowed to that
/// one; otherwise it stays as it is, and choosing the device says what is
/// ambiguous or missing.
pub fn to_start(discovery: &mut impl Discovery, selector: &Selector) -> Result<Selector> {
    if selector.ecid.is_some() || selector.location.is_some() {
        return Ok(selector.clone());
    }
    let found = matching(discovery, selector)?;
    let mut waiting = found.iter().filter(|d| d.mode != Mode::Linux);
    match (waiting.next(), waiting.next()) {
        (Some(only), None) if found.len() > 1 => Ok(Selector {
            location: Some(only.location.clone()),
            ..selector.clone()
        }),
        _ => Ok(selector.clone()),
    }
}

/// How often the connected devices are looked at while waiting.
pub(crate) const POLL: Duration = Duration::from_millis(50);

/// Waits until exactly one device matches. Several matching devices are an
/// error at once: waiting would not tell them apart.
pub fn wait_one(
    discovery: &mut impl Discovery,
    selector: &Selector,
    deadline: &Deadline,
) -> Result<Device> {
    loop {
        match one(discovery, selector) {
            Err(e) if e.code == Code::NoDevice => {}
            found => return found,
        }
        deadline
            .sleep(POLL)
            .map_err(|_| Error::new(Code::Timeout, "no matching device showed up in time"))?;
    }
}

/// An ECID as 16 lowercase hex digits, whatever spelling it came in.
pub(crate) fn normalize_ecid(ecid: &str) -> String {
    let hex = ecid.trim_start_matches("0x").to_ascii_lowercase();
    format!("{:0>16}", hex.trim_start_matches('0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_usb::Identity;

    struct Fake(Vec<Device>);

    impl Discovery for Fake {
        fn list(&mut self) -> std::result::Result<Vec<Device>, orchard_usb::Error> {
            Ok(self.0.clone())
        }
    }

    fn device(location: &str, mode: Mode, ecid: &str) -> Device {
        Device {
            mode,
            system: None,
            location: location.into(),
            product: None,
            identity: Identity {
                ecid: Some(ecid.into()),
                ..Identity::default()
            },
        }
    }

    fn two() -> Fake {
        Fake(vec![
            device("0x100000", Mode::Dfu, "0000000000000001"),
            device("0x200000", Mode::Recovery, "00000000000000ab"),
        ])
    }

    #[test]
    fn two_devices_need_a_selector() {
        let e = one(&mut two(), &Selector::default()).unwrap_err();
        assert_eq!(e.code, Code::Ambiguous);
    }

    #[test]
    fn ecid_matches_in_any_spelling() {
        let s = Selector {
            ecid: Some("0xAB".into()),
            ..Selector::default()
        };
        assert_eq!(one(&mut two(), &s).unwrap().location, "0x200000");
    }

    #[test]
    fn waiting_returns_the_device_once_it_is_there() {
        /// Empty for the first calls, then the two devices.
        struct Late(u32);
        impl Discovery for Late {
            fn list(&mut self) -> std::result::Result<Vec<Device>, orchard_usb::Error> {
                self.0 = self.0.saturating_sub(1);
                Ok(if self.0 > 0 { vec![] } else { two().0 })
            }
        }
        let recovery = Selector {
            mode: Some(Mode::Recovery),
            ..Selector::default()
        };
        let long = Deadline::after(Duration::from_secs(5));
        let found = wait_one(&mut Late(3), &recovery, &long).unwrap();
        assert_eq!(found.location, "0x200000");

        // Two matches do not wait for the deadline.
        let e = wait_one(&mut two(), &Selector::default(), &long).unwrap_err();
        assert_eq!(e.code, Code::Ambiguous);

        let short = Deadline::after(Duration::from_millis(120));
        let e = wait_one(&mut Late(u32::MAX), &recovery, &short).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
    }

    #[test]
    fn a_device_that_runs_linux_is_not_one_to_start() {
        let linux = device("0x300000", Mode::Linux, "00000000000000c3");
        // One device waits in DFU mode, another already runs Linux.
        let dfu = device("0x100000", Mode::Dfu, "0000000000000001");
        let mut both = Fake(vec![linux.clone(), dfu]);
        let chosen = to_start(&mut both, &Selector::default()).unwrap();
        assert_eq!(one(&mut both, &chosen).unwrap().location, "0x100000");
        // The same with one in m1n1.
        let m1n1 = device("0x200000", Mode::M1n1, "00000000000000ab");
        let mut both = Fake(vec![m1n1, linux.clone()]);
        let chosen = to_start(&mut both, &Selector::default()).unwrap();
        assert_eq!(one(&mut both, &chosen).unwrap().mode, Mode::M1n1);

        // Two that wait are still two to choose from.
        let mut three = Fake(vec![two().0[0].clone(), two().0[1].clone(), linux.clone()]);
        let chosen = to_start(&mut three, &Selector::default()).unwrap();
        assert_eq!(one(&mut three, &chosen).unwrap_err().code, Code::Ambiguous);
        // A device that is named is the one meant, Linux or not.
        let named = Selector {
            location: Some("0x300000".into()),
            ..Selector::default()
        };
        let chosen = to_start(&mut three, &named).unwrap();
        assert_eq!(one(&mut three, &chosen).unwrap().mode, Mode::Linux);
        // The only device connected is looked at as it is, so the command
        // can say what it found.
        let mut alone = Fake(vec![linux]);
        let chosen = to_start(&mut alone, &Selector::default()).unwrap();
        assert_eq!(one(&mut alone, &chosen).unwrap().mode, Mode::Linux);
    }

    #[test]
    fn mode_narrows() {
        let s = Selector {
            mode: Some(Mode::Dfu),
            ..Selector::default()
        };
        assert_eq!(one(&mut two(), &s).unwrap().location, "0x100000");
    }
}

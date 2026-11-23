use serde::Serialize;

const APPLE_VID: u16 = 0x05ac;
const M1N1_VID: u16 = 0x1209;
const M1N1_PID: u16 = 0x316d;
const LINUX_FOUNDATION_VID: u16 = 0x1d6b;
const NETCHIP_VID: u16 = 0x0525;

/// What a device is running, as told by its USB ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dfu,
    /// iOS or iPadOS is running.
    Normal,
    Recovery,
    Pongo,
    M1n1,
    Linux,
}

impl Mode {
    /// The name used in output, the same as in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Dfu => "dfu",
            Mode::Normal => "normal",
            Mode::Recovery => "recovery",
            Mode::Pongo => "pongo",
            Mode::M1n1 => "m1n1",
            Mode::Linux => "linux",
        }
    }

    pub fn from_ids(vendor: u16, product: u16) -> Option<Mode> {
        classify(vendor, product).map(|(mode, _)| mode)
    }
}

/// Which of Orchard's own Linux systems a device runs. Each announces
/// itself with its own product id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum System {
    /// Orchard Recovery.
    Recovery,
    /// A system started in RAM, in its USB session.
    Ram,
    /// A system installed on the device.
    Installed,
    /// The installer.
    Install,
}

impl System {
    /// The name used in output, the same as in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            System::Recovery => "recovery",
            System::Ram => "ram",
            System::Installed => "installed",
            System::Install => "install",
        }
    }
}

/// What a device with these USB ids is running. A Linux system is known
/// either by one of Orchard's ids, which also tell which system it is, or by
/// one of the ids the kernel's stock USB gadgets use.
/// Like [`classify`], with the serial: installed systems now present
/// 0x05ac:0x1014 (macOS grants that ID 2400 mA), which real Apple hardware uses
/// too, so it counts as an installed Orchard system only with an Orchard serial
/// (orchard's docs/device-protocol.md, "USB identity").
pub fn classify_with_serial(vendor: u16, product: u16, serial: Option<&str>) -> Option<(Mode, Option<System>)> {
    if (vendor, product) == (APPLE_VID, 0x1014) {
        return serial.filter(|s| orchard_serial(s)).map(|_| (Mode::Linux, Some(System::Installed)));
    }
    classify(vendor, product)
}

/// An Orchard system's serial: `BOARD:<profile>` with `ID:<12 hex>`, or with
/// the legacy `BOOT:<uuid>`.
pub fn orchard_serial(serial: &str) -> bool {
    let mut board = false;
    let mut id = false;
    for field in serial.split_whitespace() {
        match field.split_once(':') {
            Some(("BOARD", v)) if !v.is_empty() => board = true,
            Some(("ID", v)) if v.len() == 12 && v.bytes().all(|b| b.is_ascii_hexdigit()) => id = true,
            Some(("BOOT", v)) if v.len() == 36 && v.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') => id = true,
            _ => {}
        }
    }
    board && id
}

pub fn classify(vendor: u16, product: u16) -> Option<(Mode, Option<System>)> {
    let orchard = |system| Some((Mode::Linux, Some(system)));
    match (vendor, product) {
        (APPLE_VID, 0x1227) => Some((Mode::Dfu, None)),
        (APPLE_VID, 0x1281) => Some((Mode::Recovery, None)),
        // iPhone, iPad and iPod touch running iOS or iPadOS.
        (APPLE_VID, 0x1290..=0x12af) => Some((Mode::Normal, None)),
        (APPLE_VID, 0x4141) => Some((Mode::Pongo, None)),
        (M1N1_VID, M1N1_PID) => Some((Mode::M1n1, None)),
        (APPLE_VID, 0x4142) => orchard(System::Recovery),
        (APPLE_VID, 0x4144) => orchard(System::Ram),
        (APPLE_VID, 0x4145) => orchard(System::Installed),
        (APPLE_VID, 0x4146) => orchard(System::Install),
        // The Linux Foundation's gadget ids (0x0001 to 0x0003 are the
        // host's own root hubs) and NetChip's, which the older gadgets use.
        (LINUX_FOUNDATION_VID, 0x0101..=0x0106) => Some((Mode::Linux, None)),
        (NETCHIP_VID, 0xa4a0..=0xa4ac) => Some((Mode::Linux, None)),
        _ => None,
    }
}

/// Fields from the boot ROM or iBoot serial string, e.g.
/// `CPID:8010 BDID:0C ECID:0011223344556677 SRTG:[iBoot-2696.0.0.1.33]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Identity {
    pub chip: Option<String>,
    /// The board number (BDID), which tells boards with the same chip apart.
    pub board_id: Option<String>,
    pub ecid: Option<String>,
    pub iboot: Option<String>,
    /// The marker a boot ROM exploit leaves (`PWND`), which names the tool.
    pub pwned: Option<String>,
    /// The device's serial number (`SRNM`), as it is written.
    pub serial_number: Option<String>,
    /// The UDID iOS or iPadOS reports as its USB serial number.
    pub udid: Option<String>,
    /// The board an Orchard system names in its serial (`BOARD:j71b`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
}

impl Identity {
    /// From the serial number of a device in normal mode: its UDID. A12 and
    /// later chips' UDIDs are `CCCCCCCC-EEEEEEEEEEEEEEEE`, chip and ECID;
    /// older ones are a hash and tell nothing more.
    pub fn from_udid(serial: &str) -> Identity {
        let udid = serial.trim().to_ascii_lowercase();
        let mut id = Identity::default();
        if !(udid.len() == 25 || udid.len() == 40) || !udid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return id;
        }
        if let Some((chip, ecid)) = udid.split_once('-') {
            if chip.len() == 8 && ecid.len() == 16 {
                id.chip = Some(chip.trim_start_matches('0').to_owned());
                id.ecid = Some(ecid.to_owned());
            }
        }
        id.udid = Some(udid);
        id
    }

    pub fn parse(serial: &str) -> Identity {
        let mut id = Identity::default();
        for field in serial.split_whitespace() {
            let Some((key, value)) = field.split_once(':') else {
                continue;
            };
            let value = value.trim_matches(|c| c == '[' || c == ']');
            if key == "SRNM" {
                let plain = value.bytes().all(|b| b.is_ascii_alphanumeric());
                if plain && (1..=32).contains(&value.len()) {
                    id.serial_number = Some(value.to_owned());
                }
                continue;
            }
            let value = value.to_ascii_lowercase();
            match key {
                "CPID" => id.chip = Some(value),
                "BDID" => id.board_id = Some(value),
                "ECID" => id.ecid = Some(value),
                "SRTG" => id.iboot = Some(value),
                "PWND" => id.pwned = Some(value),
                "BOARD" => id.board = Some(value),
                _ => {}
            }
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_mode_udids() {
        assert_eq!(classify(0x05ac, 0x12ab), Some((Mode::Normal, None)));
        let new = Identity::from_udid("00008020-001A2B3C4D5E6F7A");
        assert_eq!((new.chip.as_deref(), new.ecid.as_deref()), (Some("8020"), Some("001a2b3c4d5e6f7a")));
        let old = Identity::from_udid("0123456789abcdef0123456789abcdef01234567");
        assert_eq!((old.ecid, old.udid.as_deref()), (None, Some("0123456789abcdef0123456789abcdef01234567")));
        assert_eq!(Identity::from_udid("not a udid").udid, None);
    }

    #[test]
    fn modes() {
        assert_eq!(Mode::from_ids(0x05ac, 0x1227), Some(Mode::Dfu));
        assert_eq!(Mode::from_ids(0x1209, 0x316d), Some(Mode::M1n1));
        assert_eq!(Mode::from_ids(0x05ac, 0x12a8), Some(Mode::Normal));
        assert_eq!(Mode::from_ids(0x05ac, 0x12b0), None);
    }

    #[test]
    fn installed_systems_on_the_charging_id_need_an_orchard_serial() {
        // As orchard's tests/test_identity.py.
        let installed = Some((Mode::Linux, Some(System::Installed)));
        assert_eq!(classify_with_serial(0x05ac, 0x1014, Some("BOARD:j71b ID:0123456789ab")), installed);
        assert_eq!(classify_with_serial(0x05ac, 0x1014, Some("BOOT:11111111-1111-4111-8111-111111111111 BOARD:j71b")), installed);
        assert_eq!(classify_with_serial(0x05ac, 0x1014, Some("DWH12345678")), None);
        assert_eq!(classify_with_serial(0x05ac, 0x1014, Some("BOARD:j71b")), None);
        assert_eq!(classify_with_serial(0x05ac, 0x1014, None), None);
        assert_eq!(classify_with_serial(0x05ac, 0x4145, Some("BOARD:j71b ID:0123456789ab")), installed);
    }

    #[test]
    fn linux_systems() {
        assert_eq!(
            classify(0x05ac, 0x4142),
            Some((Mode::Linux, Some(System::Recovery)))
        );
        assert_eq!(
            classify(0x05ac, 0x4145),
            Some((Mode::Linux, Some(System::Installed)))
        );
        // Stock gadgets are Linux without a system; a root hub is not.
        assert_eq!(classify(0x1d6b, 0x0104), Some((Mode::Linux, None)));
        assert_eq!(classify(0x0525, 0xa4a7), Some((Mode::Linux, None)));
        assert_eq!(classify(0x1d6b, 0x0002), None);
        for s in [
            System::Recovery,
            System::Ram,
            System::Installed,
            System::Install,
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
    }

    #[test]
    fn output_name_matches_json() {
        for m in [
            Mode::Dfu,
            Mode::Recovery,
            Mode::Pongo,
            Mode::M1n1,
            Mode::Linux,
        ] {
            assert_eq!(serde_json::to_value(m).unwrap(), m.as_str());
        }
    }

    #[test]
    fn serial_string() {
        let id = Identity::parse(
            "CPID:8010 CPRV:11 BDID:0C ECID:0011223344556677 SRTG:[iBoot-2696.0.0.1.33]",
        );
        assert_eq!(id.chip.as_deref(), Some("8010"));
        assert_eq!(id.board_id.as_deref(), Some("0c"));
        assert_eq!(id.ecid.as_deref(), Some("0011223344556677"));
        assert_eq!(id.iboot.as_deref(), Some("iboot-2696.0.0.1.33"));
        assert_eq!(id.pwned, None);
        assert_eq!(id.serial_number, None);
        let pwned = Identity::parse("CPID:8942 BDID:00 SRTG:[iBoot-1145.3] PWND:[checkm8]");
        assert_eq!(pwned.pwned.as_deref(), Some("checkm8"));
        // The serial number keeps its spelling; one that is not plain
        // letters and digits is not taken.
        let named = Identity::parse("CPID:8942 SRNM:[F4KxY12ZAB] ECID:00000000000000B2");
        assert_eq!(named.serial_number.as_deref(), Some("F4KxY12ZAB"));
        assert_eq!(Identity::parse("SRNM:[a/b]").serial_number, None);
        assert_eq!(Identity::parse("SRNM:[]").serial_number, None);
    }
}

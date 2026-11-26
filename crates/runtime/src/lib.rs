//! Runs the installer's flow against the real world.
//!
//! So far: USB discovery turned into [`orchard_flow::Event::Attached`] and
//! [`orchard_flow::Event::Detached`]. Discovery only lists devices; nothing is
//! opened or sent. Effects are carried out by executors added one at a time.
pub mod chain;
pub mod companion;
pub mod demo;
pub mod driver;
pub mod exec;
pub mod flags;
pub mod install;
pub use orchard_boards::kernel_package;
pub mod launcher;
pub mod stamp;
pub mod orchard_tools;
pub mod ssh_keys;
pub mod timezone;
pub mod session_log;
pub mod setup_launcher;
pub mod lockdown;
pub mod overlay;
pub mod records;
pub mod usb_access;
mod held;
pub mod usbmux;
pub mod replug;
pub use orchard_boards::release;
pub mod tools;

use orchard_boards::catalog::{Board, Catalog};
use orchard_flow::{Device, DeviceId, Event, Model, Sequence, Support, UsbMode};
use std::collections::BTreeMap;

/// What the flow needs to know about a board, from the catalog.
pub fn describe(catalog: &Catalog, board: &Board, ecid: &str) -> Device {
    // Every start here goes through palera1n, which reaches 64-bit chips
    // only: a 32-bit board is refused before anything is started on it.
    let reachable = catalog.chip(board.chip).map_or(true, |chip| chip.architecture != "arm");
    let product = board.products.first().map(String::as_str).unwrap_or("");
    let (model, sequence) = layout(product);
    Device {
        id: DeviceId(ecid.to_owned()),
        board: board.name.clone(),
        name: board.display_name.clone(),
        model,
        sequence,
        support: match board.status.as_str() {
            _ if !reachable => Support::Unsupported,
            "supported" => Support::Supported,
            "experimental" => Support::Experimental,
            _ => Support::Unsupported,
        },
    }
}

/// The drawing and DFU sequence for an Apple product type, by its family and
/// generation. iPads that palera1n reaches all have a Home button; iPhone 7
/// (iPhone9,x) uses Volume Down + Side; iPhone 8 and X (iPhone10,x) the
/// volume sequence, and the X has no Home button.
pub fn layout(product: &str) -> (Model, Sequence) {
    let generation = |prefix: &str| {
        product.strip_prefix(prefix)?.split(',').next()?.parse::<u32>().ok()
    };
    if let Some(g) = generation("iPhone") {
        return match (g, product) {
            (_, "iPhone10,3" | "iPhone10,6") => (Model::IphoneNoHome, Sequence::VolumeSequence),
            (10.., _) => (Model::IphoneHome, Sequence::VolumeSequence),
            (9, _) => (Model::IphoneHome, Sequence::VolumeDownSide),
            _ => (Model::IphoneHome, Sequence::HomePower),
        };
    }
    (Model::IpadHome, Sequence::HomePower)
}

/// The one device a run is for, named by the person: its ECID, the USB port
/// it is at, or both. Every other device is one too many.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Only {
    pub ecid: Option<String>,
    pub location: Option<String>,
}

impl Only {
    /// Whether a listed device is the named one; `ecid` is what is known of
    /// its ECID. A device that has not told its ECID yet is not it yet.
    pub fn is(&self, usb: &orchard_usb::Device, ecid: Option<&str>) -> bool {
        let at = self.location.as_deref().is_none_or(|location| usb.location == location);
        let named = match self.ecid.as_deref() {
            Some(wanted) => ecid.is_some_and(|known| ecid_key(known) == ecid_key(wanted)),
            None => true,
        };
        at && named
    }
}

/// A device as discovery saw it, before the flow hears about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub device: Device,
    pub mode: UsbMode,
}

/// Identifies a listed USB device: its mode and the board its chip and board
/// numbers name. Modes the flow does not track yet, and devices without an
/// ECID, are left out.
pub fn identify(catalog: &Catalog, usb: &orchard_usb::Device) -> Option<Seen> {
    let mode = match usb.mode {
        orchard_usb::Mode::Dfu => UsbMode::Dfu,
        orchard_usb::Mode::Recovery => UsbMode::Recovery,
        _ => return None,
    };
    let id = &usb.identity;
    let ecid = &ecid_key(id.ecid.as_deref()?);
    let chip = u32::from_str_radix(id.chip.as_deref()?, 16).ok()?;
    let board = u32::from_str_radix(id.board_id.as_deref()?, 16).ok()?;
    let device = match catalog.board_for(chip, board) {
        Ok(b) => describe(catalog, &b, ecid),
        Err(_) => Device {
            id: DeviceId(ecid.to_owned()),
            board: String::new(),
            name: format!("Unknown device (chip {chip:#x}, board {board:#x})"),
            model: Model::IpadHome,
            sequence: Sequence::HomePower,
            support: Support::Unsupported,
        },
    };
    Some(Seen { device, mode })
}

/// The ECID as the device's id: lower-case hex without leading zeros, so the
/// boot ROM's padded form and the install system's plain one are the same.
pub fn ecid_key(ecid: &str) -> String {
    orchard_usb::ecid_key(ecid)
}

/// The mode of one of Orchard's own systems, for a device already known by
/// its ECID from an earlier DFU sighting.
fn orchard_mode(usb: &orchard_usb::Device) -> Option<UsbMode> {
    match (usb.mode, usb.system) {
        (orchard_usb::Mode::Linux, Some(orchard_usb::System::Install)) => Some(UsbMode::Install),
        (orchard_usb::Mode::Linux, Some(orchard_usb::System::Installed)) => Some(UsbMode::Installed),
        _ => None,
    }
}

/// Turns successive USB listings into attach and detach events.
#[derive(Debug, Default)]
pub struct Watcher {
    present: BTreeMap<DeviceId, Seen>,
    /// Devices seen in DFU or recovery, to recognise them by ECID once they
    /// run the install system or the installed one, which report no chip.
    known: BTreeMap<DeviceId, Device>,
    /// The raw USB device of each install system present, to open its receiver.
    install: BTreeMap<DeviceId, orchard_usb::Device>,
    /// The raw USB device of each device running iPadOS, for its UDID.
    normal: BTreeMap<DeviceId, orchard_usb::Device>,
    /// What devices running iPadOS said about themselves (lockdown), by UDID.
    told: BTreeMap<String, lockdown::Answers>,
    /// Where each device was last seen: its USB port, which stays while it
    /// changes modes on the same cable.
    locations: BTreeMap<DeviceId, String>,
}

impl Watcher {
    /// The install system's USB device for `id`, while it is present.
    pub fn install_system(&self, id: &DeviceId) -> Option<&orchard_usb::Device> {
        self.install.get(id)
    }

    /// The USB device of `id` while it runs iPadOS: its UDID is what usbmux
    /// addresses it by.
    pub fn normal_device(&self, id: &DeviceId) -> Option<&orchard_usb::Device> {
        self.normal.get(id)
    }

    /// Notes what a device running iPadOS said about itself (its model and,
    /// when it tells, its ECID), so it is recognised without a DFU sighting.
    pub fn learn(&mut self, udid: &str, answers: lockdown::Answers) {
        self.told.insert(udid.to_ascii_lowercase(), answers);
    }

    /// The model a device running iOS told (lockdown), if it did.
    pub fn told_product(&self, usb: &orchard_usb::Device) -> Option<String> {
        self.told.get(usb.identity.udid.as_deref()?)?.product.clone()
    }

    /// Whether this UDID has been asked about.
    pub fn knows(&self, udid: &str) -> bool {
        self.told.contains_key(&udid.to_ascii_lowercase())
    }

    /// Which known device a device running iPadOS is. Its ECID, when the
    /// UDID carries one (A12 and later); otherwise, on older chips whose UDID
    /// is a hash, the one known device that isn't present in another mode,
    /// and only while it is the only device running iPadOS. The companion
    /// confirms the ECID once it answers.
    fn normal_owner(&mut self, catalog: &Catalog, usb: &orchard_usb::Device, listing: &[orchard_usb::Device], others: &[Seen]) -> Option<Device> {
        let told = usb.identity.udid.as_deref().and_then(|u| self.told.get(u)).cloned().unwrap_or_default();
        if let Some(ecid) = usb.identity.ecid.as_deref().or(told.ecid.as_deref()) {
            let id = DeviceId(ecid_key(ecid));
            if let Some(device) = self.known.get(&id) {
                return Some(device.clone());
            }
            // Plugged in running iPadOS: its model names the board.
            let board = told.product.as_deref().and_then(|p| catalog.board_for_product(p).ok())?;
            let device = describe(catalog, &board, &id.0);
            self.known.insert(id, device.clone());
            return Some(device);
        }
        let running = listing.iter().filter(|d| d.mode == orchard_usb::Mode::Normal).count();
        let absent: Vec<&Device> =
            self.known.values().filter(|d| !others.iter().any(|s| s.device.id == d.id)).collect();
        // ... and only at the port it was seen at: another iPhone plugged in
        // elsewhere isn't it.
        if let (1, [only]) = (running, absent.as_slice()) {
            if self.locations.get(&only.id).is_none_or(|at| *at == usb.location) {
                return Some((*only).clone());
            }
        }
        // No ECID told and no known device it could be: the model alone shows
        // which device to put in DFU (1.2). Its DFU sighting brings the ECID.
        let product = told.product.as_deref()?;
        let udid = usb.identity.udid.as_deref()?;
        catalog.board_for_product(product).ok().map(|board| describe(catalog, &board, &format!("udid-{udid}")))
    }

    /// Where a device was last seen on USB.
    pub fn location(&self, id: &DeviceId) -> Option<&str> {
        self.locations.get(id).map(String::as_str)
    }

    /// A listed device's ECID: from the boot ROM or iBoot, or what it said
    /// while running iOS.
    pub fn ecid(&self, usb: &orchard_usb::Device) -> Option<String> {
        let told = || usb.identity.udid.as_deref().and_then(|u| self.told.get(u)).and_then(|t| t.ecid.clone());
        usb.identity.ecid.clone().or_else(told)
    }

    /// The Apple devices in `listing` other than `target` (any mode), by the
    /// shared rule (orchard_usb::others): its ECID, else the port it was
    /// last seen at. A device running iOS that told its ECID is known by it.
    pub fn others(&self, listing: &[orchard_usb::Device], target: Option<&DeviceId>) -> Vec<orchard_usb::Device> {
        let listing: Vec<orchard_usb::Device> = listing
            .iter()
            .cloned()
            .map(|mut d| {
                if d.identity.ecid.is_none() {
                    let told = d.identity.udid.as_deref().and_then(|u| self.told.get(u)).and_then(|t| t.ecid.clone());
                    d.identity.ecid = told;
                }
                d
            })
            .collect();
        let target = target.map(|id| orchard_usb::Target {
            // Ids that aren't an ECID: a device known only by its UDID or board.
            ecid: (!id.0.contains('-')).then_some(id.0.as_str()),
            location: self.locations.get(id).map(String::as_str),
        });
        orchard_usb::others(&listing, target).into_iter().cloned().collect()
    }

    /// Identifies a listing and returns the events since the last one.
    pub fn see(&mut self, catalog: &Catalog, listing: &[orchard_usb::Device]) -> Vec<Event> {
        let mut now = Vec::new();
        self.install.clear();
        self.normal.clear();
        let mut normal = Vec::new();
        for usb in listing {
            if usb.mode == orchard_usb::Mode::Normal {
                normal.push(usb);
                continue;
            }
            if let Some(seen) = identify(catalog, usb) {
                self.known.insert(seen.device.id.clone(), seen.device.clone());
                self.locations.insert(seen.device.id.clone(), usb.location.clone());
                now.push(seen);
            } else if let (Some(UsbMode::Installed), None, Some(board)) =
                (orchard_mode(usb), usb.identity.ecid.as_deref(), usb.identity.board.as_deref())
            {
                // The installed system names its board, not the device (it
                // shouldn't broadcast a hardware ID): the one known device of
                // that board if there is one (the one that was just installing),
                // else a device of that board, enough to show it and its DFU
                // steps; in DFU the boot ROM names the device.
                let mut of_board = self.known.values().filter(|d| d.board.eq_ignore_ascii_case(board));
                let device = match (of_board.next(), of_board.next()) {
                    (Some(device), None) => Some(device.clone()),
                    _ => catalog.board(board).ok().map(|b| describe(catalog, &b, &format!("board-{board}"))),
                };
                if let Some(device) = device {
                    now.push(Seen { device, mode: UsbMode::Installed });
                }
            } else if let (Some(mode), Some(ecid)) = (orchard_mode(usb), usb.identity.ecid.as_deref()) {
                // Not seen in DFU this run (the installer restarted): the
                // system's serial names its board.
                let id = DeviceId(ecid_key(ecid));
                if !self.known.contains_key(&id) {
                    if let Some(board) = usb.identity.board.as_deref().and_then(|b| catalog.board(b).ok()) {
                        self.known.insert(id.clone(), describe(catalog, &board, &id.0));
                    }
                }
                if let Some(device) = self.known.get(&id) {
                    self.locations.insert(device.id.clone(), usb.location.clone());
                    if mode == UsbMode::Install {
                        self.install.insert(device.id.clone(), usb.clone());
                    }
                    now.push(Seen { device: device.clone(), mode });
                }
            }
        }
        for usb in normal {
            if let Some(device) = self.normal_owner(catalog, usb, listing, &now) {
                self.locations.insert(device.id.clone(), usb.location.clone());
                self.normal.insert(device.id.clone(), usb.clone());
                now.push(Seen { device, mode: UsbMode::Normal });
            }
        }
        self.update(now)
    }

    /// The events between the last listing and this one: detaches first, then
    /// attaches and mode changes (a mode change is an attach in the new mode).
    pub fn update(&mut self, now: Vec<Seen>) -> Vec<Event> {
        let now: BTreeMap<DeviceId, Seen> = now.into_iter().map(|s| (s.device.id.clone(), s)).collect();
        let mut events = Vec::new();
        for id in self.present.keys() {
            if !now.contains_key(id) {
                events.push(Event::Detached { id: id.clone() });
            }
        }
        for (id, seen) in &now {
            if self.present.get(id).map(|s| s.mode) != Some(seen.mode) {
                events.push(Event::Attached { device: seen.device.clone(), mode: seen.mode });
            }
        }
        self.present = now;
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_device_is_told_by_its_ecid_or_its_port() {
        let device = usb_at(orchard_usb::Mode::Dfu, "CPID:8010 BDID:0C ECID:00000000000000AB", "0x1100000");
        let ecid = device.identity.ecid.clone();
        let only = |ecid: Option<&str>, location: Option<&str>| Only { ecid: ecid.map(str::to_owned), location: location.map(str::to_owned) };
        assert!(only(Some("0xab"), None).is(&device, ecid.as_deref()));
        assert!(!only(Some("ac"), None).is(&device, ecid.as_deref()));
        // Running iOS before it said who it is: not it yet.
        assert!(!only(Some("ab"), None).is(&device, None));
        assert!(only(None, Some("0x1100000")).is(&device, None));
        assert!(!only(None, Some("0x1200000")).is(&device, ecid.as_deref()));
        assert!(!only(Some("ab"), Some("0x1200000")).is(&device, ecid.as_deref()));
        assert!(Only::default().is(&device, None));
    }

    #[test]
    fn a_32_bit_board_is_unsupported_whatever_its_status() {
        let root = std::env::temp_dir().join(format!("orchard-runtime-arch-{}", std::process::id()));
        let write = |path: &str, text: &str| {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), text).unwrap();
        };
        for (board, chip, architecture) in [("wide", 0x8010, "arm64"), ("narrow", 0x8950, "arm"), ("plain", 0x8011, "")] {
            write(
                &format!("devices/{board}/device.toml"),
                &format!(
                    "[device]\nname = \"{board}\"\napple_products = [\"iPad{chip},1\"]\ndt_compatible = [\"apple,{board}\"]\n\
                     [match.dfu]\nchip = {chip}\nboard = 1\n[support]\nstatus = \"experimental\"\n"
                ),
            );
            write(
                &format!("chips/{board}.toml"),
                &format!("cpid = {chip}\nname = \"A\"\nsoc = \"s{chip}\"\narchitecture = \"{architecture}\"\n"),
            );
        }
        let catalog = Catalog::open(&root).unwrap();
        let support = |name: &str| describe(&catalog, &catalog.board(name).unwrap(), "1").support;
        assert_eq!(support("wide"), Support::Experimental);
        assert_eq!(support("plain"), Support::Experimental);
        assert_eq!(support("narrow"), Support::Unsupported);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn seen(ecid: &str, mode: UsbMode) -> Seen {
        Seen {
            device: Device {
                id: DeviceId(ecid.into()),
                board: "j71b".into(),
                name: "iPad (6th generation)".into(),
                model: Model::IpadHome,
                sequence: Sequence::HomePower,
                support: Support::Supported,
            },
            mode,
        }
    }

    fn usb(mode: orchard_usb::Mode, serial: &str) -> orchard_usb::Device {
        usb_at(mode, serial, &format!("loc-{serial}"))
    }

    fn usb_at(mode: orchard_usb::Mode, serial: &str, location: &str) -> orchard_usb::Device {
        orchard_usb::Device {
            mode,
            system: None,
            location: location.into(),
            product: None,
            identity: match mode {
                orchard_usb::Mode::Normal => orchard_usb::Identity::from_udid(serial),
                _ => orchard_usb::Identity::parse(serial),
            },
        }
    }

    #[test]
    fn a_device_running_ipados_is_the_known_one_only_when_nothing_else_could_be() {
        let catalog = Catalog::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join("orchard").as_path());
        let Ok(catalog) = catalog else { return };
        let dfu = usb_at(orchard_usb::Mode::Dfu, "CPID:8010 BDID:0C ECID:0011223344556677", "port-1");
        let ipados = usb_at(orchard_usb::Mode::Normal, "0123456789abcdef0123456789abcdef01234567", "port-1");
        let other = usb(orchard_usb::Mode::Normal, "1111111111111111111111111111111111111111");
        let mut w = Watcher::default();
        w.see(&catalog, std::slice::from_ref(&dfu));
        let events = w.see(&catalog, std::slice::from_ref(&ipados));
        // A mode change is an attach in the new mode.
        assert!(matches!(&events[..], [Event::Attached { mode: UsbMode::Normal, .. }]), "{events:?}");
        assert!(w.normal_device(&DeviceId("11223344556677".into())).is_some());
        // Two devices running iPadOS: neither is attributed.
        let events = w.see(&catalog, &[ipados.clone(), other]);
        assert!(matches!(&events[..], [Event::Detached { .. }]), "{events:?}");
        // A UDID with an ECID names its device.
        let mut w = Watcher::default();
        w.see(&catalog, std::slice::from_ref(&dfu));
        let modern = usb(orchard_usb::Mode::Normal, "00008010-0011223344556677");
        let events = w.see(&catalog, &[modern, usb(orchard_usb::Mode::Normal, "2222222222222222222222222222222222222222")]);
        assert!(events.iter().any(|e| matches!(e, Event::Attached { mode: UsbMode::Normal, .. })), "{events:?}");
    }

    #[test]
    fn ecids_compare_without_padding() {
        assert_eq!(ecid_key("0011223344556677"), ecid_key("11223344556677"));
        assert_eq!(ecid_key("0x1A2B"), "1a2b");
    }

    #[test]
    fn layouts_by_product() {
        assert_eq!(layout("iPad7,5"), (Model::IpadHome, Sequence::HomePower));
        assert_eq!(layout("iPhone8,1"), (Model::IphoneHome, Sequence::HomePower));
        assert_eq!(layout("iPhone9,3"), (Model::IphoneHome, Sequence::VolumeDownSide));
        assert_eq!(layout("iPhone10,4"), (Model::IphoneHome, Sequence::VolumeSequence));
        assert_eq!(layout("iPhone10,6"), (Model::IphoneNoHome, Sequence::VolumeSequence));
    }

    #[test]
    fn watcher_reports_attach_mode_change_and_detach_once() {
        let mut w = Watcher::default();
        assert!(matches!(w.update(vec![seen("a", UsbMode::Recovery)])[..], [Event::Attached { mode: UsbMode::Recovery, .. }]));
        assert!(w.update(vec![seen("a", UsbMode::Recovery)]).is_empty(), "unchanged: nothing");
        assert!(matches!(w.update(vec![seen("a", UsbMode::Dfu)])[..], [Event::Attached { mode: UsbMode::Dfu, .. }]));
        assert!(matches!(w.update(vec![])[..], [Event::Detached { .. }]));
    }

    /// The installer's device (j71b's chip and board, ECID 11223344556677)
    /// on port-1 in DFU, then its other modes on the same cable.
    fn the_ipad_and_another_device(catalog: &Catalog) {
        let dfu = usb_at(orchard_usb::Mode::Dfu, "CPID:8010 BDID:0C ECID:0011223344556677", "port-1");
        let id = DeviceId("11223344556677".into());
        let mut w = Watcher::default();
        w.see(catalog, std::slice::from_ref(&dfu));
        let others = |w: &Watcher, listing: &[orchard_usb::Device]| w.others(listing, Some(&id)).len();
        // Its own modes, and the moments between them, are never another device.
        let pongo = usb_at(orchard_usb::Mode::Pongo, "CPID:8010 ECID:0011223344556677", "port-1");
        let ios = usb_at(orchard_usb::Mode::Normal, "0123456789abcdef0123456789abcdef01234567", "port-1");
        let m1n1 = usb_at(orchard_usb::Mode::M1n1, "", "port-1");
        for listing in [vec![dfu.clone()], vec![], vec![pongo.clone()], vec![ios.clone()], vec![m1n1]] {
            assert_eq!(others(&w, &listing), 0, "{listing:?}");
        }
        // An iPhone running iOS (a UDID that is a hash) on another port, beside
        // the iPad in DFU, and alone while the iPad is off USB after checkm8.
        let phone = usb_at(orchard_usb::Mode::Normal, "1111111111111111111111111111111111111111", "port-2");
        assert_eq!(others(&w, &[dfu.clone(), phone.clone()]), 1);
        assert_eq!(others(&w, std::slice::from_ref(&phone)), 1);
        // ... and the watcher doesn't take it for the iPad having restarted.
        let events = w.see(catalog, std::slice::from_ref(&phone));
        assert!(!events.iter().any(|e| matches!(e, Event::Attached { device, .. } if device.id == id)), "{events:?}");
        // A device whose UDID carries its ECID, even on the iPad's port.
        let modern = usb_at(orchard_usb::Mode::Normal, "00008020-0099887766554433", "port-1");
        assert_eq!(others(&w, std::slice::from_ref(&modern)), 1);
        // Plugged in mid-run, while the iPad runs Pongo.
        assert_eq!(others(&w, &[pongo, phone]), 1);
    }

    #[test]
    fn another_device_is_never_the_ipad() {
        let catalog = Catalog::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join("orchard").as_path());
        let Ok(catalog) = catalog else { return };
        the_ipad_and_another_device(&catalog);
    }

}

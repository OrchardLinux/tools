use crate::args::{DevicesArgs, Global};
use crate::output::{Output, Report};
use crate::table::{self, Cell, Style};
use orchard_boards::catalog::Catalog;
use orchard_boards::data::board_data;
use orchard_cache::store::Store;
use orchard_cache::Result;
use orchard_device::deadline::Deadline;
use orchard_device::select;
use orchard_device::session::Session;
use orchard_runtime::lockdown::{self, Answers};
use orchard_runtime::usbmux;
use orchard_usb::{Device, Host, Mode};
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct Devices {
    devices: Vec<Row>,
    /// Whether the user narrowed the list with a mode or device flag.
    #[serde(skip)]
    filtered: bool,
}

#[derive(Serialize)]
struct Row {
    #[serde(flatten)]
    device: Device,
    /// The board from the orchard data, when data is set and it matches.
    board: Option<String>,
    /// What the device is called, such as "iPad (6th generation)", when its
    /// board is known.
    name: Option<String>,
    /// Its product type, such as `iPad7,5`, which a device running iOS says
    /// when it is asked.
    model: Option<String>,
    /// The serial port to open for a console: a Linux system's, m1n1's proxy.
    serial_port: Option<String>,
}

impl Report for Devices {
    fn human(&self) -> Vec<u8> {
        if self.devices.is_empty() {
            let text: &[u8] = if self.filtered {
                b"No matching device.\n"
            } else {
                b"No device in DFU, recovery, Pongo, m1n1 or Linux mode.\n"
            };
            return text.to_vec();
        }
        // The system and board columns appear only when a device has one.
        let systems = self.devices.iter().any(|r| r.device.system.is_some());
        let boards = self.devices.iter().any(|r| r.board.is_some());
        let pwned = self
            .devices
            .iter()
            .any(|r| r.device.identity.pwned.is_some());
        let mut headers = vec!["Mode"];
        if systems {
            headers.push("System");
        }
        headers.extend(["Location", "CPID", "BDID", "ECID"]);
        if pwned {
            headers.push("Pwned");
        }
        if boards {
            headers.push("Board");
        }
        let ports = self.devices.iter().any(|r| r.serial_port.is_some());
        if ports {
            headers.push("Serial port");
        }
        headers.push("Product");
        let rows: Vec<Vec<Cell>> = self
            .devices
            .iter()
            .map(|Row { device: d, board, serial_port, name, .. }| {
                let id = &d.identity;
                let mut row = vec![Cell::mode(d.mode.as_str())];
                if systems {
                    let system = d.system.map(|s| s.as_str());
                    row.push(Cell::optional(system, Style::Mode));
                }
                row.extend([
                    Cell::number(&d.location),
                    Cell::optional(id.chip.as_deref(), Style::Number),
                    Cell::optional(id.board_id.as_deref(), Style::Number),
                    Cell::optional(id.ecid.as_deref(), Style::Number),
                ]);
                if pwned {
                    row.push(Cell::optional(id.pwned.as_deref(), Style::Mode));
                }
                if boards {
                    row.push(Cell::optional(board.as_deref(), Style::Board));
                }
                if ports {
                    row.push(Cell::optional(serial_port.as_deref(), Style::Plain));
                }
                // What it is called, where that is known: USB itself only
                // says "iPhone" or "iPad".
                row.push(Cell::optional(name.as_deref().or(d.product.as_deref()), Style::Plain));
                row
            })
            .collect();
        format!("{}\n", table::render(&headers, &rows)).into_bytes()
    }
}

pub fn run(global: &Global, args: &DevicesArgs) -> Result<Output> {
    let selector = global.device.selector(args.mode.map(Into::into));
    let filtered =
        selector.mode.is_some() || selector.ecid.is_some() || selector.location.is_some();
    let devices = select::matching(&mut Host, &selector)?;
    report(global, devices, filtered)
}

/// Waits for the one device the flags choose to be in `mode`, and shows it.
pub fn wait(global: &Global, mode: Mode, timeout: Duration) -> Result<Output> {
    let selector = global.device.selector(Some(mode));
    // Inside a session and without a device flag, the device to wait for
    // is the session's.
    if selector.ecid.is_none() && selector.location.is_none() {
        if let Some(session) = Session::resume(Host, mode, timeout)? {
            return report(global, vec![session.device().clone()], true);
        }
    }
    let deadline = Deadline::after(timeout);
    let device = select::wait_one(&mut Host, &selector, &deadline)?;
    report(global, vec![device], true)
}

/// How long a listing waits for devices running iOS to say who they are.
/// One that has just been plugged in answers a moment later; the listing
/// then shows it as USB does and the next one knows it.
const ASKING: Duration = Duration::from_millis(300);

/// Asks every device running iOS for its product type and ECID, all at
/// once, and returns what came back in time, by UDID. It reads two values
/// of the device and changes nothing on it.
fn ask(devices: &[Device]) -> HashMap<String, Answers> {
    let (sender, answers) = std::sync::mpsc::channel();
    let mut asked = 0;
    for udid in devices.iter().filter(|d| d.mode == Mode::Normal).filter_map(|d| d.identity.udid.clone()) {
        asked += 1;
        let sender = sender.clone();
        std::thread::spawn(move || {
            let said = lockdown::ask(std::path::Path::new(usbmux::SOCKET), &udid).ok();
            let _ = sender.send((udid, said));
        });
    }
    let (mut told, until) = (HashMap::new(), Instant::now() + ASKING);
    while told.len() < asked {
        match answers.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok((udid, Some(said))) => {
                told.insert(udid, said);
            }
            Ok((_, None)) => asked -= 1,
            Err(_) => break,
        }
    }
    told
}

/// A device with what it said about itself: its ECID where USB gave none,
/// and its product type.
fn with_answers(mut device: Device, told: &HashMap<String, Answers>) -> (Device, Option<String>) {
    let said = device.identity.udid.as_ref().and_then(|udid| told.get(udid));
    if device.identity.ecid.is_none() {
        device.identity.ecid = said.and_then(|s| s.ecid.clone());
    }
    let model = said.and_then(|s| s.product.clone());
    (device, model)
}

fn report(global: &Global, devices: Vec<Device>, filtered: bool) -> Result<Output> {
    // Board names only from data already here: listing devices never downloads.
    let store = Store::open(global.cache.as_deref())?;
    let catalog =
        board_data::existing(global.data.as_deref(), &store).and_then(|d| Catalog::open(&d).ok());
    let hex = |s: &Option<String>| s.as_deref().and_then(|s| u32::from_str_radix(s, 16).ok());
    let ports = orchard_usb::serial_ports(&devices);
    let told = ask(&devices);
    let devices = devices
        .into_iter()
        .zip(ports)
        .map(|(device, port)| {
            let (device, model) = with_answers(device, &told);
            // By its chip and board numbers, or by the product type it said.
            let board = catalog.as_ref().and_then(|c| {
                let numbers = || c.board_for(hex(&device.identity.chip)?, hex(&device.identity.board_id)?).ok();
                numbers().or_else(|| c.board_for_product(model.as_deref()?).ok())
            });
            let (name, board) = (board.as_ref().map(|b| b.display_name.clone()), board.map(|b| b.name));
            Row { device, board, name, model, serial_port: port.map(|p| p.display().to_string()) }
        })
        .collect();
    Ok(Devices { devices, filtered }.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_usb::{Identity, System};

    fn row(mode: Mode, system: Option<System>, serial: &str, board: Option<&str>) -> Row {
        Row {
            device: Device {
                mode,
                system,
                location: "0x100000".into(),
                product: None,
                identity: Identity::parse(serial),
            },
            board: board.map(str::to_owned),
            name: None,
            model: None,
            serial_port: None,
        }
    }

    #[test]
    fn a_device_running_ios_is_known_by_what_it_says() {
        let mut phone = row(Mode::Normal, None, "", None).device;
        phone.identity = Identity::from_udid("00008030-001A2B3C4D5E6F70");
        phone.product = Some("iPhone".into());
        let udid = phone.identity.udid.clone().unwrap();
        let mut told = HashMap::new();
        told.insert(udid, Answers { ecid: Some("1a2b3c4d5e6f70".into()), product: Some("iPhone9,3".into()) });
        let (known, model) = with_answers(phone.clone(), &told);
        assert_eq!(model.as_deref(), Some("iPhone9,3"));
        assert!(known.identity.ecid.is_some());
        // One that has not answered stays as USB shows it.
        let (unknown, model) = with_answers(phone.clone(), &HashMap::new());
        assert_eq!((model, unknown.identity.ecid.clone()), (None, phone.identity.ecid.clone()));
        // The name stands where the product was; nothing but that changes.
        let named = Row { device: known, board: Some("d10".into()), name: Some("iPhone 7".into()), model: Some("iPhone9,3".into()), serial_port: None };
        let text = String::from_utf8(Devices { devices: vec![named], filtered: false }.human()).unwrap();
        assert!(text.contains("iPhone 7") && text.contains("d10"), "{text}");
        // With no device running iOS nobody is asked, and nothing is waited for.
        let started = Instant::now();
        assert!(ask(&[row(Mode::Dfu, None, "CPID:8010 BDID:0C ECID:1", None).device]).is_empty());
        assert!(started.elapsed() < ASKING);
    }

    #[test]
    fn a_serial_port_gets_its_column() {
        let mut linux = row(Mode::Linux, Some(System::Ram), "", None);
        linux.serial_port = Some("/dev/cu.usbmodem1".into());
        let devices = Devices { devices: vec![linux, row(Mode::Dfu, None, "CPID:8010 BDID:0C ECID:1", None)], filtered: false };
        let json = serde_json::to_value(&devices).unwrap();
        assert_eq!(json["devices"][0]["serial_port"], "/dev/cu.usbmodem1");
        assert!(json["devices"][1]["serial_port"].is_null());
        let table = String::from_utf8(devices.human()).unwrap();
        assert!(table.contains("│ Serial port       │ Product │"));
        assert!(table.contains("│ /dev/cu.usbmodem1 │"));
    }

    fn text(devices: Vec<Row>) -> String {
        report(devices, false)
    }

    fn report(devices: Vec<Row>, filtered: bool) -> String {
        String::from_utf8(Devices { devices, filtered }.human()).unwrap()
    }

    #[test]
    fn columns_follow_what_the_devices_have() {
        let dfu = || row(Mode::Dfu, None, "CPID:8010 BDID:0C ECID:1", Some("j1"));
        let plain = text(vec![row(Mode::Dfu, None, "CPID:8010 BDID:0C ECID:1", None)]);
        assert!(plain.contains("│ Mode │ Location │ CPID │ BDID │ ECID │ Product │"));
        assert!(plain.contains("│ dfu  │ 0x100000 │ 8010 │ 0c   │ 1    │ —       │"));

        let full = text(vec![
            dfu(),
            row(Mode::Linux, Some(System::Installed), "", Some("j2")),
        ]);
        assert!(full.contains("│ Mode  │ System    │ Location │ CPID │ BDID │ ECID │ Board │"));
        assert!(full.contains("│ dfu   │ —         │ 0x100000 │ 8010 │ 0c   │ 1    │ j1    │"));
        assert!(full.contains("│ linux │ installed │ 0x100000 │ —    │ —    │ —    │ j2    │"));
    }

    #[test]
    fn a_pwned_device_shows_its_marker() {
        let table = text(vec![
            row(
                Mode::Dfu,
                None,
                "CPID:8942 BDID:00 ECID:2 PWND:[checkm8]",
                None,
            ),
            row(Mode::Dfu, None, "CPID:8010 BDID:0C ECID:1", None),
        ]);
        assert!(table.contains("│ ECID │ Pwned   │ Product │"));
        assert!(table.contains("│ 2    │ checkm8 │"));
        assert!(table.contains("│ 1    │ —       │"));
    }

    #[test]
    fn no_devices() {
        assert_eq!(
            text(vec![]),
            "No device in DFU, recovery, Pongo, m1n1 or Linux mode.\n"
        );
        assert_eq!(report(vec![], true), "No matching device.\n");
    }

    #[test]
    fn json_keeps_board_name_and_board_id_apart() {
        let devices = vec![row(Mode::Dfu, None, "CPID:8010 BDID:0C", Some("j1"))];
        let json = serde_json::to_value(Devices {
            devices,
            filtered: false,
        })
        .unwrap();
        assert_eq!(json["devices"][0]["board"], "j1");
        assert_eq!(json["devices"][0]["board_id"], "0c");
        assert_eq!(json["devices"][0]["chip"], "8010");
    }
}

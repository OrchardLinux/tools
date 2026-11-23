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
use orchard_usb::{Device, Host, Mode};
use serde::Serialize;
use std::time::Duration;

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
        headers.push("Product");
        let rows: Vec<Vec<Cell>> = self
            .devices
            .iter()
            .map(|Row { device: d, board }| {
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
                row.push(Cell::optional(d.product.as_deref(), Style::Plain));
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

fn report(global: &Global, devices: Vec<Device>, filtered: bool) -> Result<Output> {
    // Board names only from data already here: listing devices never downloads.
    let store = Store::open(global.cache.as_deref())?;
    let catalog =
        board_data::existing(global.data.as_deref(), &store).and_then(|d| Catalog::open(&d).ok());
    let hex = |s: &Option<String>| s.as_deref().and_then(|s| u32::from_str_radix(s, 16).ok());
    let devices = devices
        .into_iter()
        .map(|device| {
            let board = catalog.as_ref().and_then(|c| {
                c.board_for(hex(&device.identity.chip)?, hex(&device.identity.board_id)?)
                    .ok()
                    .map(|b| b.name)
            });
            Row { device, board }
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
        }
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

//! Firmware keys from a connected device: a boot-firmware build that lacks
//! keys hands its keybags here, and gaster decrypts them on the device.
use crate::host::Connect;
use crate::methods::{self, Kind};
use crate::select::Selector;
use crate::session::Session;
use orchard_boards::catalog::{Board, Catalog};
use orchard_boards::firmware_build::{self, Built, KeyDevice, Request};
use orchard_cache::store::Store;
use orchard_cache::{Code, Error, Result};
use std::path::Path;
use std::time::Duration;

/// Builds firmware like [`firmware_build::build`]; with a `device`, missing
/// keys come from it. A cache hit never opens USB.
pub fn build(
    data: &Path,
    cache: Option<&Path>,
    name: &str,
    out: &Path,
    request: &Request<'_>,
    device: Option<&Selector>,
) -> Result<Built> {
    let mut keys = device.map(|selector| DeviceKeys { selector });
    firmware_build::build_with_device(
        data,
        cache,
        name,
        out,
        request,
        keys.as_mut().map(|k| k as &mut dyn KeyDevice),
    )
}

/// The selected device, opened only when a build asks for keys.
struct DeviceKeys<'a> {
    selector: &'a Selector,
}

impl KeyDevice for DeviceKeys<'_> {
    fn decrypt(
        &mut self,
        catalog: &Catalog,
        board: &Board,
        store: &Store,
        bags: &[[u8; 48]],
    ) -> Result<Vec<[u8; 48]>> {
        let mut session = Session::start(
            orchard_usb::Host,
            self.selector,
            Some(Duration::from_secs(300)),
        )?;
        let needs_entry = needs_entry(catalog, board, &session)?;
        let worker = session.gaster_worker(store)?;
        if needs_entry {
            session.gaster(None, &worker)?;
        }
        session.gaster(Some(bags), &worker)
    }
}

/// Checks that the selected board is in compatible DFU and whether it needs entry.
fn needs_entry<D: Connect>(catalog: &Catalog, board: &Board, session: &Session<D>) -> Result<bool> {
    let id = session.identity();
    let number = |s: &Option<String>| {
        s.as_deref()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
    };
    if number(&id.chip) != Some(board.chip) || number(&id.board_id) != Some(board.board) {
        return Err(Error::new(
            Code::DeviceChanged,
            "key device does not match the firmware board",
        ));
    }
    if session.device().mode != orchard_usb::Mode::Dfu {
        return Err(Error::new(
            Code::InvalidInput,
            "device keys require compatible DFU",
        ));
    }
    match session.device().identity.pwned.as_deref() {
        Some("gaster") => return Ok(false),
        Some(_) => {
            return Err(Error::new(
                Code::InvalidInput,
                "key acquisition needs compatible gaster DFU",
            )
            .hint("use cached/public keys or --keys FILE from this starting state"));
        }
        None => {}
    }
    let chip = catalog.chip(board.chip)?;
    let supported = methods::read(catalog)?.iter().any(|e| {
        e.kind == Kind::Entry
            && e.name == "gaster"
            && e.chips.contains(&chip.soc)
            && e.hosts.iter().any(|h| h == methods::host())
    });
    if !supported {
        return Err(Error::new(
            Code::Unsupported,
            "this board has no supported gaster entry",
        ));
    }
    Ok(true)
}

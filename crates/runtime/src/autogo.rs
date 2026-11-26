//! The `autogo` boot route of the 32-bit boards, Project Cascadia's: the
//! patched iBSS, then an iBEC with the autogo hook, which enters what it
//! was sent as soon as an upload ends, with no command. It is sent the
//! bundle (Cascadia's loader, the kernel, the device tree and the
//! initramfs), and then the bundle's own loader again, which the hook
//! runs: that is why the loader goes last and nothing sends `go`
//! (`devices/a5/tools/flash.sh`, `scripts/patch-ibec-autogo.py`).
//!
//! As the `ibss` route there, it starts from a boot ROM that checkm8 from
//! a Pico entered, or from kDFU.
use crate::exec::Executor;
use orchard_boards::catalog::Catalog;
use orchard_boards::firmware_build::Boot;
use orchard_boards::boot_firmware;
use orchard_cache::store::Store;
use orchard_cache::{Code, Error, Result};
use orchard_device::session::Session;
use orchard_device::usb;
use orchard_flow::Event;
use orchard_formats::cascadia::Bundle;
use orchard_usb::Host;
use std::path::Path;
use std::time::Duration;

/// Takes the session's device, a `board` in pwned DFU or kDFU, into the
/// Linux of `bundle`. Linux then shows up on USB by itself.
pub fn start(executor: &Executor, session: &mut Session<Host>, board: &str, keys: Option<&Path>, bundle: &Bundle) -> Result<()> {
    let mut lines: Vec<String> = Vec::new();
    let mut say = |message: &str| {
        eprintln!("{} autogo: {message}", crate::install::stamp());
        lines.push(message.to_owned());
        let _ = executor.events().send(Event::StartLog(lines.clone()));
    };
    let catalog = Catalog::open(&executor.data()?)?;
    let board = catalog.board(board)?;
    let store = Store::open(Some(executor.cache()))?;

    say("Preparing the patched iBoot");
    let plan = boot_firmware::Plan::new(&catalog, &board, Some("iboot-autogo"))?;
    let firmware = plan.prepare(&board, &store, &Boot { chain: Some("iboot-autogo"), keys, im4m: None })?;
    if firmware.needs_device() {
        return Err(Error::new(Code::NotFound, "the firmware's keys are not in the board data, and no tool of ours reads them from this chip").hint("give them with --keys FILE"));
    }
    let images = firmware.finish(&store)?;

    let mut iboot = crate::ibss::into_ibec(session, &images, &mut say)?;
    say("Sending the bundle");
    iboot.send(&bundle.image, session.timeout(Duration::from_secs(120))?).map_err(usb::error)?;
    std::thread::sleep(Duration::from_secs(1));
    say("Sending the loader, which the iBEC starts");
    // The loader as the bundle holds it: one without the filled-in
    // configuration would replace the one the bundle brought.
    session.check()?;
    iboot.send(bundle.loader(), session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
    Ok(())
}

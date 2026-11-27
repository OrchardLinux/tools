//! The `ibss` boot route, DFU mode to m1n1, in the order it happens: gaster
//! enters the boot ROM, Apple's own iBSS (and iBEC, where the chip loads
//! one) is sent with the open patcher's changes, and that patched iBoot
//! takes m1n1 as its restore kernel. The sequence is remote_boot's
//! (`HoolockLinux/remote_boot@e254a58`, `remoteboot.sh`).
//!
//! Everything is made before the device is touched, except the firmware's
//! keys where they are not in the cache yet: those the device itself
//! unwraps once gaster is in.
use crate::exec::Executor;
use orchard_boards::catalog::{Board, Catalog};
use orchard_boards::data::loaders;
use orchard_boards::firmware_build::Boot;
use orchard_boards::{boot_firmware, pack};
use orchard_cache::store::Store;
use orchard_cache::{Code, Error, Result};
use orchard_device::host::Connect;
use orchard_device::session::Session;
use orchard_device::usb;
use orchard_flow::Event;
use orchard_usb::dfu::Pwned;
use orchard_usb::{Host, Mode, Sent};
use std::path::Path;
use std::time::{Duration, Instant};

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::NotFound, message.into())
}

fn acknowledged(sent: Sent, command: &str) -> Result<()> {
    match sent {
        Sent::Acknowledged => Ok(()),
        Sent::Unconfirmed => Err(Error::new(Code::Usb, format!("recovery {command} was not acknowledged"))),
    }
}

/// Takes the session's device, a `board` in DFU mode, into what `image`
/// makes of m1n1's Mach-O build: m1n1 as it is, or with a system behind it
/// that it starts at once. What it started then shows up on USB. `keys` is
/// a firmware key file to use instead of the cache and the device.
pub fn start(executor: &Executor, session: &mut Session<Host>, board: &str, keys: Option<&Path>, image: impl FnOnce(&[u8]) -> Result<Vec<u8>>) -> Result<()> {
    let mut lines: Vec<String> = Vec::new();
    let mut say = |message: &str| {
        eprintln!("{} ibss: {message}", crate::install::stamp());
        lines.push(message.to_owned());
        let _ = executor.events().send(Event::StartLog(lines.clone()));
    };

    let catalog = Catalog::open(&executor.data()?)?;
    let board = catalog.board(board)?;
    let chip = catalog.chip(board.chip)?;
    let store = Store::open(Some(executor.cache()))?;
    if chip.image_format == "img3" {
        return start_arm(session, &catalog, &board, &store, keys, image, &mut say);
    }
    let two_stage = pack::two_stage(&chip);

    say("Preparing m1n1");
    let record = loaders::resolve(&store)?;
    let m1n1 = image(&loaders::file(&store, &record, "m1n1", "m1n1-idevice.macho")?)?;
    let stub = if two_stage { loaders::file(&store, &record, "m1n1", "monitor-stub.macho")? } else { Vec::new() };

    say("Preparing the patched iBoot");
    let plan = boot_firmware::Plan::new(&catalog, &board, Some("iboot"))?;
    let mut firmware = plan.prepare(&board, &store, &Boot { chain: Some("iboot"), keys, im4m: None })?;
    firmware.check_restore(&m1n1, two_stage.then_some(&stub[..]))?;
    firmware.check_acquisition()?;

    if session.check()?.mode != Mode::Dfu {
        return Err(fail("the device is no longer in DFU mode"));
    }
    let pwned = match session.device().identity.pwned.as_deref() {
        None => false,
        Some("gaster") => true,
        Some(other) => return Err(fail(format!("the boot ROM was entered by {other}; put the device in DFU mode again"))),
    };
    let needs_keys = firmware.needs_device();
    let worker = (!pwned || needs_keys).then(|| Host.gaster_worker(&store)).transpose()?;
    let worker = || worker.as_ref().expect("the worker is fetched when it is needed");

    // The session runs gaster with its lock and finds the boot ROM again,
    // with gaster's mark.
    if !pwned {
        say("Entering the boot ROM with gaster");
        session.gaster(None, worker())?;
    }
    if needs_keys {
        say("Unwrapping the firmware keys on the device");
        firmware.acquire(|bags| session.gaster(Some(bags), worker()))?;
    }
    let images = firmware.finish(&store)?;
    let restore = pack::restore(&chip, &images, &m1n1, &stub)?;

    // gaster leaves the boot ROM mid-transfer: back to idle before an upload.
    let mut dfu = session.dfu()?;
    dfu.clear_status(session.timeout(Duration::from_secs(10))?).map_err(usb::error)?;
    dfu.reset().map_err(usb::error)?;
    drop(dfu);
    if session.wait_for(Mode::Dfu, Duration::from_secs(10))?.identity.pwned.as_deref() != Some("gaster") {
        return Err(fail("the boot ROM lost gaster's mark after its reset"));
    }

    say("Sending the patched iBSS");
    let mut dfu = session.dfu()?;
    dfu.send(pack::file(&images, "ibss.img4")?, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
    drop(dfu);

    if two_stage {
        // The iBSS shows up in DFU mode again, without the boot ROM's mark,
        // and takes the iBEC as an ordinary upload.
        say("Waiting for the iBSS");
        if let Some(mark) = &session.wait_again(Duration::from_secs(30))?.identity.pwned {
            return Err(fail(format!("the boot ROM is back (marked {mark}); the iBSS did not start")));
        }
        say("Sending the patched iBEC");
        let mut dfu = session.dfu()?;
        dfu.send(pack::file(&images, "ibec.img4")?, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
        drop(dfu);
    }

    say("Waiting for the patched iBoot");
    session.wait_for(Mode::Recovery, Duration::from_secs(60))?;
    // Recovery mode can show up before the system lets go of the device.
    let opening = Instant::now() + session.timeout(Duration::from_secs(10))?;
    let mut recovery = loop {
        match session.recovery() {
            Ok(recovery) => break recovery,
            Err(e) if e.code == Code::Usb && Instant::now() < opening => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e),
        }
    };

    say("Sending the device tree");
    recovery.send(&restore.devicetree, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
    acknowledged(recovery.command("devicetree", session.timeout(Duration::from_secs(10))?).map_err(usb::error)?, "devicetree")?;
    say("Sending the trust cache");
    recovery.send(&restore.trustcache, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
    acknowledged(recovery.command("firmware", session.timeout(Duration::from_secs(10))?).map_err(usb::error)?, "firmware")?;
    say("Sending m1n1");
    recovery.send(&restore.kernelcache, session.timeout(Duration::from_secs(120))?).map_err(usb::error)?;
    say("Starting m1n1");
    // iBoot is gone once m1n1 runs: only what shows up on USB confirms the start.
    recovery.command("bootx", session.timeout(Duration::from_secs(10))?).map_err(usb::error)?;
    Ok(())
}

/// The same route on the 32-bit chips (A5 to A6X), where no tool of ours
/// enters the boot ROM: the device is in DFU mode with a boot ROM that
/// checkm8 from a Pico entered (it shows `PWND:[checkm8]`), or in kDFU
/// mode, where a patched iBSS started from a jailbroken iOS already runs.
///
/// From the entered boot ROM the patched iBSS is started and takes the
/// patched iBEC encrypted, as the firmware's own is; after kDFU the device's
/// key is off and the iBEC goes unencrypted. The iBEC then takes Apple's
/// device tree and m1n1's 32-bit build as its kernel, and `bootx` (Project
/// Cascadia `devices/a5/tools/flash.sh` and `docs/research/boot-strategy.md`,
/// `091d441`).
fn start_arm(
    session: &mut Session<Host>,
    catalog: &Catalog,
    board: &Board,
    store: &Store,
    keys: Option<&Path>,
    image: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
    say: &mut dyn FnMut(&str),
) -> Result<()> {
    say("Preparing m1n1");
    // Orchard's published 32-bit m1n1, where the build that is current has
    // one; else the caller brings it.
    let published = loaders::resolve(store).and_then(|record| loaders::file(store, &record, "m1n1", "m1n1-armv7.macho")).unwrap_or_default();
    let m1n1 = image(&published)?;
    if !orchard_formats::m1n1::is_macho_arm(&m1n1) {
        return Err(Error::new(Code::NotFound, "Orchard's current build publishes no 32-bit m1n1 for this board").hint("give one with --m1n1 FILE (m1n1-armv7.macho), or update: orchard data update"));
    }
    let kernel = orchard_formats::m1n1::kernel_image_arm(&m1n1).map_err(|e| Error::new(Code::Format, e.to_string()))?;

    say("Preparing the patched iBoot");
    let plan = boot_firmware::Plan::new(catalog, board, Some("iboot"))?;
    let firmware = plan.prepare(board, store, &Boot { chain: Some("iboot"), keys, im4m: None })?;
    if firmware.needs_device() {
        return Err(Error::new(Code::NotFound, "the firmware's keys are not in the board data, and no tool of ours reads them from this chip").hint("give them with --keys FILE"));
    }
    let images = firmware.finish(store)?;
    let devicetree = plan.input(board, store, "DeviceTree")?;

    let mut recovery = into_ibec(session, &images, say)?;
    say("Sending the device tree");
    recovery.send(&devicetree, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
    acknowledged(recovery.command("devicetree", session.timeout(Duration::from_secs(10))?).map_err(usb::error)?, "devicetree")?;
    say("Sending m1n1");
    recovery.send(&kernel, session.timeout(Duration::from_secs(120))?).map_err(usb::error)?;
    say("Starting m1n1");
    recovery.command("bootx", session.timeout(Duration::from_secs(10))?).map_err(usb::error)?;
    Ok(())
}

/// On a 32-bit board: starts the patched iBSS and sends the patched iBEC of
/// `images`, then returns the iBEC in recovery mode. From a boot ROM that
/// checkm8 from a Pico entered, the iBSS is started and the iBEC goes
/// encrypted as the firmware's own is; in kDFU the iBSS runs already and
/// the device's key is off, so the iBEC goes unencrypted. The pauses are
/// Project Cascadia's (`devices/a5/tools/flash.sh`).
pub(crate) fn into_ibec(session: &mut Session<Host>, images: &boot_firmware::Images, say: &mut dyn FnMut(&str)) -> Result<Box<dyn orchard_device::host::RecoveryMode>> {
    if session.check()?.mode != Mode::Dfu {
        return Err(fail("the device is no longer in DFU mode"));
    }
    match session.device().identity.pwned.clone().as_deref() {
        Some("checkm8") => {
            say("Starting the patched iBSS on the entered boot ROM");
            let mut dfu = session.dfu()?;
            dfu.start_pwned(Pwned::Checkm8, pack::file(images, "ibss.raw")?, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
            drop(dfu);
            std::thread::sleep(Duration::from_secs(2));
            say("Waiting for the iBSS");
            if let Some(mark) = &session.wait_again(Duration::from_secs(30))?.identity.pwned {
                return Err(fail(format!("the boot ROM is back (marked {mark}); the iBSS did not start")));
            }
            say("Sending the patched iBEC");
            let mut dfu = session.dfu()?;
            dfu.send(pack::file(images, "ibec.encrypted.img3")?, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
        }
        // DFU mode without a mark: kDFU, where the iBSS runs already. An
        // untouched boot ROM looks the same and refuses the upload.
        None => {
            say("Sending the patched iBEC to the iBSS (kDFU)");
            let mut dfu = session.dfu()?;
            dfu.send(pack::file(images, "ibec.plain.img3")?, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
        }
        Some(other) => return Err(fail(format!("the boot ROM was entered by {other}, which this route does not start from"))),
    }
    std::thread::sleep(Duration::from_secs(1));
    say("Waiting for the patched iBEC");
    session.wait_for(Mode::Recovery, Duration::from_secs(60))?;
    let opening = Instant::now() + session.timeout(Duration::from_secs(10))?;
    loop {
        match session.recovery() {
            Ok(recovery) => return Ok(recovery),
            Err(e) if e.code == Code::Usb && Instant::now() < opening => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e),
        }
    }
}

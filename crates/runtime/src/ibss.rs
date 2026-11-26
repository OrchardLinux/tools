//! The `ibss` boot route, DFU mode to m1n1, in the order it happens: gaster
//! enters the boot ROM, Apple's own iBSS (and iBEC, where the chip loads
//! one) is sent with the open patcher's changes, and that patched iBoot
//! takes m1n1 as its restore kernel. The sequence is remote_boot's
//! (`HoolockLinux/remote_boot@e254a58`, `remoteboot.sh`).
//!
//! Everything is made before the device is touched, except the firmware's
//! keys where they are not in the cache yet: those the device itself
//! unwraps once gaster is in.
use crate::chain::the_device;
use crate::exec::Executor;
use orchard_boards::catalog::Catalog;
use orchard_boards::data::loaders;
use orchard_boards::firmware_build::Boot;
use orchard_boards::{boot_firmware, pack};
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_device::deadline::Deadline;
use orchard_device::host::Connect;
use orchard_device::usb;
use orchard_flow::{Device, Event};
use orchard_usb::dfu::Dfu;
use orchard_usb::recovery::Recovery;
use orchard_usb::{Host, Mode, Sent};
use std::time::Duration;

/// All the device work together, once the files are made.
const DEVICE_SECONDS: u64 = 300;

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::NotFound, message.into())
}

/// The device in `mode`, waited for until the deadline.
fn wait_for(device: &Device, mode: Mode, deadline: &Deadline) -> Result<orchard_usb::Device> {
    loop {
        if let Some(usb) = the_device(device, mode)? {
            return Ok(usb);
        }
        deadline
            .sleep(Duration::from_millis(100))
            .context(format!("waiting for the device in {} mode", mode.as_str()))?;
    }
}

fn acknowledged(sent: Sent, command: &str) -> Result<()> {
    match sent {
        Sent::Acknowledged => Ok(()),
        Sent::Unconfirmed => Err(Error::new(
            Code::Usb,
            format!("recovery {command} was not acknowledged"),
        )),
    }
}

/// Takes the device from DFU mode into m1n1, which then shows up on USB.
pub fn start_m1n1(executor: &Executor, device: &Device) -> Result<()> {
    start(executor, device, |m1n1| Ok(m1n1.to_vec()))
}

/// Takes the device from DFU mode into what `image` makes of m1n1's Mach-O
/// build: m1n1 as it is, or with a system behind it that it starts at once.
pub fn start(
    executor: &Executor,
    device: &Device,
    image: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
) -> Result<()> {
    let mut lines: Vec<String> = Vec::new();
    let mut say = |message: &str| {
        eprintln!("{} ibss: {message}", crate::install::stamp());
        lines.push(message.to_owned());
        let _ = executor.events().send(Event::StartLog(lines.clone()));
    };

    let catalog = Catalog::open(&executor.data()?)?;
    let board = catalog.board(&device.board)?;
    let chip = catalog.chip(board.chip)?;
    let store = Store::open(Some(executor.cache()))?;
    let two_stage = pack::two_stage(&chip);

    say("Preparing m1n1");
    let record = loaders::resolve(&store)?;
    let m1n1 = image(&loaders::file(
        &store,
        &record,
        "m1n1",
        "m1n1-idevice.macho",
    )?)?;
    let stub = if two_stage {
        loaders::file(&store, &record, "m1n1", "monitor-stub.macho")?
    } else {
        Vec::new()
    };

    say("Preparing the patched iBoot");
    let plan = boot_firmware::Plan::new(&catalog, &board, Some("iboot"))?;
    let mut firmware = plan.prepare(
        &board,
        &store,
        &Boot {
            chain: Some("iboot"),
            keys: None,
            im4m: None,
        },
    )?;
    firmware.check_restore(&m1n1, two_stage.then_some(&stub[..]))?;
    firmware.check_acquisition()?;

    let deadline = Deadline::after(Duration::from_secs(DEVICE_SECONDS));
    let mut usb = the_device(device, Mode::Dfu)?
        .ok_or_else(|| fail("the device is no longer in DFU mode"))?;
    let pwned = match usb.identity.pwned.as_deref() {
        None => false,
        Some("gaster") => true,
        Some(other) => {
            return Err(fail(format!(
                "the boot ROM was entered by {other}; put the device in DFU mode again"
            )))
        }
    };
    let needs_keys = firmware.needs_device();
    let worker = (!pwned || needs_keys)
        .then(|| Host.gaster_worker(&store))
        .transpose()?;
    let worker = || {
        worker
            .as_ref()
            .expect("the worker is fetched when it is needed")
    };

    if !pwned {
        say("Entering the boot ROM with gaster");
        Host.gaster(
            &usb,
            None,
            worker(),
            deadline.step(Duration::from_secs(120))?,
            &|_| Ok(()),
        )?;
        usb = wait_for(device, Mode::Dfu, &deadline)?;
    }
    if usb.identity.pwned.as_deref() != Some("gaster") {
        return Err(fail(
            "gaster ended, but the boot ROM does not show its mark",
        ));
    }
    if needs_keys {
        say("Unwrapping the firmware keys on the device");
        firmware.acquire(|bags| {
            Host.gaster(
                &usb,
                Some(bags),
                worker(),
                deadline.step(Duration::from_secs(120))?,
                &|_| Ok(()),
            )
        })?;
    }
    let images = firmware.finish(&store)?;
    let restore = pack::restore(&chip, &images, &m1n1, &stub)?;

    // gaster leaves the boot ROM mid-transfer: back to idle before an upload.
    let mut dfu = Dfu::open(&usb).map_err(usb::error)?;
    dfu.clear_status(deadline.step(Duration::from_secs(10))?)
        .map_err(usb::error)?;
    dfu.reset().map_err(usb::error)?;
    drop(dfu);
    usb = wait_for(device, Mode::Dfu, &deadline)?;
    if usb.identity.pwned.as_deref() != Some("gaster") {
        return Err(fail("the boot ROM lost gaster's mark after its reset"));
    }

    say("Sending the patched iBSS");
    let mut dfu = Dfu::open(&usb).map_err(usb::error)?;
    dfu.send(
        pack::file(&images, "ibss.img4")?,
        deadline.step(Duration::from_secs(60))?,
    )
    .map_err(usb::error)?;
    drop(dfu);

    if two_stage {
        // The iBSS shows up in DFU mode again, without the boot ROM's mark,
        // and takes the iBEC as an ordinary upload.
        say("Waiting for the iBSS");
        usb = loop {
            let seen = wait_for(device, Mode::Dfu, &deadline)?;
            if seen.identity.pwned.is_none() {
                break seen;
            }
            deadline
                .sleep(Duration::from_millis(100))
                .context("waiting for the iBSS")?;
        };
        say("Sending the patched iBEC");
        let mut dfu = Dfu::open(&usb).map_err(usb::error)?;
        dfu.send(
            pack::file(&images, "ibec.img4")?,
            deadline.step(Duration::from_secs(60))?,
        )
        .map_err(usb::error)?;
        drop(dfu);
    }

    say("Waiting for the patched iBoot");
    usb = wait_for(device, Mode::Recovery, &deadline)?;
    // Recovery mode can show up before the system lets go of the device.
    let opening = Deadline::after(deadline.step(Duration::from_secs(10))?);
    let mut recovery = loop {
        match Recovery::open(&usb) {
            Ok(recovery) => break recovery,
            Err(e)
                if (e.kind == orchard_usb::Kind::Busy || e.detail.contains("0xe00002c5"))
                    && opening.left().is_ok() =>
            {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(usb::error(e)),
        }
    };

    say("Sending the device tree");
    recovery
        .send(&restore.devicetree, deadline.step(Duration::from_secs(60))?)
        .map_err(usb::error)?;
    acknowledged(
        recovery
            .command("devicetree", deadline.step(Duration::from_secs(10))?)
            .map_err(usb::error)?,
        "devicetree",
    )?;
    say("Sending the trust cache");
    recovery
        .send(&restore.trustcache, deadline.step(Duration::from_secs(60))?)
        .map_err(usb::error)?;
    acknowledged(
        recovery
            .command("firmware", deadline.step(Duration::from_secs(10))?)
            .map_err(usb::error)?,
        "firmware",
    )?;
    say("Sending m1n1");
    recovery
        .send(
            &restore.kernelcache,
            deadline.step(Duration::from_secs(120))?,
        )
        .map_err(usb::error)?;
    say("Starting m1n1");
    // iBoot is gone once m1n1 runs: only m1n1 on USB confirms the start.
    recovery
        .command("bootx", deadline.step(Duration::from_secs(10))?)
        .map_err(usb::error)?;
    Ok(())
}

//! One J71b RAM boot, from Linux build outputs to Linux, in execution order.
//!
//! `orchard2 run --kernel Image.gz --dtb j71b.dtb --initramfs initramfs.cpio.gz`
//!
//! The command uses the format, firmware and USB primitives directly. The
//! only device helpers are the lock, deadline, USB error conversion and
//! isolated gaster process. All boot decisions and transfers are below.
use crate::args::Global;
use crate::output::{self, Output, Report};
use orchard_boards::boot_firmware;
use orchard_boards::catalog::{Catalog, Kind};
use orchard_boards::data::{firmware::Inputs, loaders};
use orchard_boards::{firmware_build::Boot, pack, peripheral};
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_device::{deadline::Deadline, host::Connect, lock::Lock, usb};
use orchard_formats::{cpio, fdt, m1n1};
use orchard_usb::m1n1::Proxy;
use orchard_usb::{dfu::Dfu, recovery::Recovery, Device, Host, Mode, Sent};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(clap::Args)]
pub struct Args {
    /// Linux kernel image
    #[arg(long)]
    kernel: PathBuf,
    /// Linux device tree for J71b
    #[arg(long)]
    dtb: PathBuf,
    /// RAM filesystem archive
    #[arg(long)]
    initramfs: PathBuf,
    /// Linux command line
    #[arg(long)]
    bootargs: Option<String>,
    /// Append J71b peripheral firmware to the supplied initramfs
    #[arg(long)]
    firmware: bool,
    #[arg(long, default_value = "j71b", value_parser = ["j71b"])]
    board: String,
    #[arg(long, default_value = "ibss-recovery", value_parser = ["ibss-recovery"])]
    chain: String,
    /// Local Mach-O m1n1; otherwise use Orchard's published loader
    #[arg(long)]
    m1n1: Option<PathBuf>,
    /// Firmware key override; otherwise use cached keys or acquire with gaster
    #[arg(long)]
    keys: Option<PathBuf>,
    /// IM4M override; otherwise use the bundled A10 manifest
    #[arg(long)]
    im4m: Option<PathBuf>,
    /// Total seconds for device work, after host inputs have been prepared
    #[arg(long, default_value = "300", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
}

pub fn run(global: &Global, args: &Args) -> Result<Output> {
    let started = Instant::now();
    let say = |message: &str| {
        output::progress(
            global,
            &format!("[{:.3}s] {message}", started.elapsed().as_secs_f64()),
        )
    };

    let kernel = read(&args.kernel)?;
    let dtb = read(&args.dtb)?;
    fdt::Tree::read(&dtb)?;
    let compressed = kernel.starts_with(&[0x1f, 0x8b]);
    if !compressed && kernel.get(56..60) != Some(b"ARM\x64".as_slice()) {
        return Err(invalid(
            "kernel must be an ARM64 Image or gzip-compressed Image",
        ));
    }
    if args.bootargs.as_deref().is_some_and(|s| s.contains('\0')) {
        return Err(invalid("boot arguments contain a NUL byte"));
    }
    let mut initramfs = read(&args.initramfs)?;
    let store = Store::open(global.cache.as_deref())?;
    let catalog = Catalog::open(&global.data_dir()?)?;
    let board = catalog.board("j71b")?;
    let chip = catalog.chip(board.chip)?;
    if chip.stages != ["iBSS"] || chip.image_format != "im4p" {
        return Err(invalid("J71b requires the single-stage IMG4 iBSS recipe"));
    }

    // Optionally append board firmware as a second initramfs archive.
    // The temporary directory is removed as soon as its files are packed.
    if args.firmware {
        say("Preparing J71b peripheral firmware");
        let recipe = board.recipe(Kind::Peripheral)?;
        let plan = peripheral::Plan::new(&recipe)?;
        if !plan.unsupported.is_empty() {
            let missing = plan
                .unsupported
                .iter()
                .map(|(destination, step)| format!("{step} ({destination})"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::new(
                Code::Unsupported,
                format!("unsupported peripheral firmware steps: {missing}"),
            ));
        }
        for destination in &plan.pending {
            say(&format!(
                "Firmware recipe marks {destination} as pending; skipping it"
            ));
        }
        let temporary = tempfile::tempdir()?;
        let written = plan.build(&mut Inputs::new(&store, &board, &recipe), temporary.path())?;
        let files = written
            .iter()
            .map(|file| {
                Ok((
                    file.destination.as_str(),
                    read(&temporary.path().join(&file.destination))?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let files: Vec<_> = files
            .iter()
            .map(|(path, data)| cpio::File { path, data })
            .collect();
        cpio::append(&mut initramfs, &files)?;
    }

    say("Preparing bare Mach-O m1n1");
    let loader = match &args.m1n1 {
        Some(path) => read(path)?,
        None => loaders::file(
            &store,
            &loaders::resolve(&store)?,
            "m1n1",
            "m1n1-idevice.macho",
        )?,
    };
    if !m1n1::is_macho(&loader) {
        return Err(invalid("iBSS/recovery needs ARM64 Mach-O m1n1"));
    }
    say("Preparing patched iBSS and the Apple recovery images");
    // The catalog calls this recipe 'iboot'; this command fixes its sequence
    // to J71b's single-stage iBSS/recovery boot.
    let plan = boot_firmware::Plan::new(&catalog, &board, Some("iboot"))?;
    let boot_firmware = format!("{} / {}", plan.firmware().version, plan.firmware().build);
    say(&format!("Apple boot firmware: {boot_firmware}"));
    let mut firmware = plan.prepare(
        &board,
        &store,
        &Boot {
            chain: Some("iboot"),
            keys: args.keys.as_deref(),
            im4m: args.im4m.as_deref(),
        },
    )?;
    firmware.check_restore(&loader, None)?;
    firmware.check_acquisition()?;

    // Select only a J71b in DFU. The same physical port is followed for the
    // rest of the command, checking identity whenever the device reports it.
    let original = find_j71b(global, &board)?;
    let lock = Lock::acquire(&original.location)?;
    let deadline = Deadline::after(Duration::from_secs(args.timeout));
    let device_started = Instant::now();
    let mut device = wait_for(&original, Mode::Dfu, &deadline)?;
    say(&format!(
        "J71b at {}, ECID {}",
        device.location,
        original.identity.ecid.as_deref().unwrap()
    ));

    let already_pwned = match device.identity.pwned.as_deref() {
        None => false,
        Some("gaster") => true,
        Some(_) => {
            return Err(invalid(
                "this command requires plain DFU or gaster-pwned DFU",
            ))
        }
    };
    let needs_keys = firmware.needs_device();
    let worker = (!already_pwned || needs_keys)
        .then(|| Host.gaster_worker(&store))
        .transpose()?;

    // Each worker call is bounded and inherits the device lock.
    if !already_pwned {
        say("Entering pwned DFU with gaster");
        Host.gaster(
            &device,
            None,
            worker.as_ref().expect("worker prepared for entry"),
            deadline.step(Duration::from_secs(120))?,
            &|child| lock.hand_to(child),
        )?;
        device = wait_for(&original, Mode::Dfu, &deadline)?;
    }
    if device.identity.pwned.as_deref() != Some("gaster") {
        return Err(invalid(
            "gaster finished but the device is not in gaster-pwned DFU",
        ));
    }

    // A cold firmware build may need the device's AES engine once. After
    // that, the firmware builder's cache supplies the same images directly.
    if needs_keys {
        say("Unwrapping the firmware keybags with gaster");
        firmware.acquire(|bags| {
            Host.gaster(
                &device,
                Some(bags),
                worker.as_ref().expect("worker prepared for key acquisition"),
                deadline.step(Duration::from_secs(120))?,
                &|child| lock.hand_to(child),
            )
        })?;
    }
    let images = firmware.finish(&store)?;
    let restore = pack::restore(&chip, &images, &loader, &[])?;
    let ibss = pack::file(&images, "ibss.img4")?;

    say("Resetting the pwned DFU receiver");
    let mut dfu = Dfu::open(&device).map_err(usb::error)?;
    dfu.clear_status(deadline.step(Duration::from_secs(10))?)
        .map_err(usb::error)?;
    dfu.reset().map_err(usb::error)?;
    drop(dfu);
    device = wait_for(&original, Mode::Dfu, &deadline)?;
    if device.identity.pwned.as_deref() != Some("gaster") {
        return Err(invalid("the device lost its gaster marker after reset"));
    }

    say("Sending patched iBSS");
    let mut dfu = Dfu::open(&device).map_err(usb::error)?;
    dfu.send(ibss, deadline.step(Duration::from_secs(60))?)
        .map_err(usb::error)?;
    drop(dfu);

    say("Waiting for iBSS recovery");
    device = wait_for(&original, Mode::Recovery, &deadline)?;
    // Recovery may enumerate before macOS releases its exclusive handle.
    let opening = Deadline::after(deadline.step(Duration::from_secs(10))?);
    let mut recovery = loop {
        match Recovery::open(&device) {
            Ok(recovery) => break recovery,
            Err(error)
                if error.kind == orchard_usb::Kind::Busy || error.detail.contains("0xe00002c5") =>
            {
                // IOKit exclusive-access errors can arrive as Kind::Other.
                if opening.left().is_err() {
                    return Err(usb::error(error).hint(
                        "recovery is still held by macOS or another program; close device-management apps before the next attempt",
                    ));
                }
                std::thread::sleep(
                    Duration::from_millis(200).min(opening.left().unwrap_or_default()),
                );
            }
            Err(error) => return Err(usb::error(error)),
        }
    };

    say("Sending the Apple device tree");
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

    say("Sending bare m1n1 as the restore kernel");
    recovery
        .send(
            &restore.kernelcache,
            deadline.step(Duration::from_secs(120))?,
        )
        .map_err(usb::error)?;
    say("Starting m1n1: bootx");
    recovery
        .command("bootx", deadline.step(Duration::from_secs(10))?)
        .map_err(usb::error)?;
    drop(recovery);

    say("Waiting for the m1n1 proxy");
    device = wait_for(&original, Mode::M1n1, &deadline)?;
    // The USB device appears before macOS finishes attaching its CDC driver.
    say("Opening the m1n1 proxy transport");
    let mut proxy = loop {
        match Proxy::open(&device) {
            Ok(proxy) => break proxy,
            Err(error) if error.kind == orchard_usb::Kind::Disconnected => {
                deadline
                    .sleep(Duration::from_millis(200))
                    .context("waiting for the m1n1 serial port")?;
                device = wait_for(&original, Mode::M1n1, &deadline)?;
            }
            Err(error) => return Err(usb::error(error)),
        }
    };
    proxy.ping(deadline.left()?).map_err(usb::error)?;
    say(if proxy.uses_bulk() {
        "m1n1 proxy connected over direct USB bulk"
    } else {
        "m1n1 proxy connected over USB serial"
    });

    let proxy_ready_seconds = device_started.elapsed().as_secs_f64();

    // Match proxyclient/tools/linux.py: leave 128 MiB for m1n1, then use
    // a separate 1 GiB host-managed heap. Reserve 512 MiB for the kernel
    // so decompression cannot overwrite the DTB or initramfs.
    let heap = call(&mut proxy, &deadline, HEAPBLOCK_ALLOC, &[0])? + (128 << 20);
    call(&mut proxy, &deadline, HEAPBLOCK_SET_LIMIT, &[heap])?;
    let mut next = heap;
    let mut allocate = |alignment: u64, size: usize| -> Result<u64> {
        let address = next.next_multiple_of(alignment);
        next = address
            .checked_add(size as u64)
            .filter(|end| *end <= heap + (1 << 30))
            .ok_or_else(|| invalid("Linux files exceed the 1 GiB proxy upload area"))?;
        Ok(address)
    };
    let kernel_capacity = 512 << 20;
    if kernel.len() > kernel_capacity {
        return Err(invalid("kernel exceeds 512 MiB"));
    }
    let kernel_address = allocate(2 << 20, kernel_capacity)?;
    let upload_address = if compressed {
        allocate(64, kernel.len())?
    } else {
        kernel_address
    };
    let dtb_address = allocate(64, dtb.len())?;
    let initramfs_address = allocate(65536, initramfs.len())?;

    let upload_started = Instant::now();
    say("Uploading the kernel through m1n1");
    proxy
        .write(upload_address, &kernel, deadline.left()?)
        .map_err(usb::error)?;
    say("Uploading the Linux device tree");
    proxy
        .write(dtb_address, &dtb, deadline.left()?)
        .map_err(usb::error)?;
    say("Uploading the initramfs");
    proxy
        .write(initramfs_address, &initramfs, deadline.left()?)
        .map_err(usb::error)?;
    let upload_seconds = upload_started.elapsed().as_secs_f64();
    call(
        &mut proxy,
        &deadline,
        KBOOT_SET_INITRD,
        &[initramfs_address, initramfs.len() as u64],
    )?;

    if let Some(bootargs) = &args.bootargs {
        let name = b"bootargs\0";
        let value = [bootargs.as_bytes(), &[0]].concat();
        let name_address = allocate(8, name.len())?;
        let value_address = allocate(8, value.len())?;
        proxy
            .write(name_address, name, deadline.left()?)
            .map_err(usb::error)?;
        proxy
            .write(value_address, &value, deadline.left()?)
            .map_err(usb::error)?;
        if (call(
            &mut proxy,
            &deadline,
            KBOOT_SET_CHOSEN,
            &[name_address, value_address],
        )? as i64)
            < 0
        {
            return Err(invalid("m1n1 could not set the Linux boot arguments"));
        }
    }

    say("Preparing Linux in m1n1");
    call(&mut proxy, &deadline, CPUFREQ_INIT, &[])?;
    call(&mut proxy, &deadline, SMP_START_SECONDARIES, &[])?;
    if call(&mut proxy, &deadline, KBOOT_PREPARE_DT, &[dtb_address])? != 0 {
        return Err(Error::new(
            Code::Usb,
            "m1n1 could not prepare the Linux device tree",
        ));
    }
    let kernel_size = if compressed {
        say("Decompressing the kernel in m1n1");
        let size = call(
            &mut proxy,
            &deadline,
            GZDEC,
            &[
                upload_address,
                kernel.len() as u64,
                kernel_address,
                kernel_capacity as u64,
            ],
        )?;
        if size == 0 || size > kernel_capacity as u64 {
            return Err(invalid("m1n1 could not decompress the kernel"));
        }
        size
    } else {
        kernel.len() as u64
    };
    call(
        &mut proxy,
        &deadline,
        DC_CVAU,
        &[kernel_address, kernel_size],
    )?;
    call(
        &mut proxy,
        &deadline,
        IC_IVAU,
        &[kernel_address, kernel_size],
    )?;
    let linux_started = Instant::now();
    say("Starting Linux");
    call(&mut proxy, &deadline, KBOOT_BOOT, &[kernel_address])?;
    drop(proxy);

    say("Waiting for Linux");
    let linux = wait_for(&original, Mode::Linux, &deadline)?;
    Ok(Booted {
        location: linux.location,
        boot_firmware,
        already_pwned,
        total_seconds: started.elapsed().as_secs_f64(),
        device_seconds: device_started.elapsed().as_secs_f64(),
        proxy_ready_seconds,
        upload_seconds,
        linux_start_seconds: linux_started.elapsed().as_secs_f64(),
    }
    .into())
}

// Helpers below do file/byte work or one bounded device observation.

// Stable m1n1 proxy opcodes from src/proxy.h.
const IC_IVAU: u64 = 0x302;
const DC_CVAU: u64 = 0x309;
const GZDEC: u64 = 0x401;
const SMP_START_SECONDARIES: u64 = 0x500;
const HEAPBLOCK_ALLOC: u64 = 0x600;
const HEAPBLOCK_SET_LIMIT: u64 = 0x604;
const KBOOT_BOOT: u64 = 0x700;
const KBOOT_SET_CHOSEN: u64 = 0x701;
const KBOOT_SET_INITRD: u64 = 0x702;
const KBOOT_PREPARE_DT: u64 = 0x703;
const CPUFREQ_INIT: u64 = 0x1300;

fn call(proxy: &mut Proxy, deadline: &Deadline, operation: u64, args: &[u64]) -> Result<u64> {
    proxy
        .call(operation, args, deadline.left()?)
        .map_err(usb::error)
}

fn find_j71b(global: &Global, board: &orchard_boards::catalog::Board) -> Result<Device> {
    let devices = orchard_usb::list().map_err(usb::error)?;
    let candidates: Vec<_> = devices
        .into_iter()
        .filter(|device| {
            device.mode == Mode::Dfu
                && hex(device.identity.chip.as_deref()) == Some(u64::from(board.chip))
                && hex(device.identity.board_id.as_deref()) == Some(u64::from(board.board))
                && global
                    .device
                    .location
                    .as_ref()
                    .is_none_or(|p| p == &device.location)
                && global.device.ecid.as_deref().is_none_or(|id| {
                    hex(Some(id)).is_some() && hex(Some(id)) == hex(device.identity.ecid.as_deref())
                })
        })
        .collect();
    match candidates.as_slice() {
        [device] if device.identity.ecid.is_some() => Ok(device.clone()),
        [] => Err(Error::new(Code::NoDevice, "no matching J71b in DFU mode")),
        [_] => Err(invalid("the J71b does not report its ECID")),
        _ => Err(Error::new(
            Code::Ambiguous,
            "several J71b devices; select --ecid or --location",
        )),
    }
}

fn wait_for(original: &Device, mode: Mode, deadline: &Deadline) -> Result<Device> {
    loop {
        deadline.left().context(format!(
            "waiting for {} at {}",
            mode.as_str(),
            original.location
        ))?;
        for device in orchard_usb::list().map_err(usb::error)? {
            if device.location != original.location {
                continue;
            }
            for (before, now) in [
                (&original.identity.ecid, &device.identity.ecid),
                (&original.identity.chip, &device.identity.chip),
                (&original.identity.board_id, &device.identity.board_id),
            ] {
                if before.is_some()
                    && now.is_some()
                    && hex(before.as_deref()) != hex(now.as_deref())
                {
                    return Err(Error::new(
                        Code::DeviceChanged,
                        "another device appeared on the selected port",
                    ));
                }
            }
            if device.mode == mode {
                return Ok(device);
            }
        }
        deadline.sleep(Duration::from_millis(100))?;
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

fn hex(text: Option<&str>) -> Option<u64> {
    let text = text?;
    u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).context(path.display())
}

fn invalid(message: &str) -> Error {
    Error::new(Code::InvalidInput, message)
}

#[derive(Serialize)]
struct Booted {
    location: String,
    boot_firmware: String,
    already_pwned: bool,
    total_seconds: f64,
    device_seconds: f64,
    proxy_ready_seconds: f64,
    upload_seconds: f64,
    linux_start_seconds: f64,
}

impl Report for Booted {
    fn human(&self) -> Vec<u8> {
        format!(
            "Linux is running in RAM on J71b at {}\n\
             Apple boot firmware: {} (already pwned: {})\n\
             Total command: {:.3}s; device work to Linux USB: {:.3}s\n\
             Device work to proxy ready: {:.3}s; Linux uploads: {:.3}s; kernel start to Linux USB: {:.3}s\n",
            self.location, self.boot_firmware, self.already_pwned,
            self.total_seconds, self.device_seconds, self.proxy_ready_seconds,
            self.upload_seconds, self.linux_start_seconds,
        ).into_bytes()
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

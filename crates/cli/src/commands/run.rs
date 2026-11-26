//! A RAM boot: m1n1 on the device in DFU mode, alone or with a Linux it
//! starts at once, by the board's boot route.
//!
//! `orchard2 run --kernel Image.gz --dtb board.dtb --initramfs initramfs.cpio.gz`
//!
//! The kernel, device tree and initramfs go up in one upload behind m1n1
//! where they fit, and through m1n1's proxy where they are too large for
//! the route's upload or `--proxy` asks for it. Without a kernel, m1n1 is
//! started and left waiting on USB.
use crate::args::Global;
use crate::commands::build::{self, BuildArgs};
use crate::commands::install::BootFlags;
use crate::output::{self, Output, Report};
use orchard_boards::catalog::{Catalog, Kind};
use orchard_boards::data::firmware::Inputs;
use orchard_boards::{pack, peripheral};
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_device::host::M1n1Proxy;
use orchard_device::select::Selector;
use orchard_device::session::Session;
use orchard_device::{deadline::Deadline, usb};
use orchard_formats::{cpio, fdt, m1n1};
use orchard_runtime::driver::Options;
use orchard_usb::{Device, Host, Mode};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The most a patched iBoot is sent behind m1n1 in one upload: one took
/// 42 MiB and refused 48 MiB on a j71b. More goes through m1n1's proxy.
const IBOOT_UPLOAD: usize = 40 << 20;

/// The file a build leaves for `run`, in a directory given to it.
const SHORTCUT: &str = "run.toml";

#[derive(clap::Args)]
pub struct Args {
    /// The project to build and boot (the build's own options apply), or
    /// a run.toml or a directory with one: the options below, written down
    #[command(flatten)]
    build: BuildArgs,
    /// Boot what the project's last build left, without building
    #[arg(long, conflicts_with = "bare")]
    no_build: bool,
    /// Start m1n1 alone and leave it waiting on USB
    #[arg(long, conflicts_with_all = ["kernel", "dtb", "initramfs", "bootargs", "firmware", "proxy"])]
    bare: bool,
    /// Linux kernel image, instead of a project's: with --dtb and
    /// --initramfs, nothing is built or read from a file
    #[arg(long)]
    kernel: Option<PathBuf>,
    /// A Linux device tree; repeat for a kernel of several boards (m1n1
    /// takes the board's)
    #[arg(long)]
    dtb: Vec<PathBuf>,
    /// RAM filesystem archive
    #[arg(long)]
    initramfs: Option<PathBuf>,
    /// Linux command line
    #[arg(long)]
    bootargs: Option<String>,
    /// Append the board's peripheral firmware to the initramfs
    #[arg(long)]
    firmware: bool,
    /// A local m1n1 instead of Orchard's published one: the plain build for
    /// the palera1n and checkra1n routes, the Mach-O build for ibss (the
    /// 32-bit one on a 32-bit board, where none is published yet)
    #[arg(long)]
    m1n1: Option<PathBuf>,
    /// Firmware key file for the ibss route, instead of the cache and the
    /// device (also ORCHARD_FIRMWARE_KEYS)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Upload the kernel through m1n1's proxy even where one upload would do
    #[arg(long)]
    proxy: bool,
    /// The boot route from DFU mode (palera1n, checkra1n, ibss, autogo); otherwise
    /// ORCHARD_BOOT, else the first that fits the device
    #[arg(long, value_name = "ROUTE")]
    boot: Option<String>,
    #[command(flatten)]
    short: BootFlags,
    /// End once the start has gone out, without waiting for m1n1 or Linux
    /// to show up on USB: for a system that never does
    #[arg(long)]
    no_wait: bool,
    /// Seconds to wait for the device in DFU mode
    #[arg(long, default_value = "0")]
    wait: u64,
    /// Total seconds for device work, after the files have been prepared
    #[arg(long, default_value = "300", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
}

/// `run.toml`: `run`'s own options for a Linux, written down. Paths count
/// from the file's directory.
#[derive(Debug, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Shortcut {
    kernel: Option<PathBuf>,
    #[serde(default)]
    dtb: Vec<PathBuf>,
    initramfs: Option<PathBuf>,
    bootargs: Option<String>,
    #[serde(default)]
    firmware: bool,
    /// Cascadia's loader, for the autogo route.
    loader: Option<PathBuf>,
}

/// The Linux a run is to boot, where it boots one: what the options name,
/// and for what they leave out, what the file does.
#[derive(Debug, PartialEq)]
struct Wanted {
    kernel: PathBuf,
    dtbs: Vec<PathBuf>,
    initramfs: PathBuf,
    bootargs: Option<String>,
    firmware: bool,
    loader: Option<PathBuf>,
}

fn wanted(args: &Args, file: Option<(Shortcut, &Path)>) -> Result<Option<Wanted>> {
    let (file, base) = file.map(|(file, path)| (file, path.parent().unwrap_or(Path::new(".")).to_owned())).unwrap_or_default();
    let from_file = |path: PathBuf| base.join(path);
    let kernel = args.kernel.clone().or(file.kernel.map(from_file));
    let dtbs = match args.dtb.is_empty() {
        true => file.dtb.into_iter().map(from_file).collect(),
        false => args.dtb.clone(),
    };
    let initramfs = args.initramfs.clone().or(file.initramfs.map(from_file));
    let bootargs = args.bootargs.clone().or(file.bootargs);
    let firmware = args.firmware || file.firmware;
    let loader = args.build.loader().map(Path::to_owned).or(file.loader.map(from_file));
    let Some(kernel) = kernel else {
        return match !dtbs.is_empty() || initramfs.is_some() || bootargs.is_some() || firmware || args.proxy {
            true => Err(invalid("a device tree, an initramfs, boot arguments, --firmware and --proxy are for a kernel: give --kernel")),
            false => Ok(None),
        };
    };
    let Some(initramfs) = initramfs else {
        return Err(invalid("a kernel needs an initramfs: give --initramfs"));
    };
    if dtbs.is_empty() {
        return Err(invalid("a kernel needs the board's device tree: give --dtb"));
    }
    Ok(Some(Wanted { kernel, dtbs, initramfs, bootargs, firmware, loader }))
}

/// The run.toml `source` stands for: the file itself, the one in the
/// directory, or the one a build for `platform` left in the project.
fn shortcut(source: &Path, platform: Option<&str>) -> Result<PathBuf> {
    if source.is_file() {
        return Ok(source.to_owned());
    }
    let direct = source.join(SHORTCUT);
    if direct.is_file() {
        return Ok(direct);
    }
    let built = platform.map(|platform| source.join("out").join(platform).join(SHORTCUT));
    match built {
        Some(built) if built.is_file() => Ok(built),
        Some(built) => Err(Error::new(Code::NotFound, format!("no build for this board in {}", source.display())).hint(format!("orchard2 build makes {}", built.display()))),
        None => Err(Error::new(Code::NotFound, format!("no {SHORTCUT} in {}", source.display()))),
    }
}

pub fn run(global: &Global, args: &Args) -> Result<Output> {
    let started = Instant::now();
    let say = |message: &str| output::progress(global, &format!("[{:.3}s] {message}", started.elapsed().as_secs_f64()));

    // What to boot: m1n1 alone, the files the options name, a run.toml, or
    // the project, built first. The board's platform names the file of a
    // build that is taken as it is.
    let directory = args.build.directory();
    let explicit = args.bare || args.kernel.is_some();
    if explicit {
        wanted(args, None)?;
    }
    // A distribution checkout's release: its installer system, from RAM.
    let release = match explicit || directory.is_file() {
        true => None,
        false => build::release(global, &args.build, !args.no_build)?,
    };
    if release.is_some() && (args.m1n1.is_some() || args.proxy || args.firmware || args.bootargs.is_some() || !args.dtb.is_empty() || args.initramfs.is_some()) {
        return Err(invalid("a distribution checkout's release is booted as it is built: --m1n1, --proxy, --firmware, --bootargs, --dtb and --initramfs are for a kernel of your own"));
    }
    let file_at = match () {
        _ if explicit || release.is_some() => None,
        _ if directory.is_file() => Some(directory.to_owned()),
        _ if directory.join(SHORTCUT).is_file() => Some(directory.join(SHORTCUT)),
        _ if args.no_build => None,
        _ => {
            say("Building");
            Some(build::payload(global, &args.build)?.join(SHORTCUT))
        }
    };
    let local_m1n1 = args.m1n1.as_deref().map(read).transpose()?;

    let store = Store::open(global.cache.as_deref())?;
    let data = global.data_dir()?;
    let catalog = Catalog::open(&data)?;

    // The one device in DFU mode, and the board its chip and board numbers name.
    // A session follows it from here on, at its port, with its lock.
    let found = find(global, Duration::from_secs(args.wait))?;
    let selector = Selector { ecid: found.identity.ecid.clone(), location: Some(found.location.clone()), mode: Some(Mode::Dfu) };
    let mut session = Session::start(Host, &selector, None)?;
    let original = session.device().clone();
    let seen = orchard_runtime::identify(&catalog, &original)
        .filter(|seen| !seen.device.board.is_empty())
        .ok_or_else(|| Error::new(Code::Unsupported, "the device in DFU mode is no board the data knows"))?;
    let device = seen.device;
    let board = catalog.board(&device.board)?;
    say(&format!("{} at {}", device.name, original.location));

    // The runtime's routes, as install and launch take them.
    let (events, log) = std::sync::mpsc::channel();
    let mut options = Options::new(orchard_flow::App::Launcher);
    options.data = Some(data.clone());
    if let Some(cache) = &global.cache {
        options.cache = cache.clone();
    }
    options.boot = args.boot.clone().or(args.short.route().map(str::to_owned));
    options.index = release.as_ref().map(|index| format!("file://{}", index.display()));
    let executor = options.executor(events)?;
    executor.check_boot()?;
    let route = executor.boot_route(&device.board)?;
    if release.is_some() {
        let name = executor.release()?.name;
        say(&format!("Starting {name}'s installer system by the {} route; nothing is installed", route.name));
        let deadline = Deadline::after(Duration::from_secs(args.timeout));
        let device_started = Instant::now();
        orchard_runtime::chain::boot_install_env_in(&executor, &mut session, &device)?;
        drop(executor);
        drop(log);
        let reached = match args.no_wait {
            true => "The start has gone out; not waited for",
            false => {
                say("Waiting for the installer system");
                session.wait_for(Mode::Linux, deadline.left()?)?;
                "The release's installer system is running in RAM; nothing is installed"
            }
        };
        drop(session);
        return Ok(Booted {
            reached,
            board: board.name,
            location: original.location,
            route: route.name,
            through_proxy: false,
            total_seconds: started.elapsed().as_secs_f64(),
            device_seconds: device_started.elapsed().as_secs_f64(),
        }
        .into());
    }
    // Routes end in m1n1, or in Cascadia's loader, which boots Linux itself.
    let autogo = route.ends.as_deref() == Some("cascadia");
    if !autogo && route.ends.as_deref() != Some("m1n1") {
        return Err(Error::new(Code::Unsupported, format!("run starts m1n1 or Cascadia's loader; the {} route ends elsewhere", route.name)));
    }
    if autogo && (local_m1n1.is_some() || args.proxy || args.bare) {
        return Err(invalid("this route starts Linux with Cascadia's loader: --m1n1, --proxy and --bare are for the routes that end in m1n1"));
    }
    let ibss = route.name == "ibss";
    // The 32-bit chips take a 32-bit m1n1 and a zImage.
    let arm = catalog.chip(board.chip)?.architecture == "arm";
    if let Some(m1n1) = &local_m1n1 {
        let fits = match (ibss, arm) {
            (true, true) => m1n1::is_macho_arm(m1n1),
            (true, false) => m1n1::is_macho(m1n1),
            (false, _) => !m1n1::is_macho(m1n1) && !m1n1::is_macho_arm(m1n1),
        };
        if !fits {
            return Err(invalid(match (ibss, arm) {
                (true, true) => "this board takes m1n1's 32-bit Mach-O build (m1n1-armv7.macho)",
                (true, false) => "the ibss route takes m1n1's Mach-O build (m1n1-idevice.macho)",
                (false, _) => "this route takes m1n1's plain build (m1n1.bin)",
            }));
        }
    }

    // The Linux to boot: the options, and the build's file where one is named.
    let file_at = match file_at {
        None if !explicit && release.is_none() => Some(shortcut(directory, board.platform.as_deref())?),
        at => at,
    };
    let file = match file_at {
        Some(path) => {
            let text = std::fs::read_to_string(&path).context(path.display())?;
            let file: Shortcut = toml::from_str(&text).map_err(|e| Error::new(Code::Format, e.message().to_owned())).context(path.display())?;
            Some((file, path))
        }
        None => None,
    };
    let wanted = wanted(args, file.as_ref().map(|(file, path)| (Shortcut { kernel: file.kernel.clone(), dtb: file.dtb.clone(), initramfs: file.initramfs.clone(), bootargs: file.bootargs.clone(), firmware: file.firmware, loader: file.loader.clone() }, path.as_path())))?;
    let kernel = wanted.as_ref().map(|w| read(&w.kernel)).transpose()?;
    let dtbs = wanted.as_ref().map_or(Ok(Vec::new()), |w| w.dtbs.iter().map(|path| read(path)).collect::<Result<Vec<_>>>())?;
    let mut initramfs = wanted.as_ref().map(|w| read(&w.initramfs)).transpose()?.unwrap_or_default();
    let bootargs = wanted.as_ref().and_then(|w| w.bootargs.clone());
    for dtb in &dtbs {
        fdt::Tree::read(dtb)?;
    }
    let compressed = kernel.as_ref().is_some_and(|k| k.starts_with(&[0x1f, 0x8b]));
    let right_kernel = |k: &Vec<u8>| match arm {
        true => k.get(0x24..0x28) == Some(&0x016f_2818u32.to_le_bytes()[..]),
        false => compressed || k.get(56..60) == Some(b"ARM\x64".as_slice()),
    };
    if kernel.as_ref().is_some_and(|k| !right_kernel(k)) {
        return Err(invalid(match arm {
            true => "the kernel must be a 32-bit ARM zImage for this board",
            false => "kernel must be an ARM64 Image or gzip-compressed Image",
        }));
    }
    if arm && args.proxy {
        return Err(Error::new(Code::Unsupported, "the kernel goes behind m1n1 on the 32-bit boards; --proxy is not for them yet"));
    }
    if bootargs.as_deref().is_some_and(|s| s.contains('\0')) {
        return Err(invalid("boot arguments contain a NUL byte"));
    }
    // Through the proxy m1n1 is given one tree: the board's, by its name.
    let own_dtb = || -> Result<&Vec<u8>> {
        let named = wanted.as_ref().and_then(|w| w.dtbs.iter().position(|path| path.file_stem().and_then(|s| s.to_str()) == board.dtb.as_deref()));
        match (named, dtbs.as_slice()) {
            (Some(at), _) => Ok(&dtbs[at]),
            (None, [only]) => Ok(only),
            _ => Err(invalid("none of the device trees is named as the board's; give its --dtb alone")),
        }
    };

    // Optionally append board firmware as a second initramfs archive.
    // The temporary directory is removed as soon as its files are packed.
    if wanted.as_ref().is_some_and(|w| w.firmware) {
        say("Preparing the board's peripheral firmware");
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


    // What m1n1 is to boot goes behind it where the route's upload takes
    // that much, else through its proxy once it runs.
    let linux = kernel.as_ref();
    let size = linux.map_or(0, |kernel| kernel.len() + dtbs.iter().map(Vec::len).sum::<usize>() + initramfs.len());
    // What a 32-bit iBEC takes in one upload has not been measured.
    let through_proxy = linux.is_some() && (args.proxy || (ibss && !arm && size > IBOOT_UPLOAD));
    let image = |m1n1: &[u8]| -> Result<Vec<u8>> {
        let m1n1 = local_m1n1.as_deref().unwrap_or(m1n1);
        match linux {
            Some(kernel) if !through_proxy => {
                let dtbs: Vec<&[u8]> = dtbs.iter().map(Vec::as_slice).collect();
                let linux = pack::Linux { kernel, dtbs: &dtbs, initramfs: Some(&initramfs), bootargs: bootargs.as_deref(), logo: None };
                pack::m1n1(m1n1, Some(&linux))
            }
            _ => Ok(m1n1.to_vec()),
        }
    };

    // Cascadia's bundle: its loader, the kernel, the board's tree and the
    // initramfs, at the board's addresses.
    let bundle = match autogo {
        false => None,
        true => {
            let Some(kernel) = linux else {
                return Err(invalid("this route boots a Linux: give --kernel, --dtb and --initramfs, or a built project"));
            };
            let loader = match wanted.as_ref().and_then(|w| w.loader.as_ref()) {
                Some(path) => read(path)?,
                None => {
                    say("Getting Cascadia's loader");
                    let published = orchard_runtime::orchard_tools::published(&format!("cascadia-{}.bin", board.name))?;
                    std::fs::read(executor.download(&published)?)?
                }
            };
            // The loader takes the command line from the tree's /chosen.
            let mut dtb = own_dtb()?.clone();
            if let Some(bootargs) = &bootargs {
                let mut tree = fdt::Tree::read(&dtb)?;
                let chosen = tree.root.children.iter_mut().find(|node| node.name == "chosen").ok_or_else(|| invalid("the device tree has no /chosen for the boot arguments"))?;
                chosen.set("bootargs", [bootargs.as_bytes(), b"\0"].concat());
                dtb = tree.write();
            }
            Some(orchard_formats::cascadia::bundle(&loader, kernel, &dtb, &initramfs, &board.cascadia()?, original.identity.serial_number.as_deref())?)
        }
    };

    say(&format!("Starting {} by the {} route", if autogo { "Linux" } else { "m1n1" }, route.name));
    let deadline = Deadline::after(Duration::from_secs(args.timeout));
    let device_started = Instant::now();
    if let Some(bundle) = &bundle {
        orchard_runtime::autogo::start(&executor, &mut session, &device.board, args.keys.as_deref(), bundle)?;
    } else if ibss {
        orchard_runtime::ibss::start(&executor, &mut session, &device.board, args.keys.as_deref(), image)?;
    } else {
        let published = match &local_m1n1 {
            Some(_) => Vec::new(),
            None => std::fs::read(executor.download(&executor.orchard_tools()?.m1n1)?)?,
        };
        orchard_runtime::chain::start_image(&executor, &mut session, &device.board, &image(&published)?)?;
    }
    drop(executor);
    drop(log);

    let reached = match (linux, through_proxy) {
        // Through the proxy the start is only out once m1n1 has been talked to.
        (None, _) | (Some(_), false) if args.no_wait => "The start has gone out; not waited for",
        (None, _) => {
            say("Waiting for m1n1");
            session.wait_for(Mode::M1n1, deadline.left()?)?;
            "m1n1 is waiting on USB"
        }
        (Some(_), false) => {
            say("Waiting for Linux");
            session.wait_for(Mode::Linux, deadline.left()?)?;
            "Linux is running in RAM"
        }
        (Some(kernel), true) => {
            upload(&mut session, &deadline, &say, kernel, compressed, own_dtb()?, &initramfs, bootargs.as_deref())?;
            match args.no_wait {
                true => "The start has gone out; not waited for",
                false => {
                    say("Waiting for Linux");
                    session.wait_for(Mode::Linux, deadline.left()?)?;
                    "Linux is running in RAM"
                }
            }
        }
    };
    drop(session);
    Ok(Booted {
        reached,
        board: board.name,
        location: original.location,
        route: route.name,
        through_proxy,
        total_seconds: started.elapsed().as_secs_f64(),
        device_seconds: device_started.elapsed().as_secs_f64(),
    }
    .into())
}

/// The kernel, device tree and initramfs through m1n1's proxy, and the
/// start of Linux from there: for what is too large for the route's upload.
#[allow(clippy::too_many_arguments)]
fn upload(
    session: &mut Session<Host>,
    deadline: &Deadline,
    say: &dyn Fn(&str),
    kernel: &[u8],
    compressed: bool,
    dtb: &[u8],
    initramfs: &[u8],
    bootargs: Option<&str>,
) -> Result<()> {
    say("Waiting for the m1n1 proxy");
    session.wait_for(Mode::M1n1, deadline.left()?)?;
    // The USB device appears before the system has attached its serial driver.
    let mut proxy = loop {
        match session.m1n1() {
            Ok(proxy) => break proxy,
            Err(error) if error.code == Code::Usb => {
                deadline.sleep(Duration::from_millis(200)).context("waiting for the m1n1 serial port")?;
                session.wait_for(Mode::M1n1, deadline.left()?)?;
            }
            Err(error) => return Err(error),
        }
    };
    proxy.ping(deadline.left()?).map_err(usb::error)?;

    // Match proxyclient/tools/linux.py: leave 128 MiB for m1n1, then use
    // a separate 1 GiB host-managed heap. Reserve 512 MiB for the kernel
    // so decompression cannot overwrite the DTB or initramfs.
    let heap = call(&mut proxy, deadline, HEAPBLOCK_ALLOC, &[0])? + (128 << 20);
    call(&mut proxy, deadline, HEAPBLOCK_SET_LIMIT, &[heap])?;
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
    call(
        &mut proxy,
        deadline,
        KBOOT_SET_INITRD,
        &[initramfs_address, initramfs.len() as u64],
    )?;

    if let Some(bootargs) = &bootargs {
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
            deadline,
            KBOOT_SET_CHOSEN,
            &[name_address, value_address],
        )? as i64)
            < 0
        {
            return Err(invalid("m1n1 could not set the Linux boot arguments"));
        }
    }

    say("Preparing Linux in m1n1");
    call(&mut proxy, deadline, CPUFREQ_INIT, &[])?;
    call(&mut proxy, deadline, SMP_START_SECONDARIES, &[])?;
    if call(&mut proxy, deadline, KBOOT_PREPARE_DT, &[dtb_address])? != 0 {
        return Err(Error::new(
            Code::Usb,
            "m1n1 could not prepare the Linux device tree",
        ));
    }
    let kernel_size = if compressed {
        say("Decompressing the kernel in m1n1");
        let size = call(
            &mut proxy,
            deadline,
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
        deadline,
        DC_CVAU,
        &[kernel_address, kernel_size],
    )?;
    call(
        &mut proxy,
        deadline,
        IC_IVAU,
        &[kernel_address, kernel_size],
    )?;
    say("Starting Linux");
    call(&mut proxy, deadline, KBOOT_BOOT, &[kernel_address])?;
    Ok(())
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

fn call(proxy: &mut Box<dyn M1n1Proxy>, deadline: &Deadline, operation: u64, args: &[u64]) -> Result<u64> {
    proxy
        .call(operation, args, deadline.left()?)
        .map_err(usb::error)
}

/// The one device in DFU mode that --ecid and --location leave, waited for
/// up to `wait`.
pub(crate) fn find(global: &Global, wait: Duration) -> Result<Device> {
    let until = Instant::now() + wait;
    loop {
        let devices = orchard_usb::list().map_err(usb::error)?;
        let candidates: Vec<_> = devices
            .into_iter()
            .filter(|device| {
                device.mode == Mode::Dfu
                    && global.device.location.as_ref().is_none_or(|p| p == &device.location)
                    && global.device.ecid.as_deref().is_none_or(|id| hex(Some(id)).is_some() && hex(Some(id)) == hex(device.identity.ecid.as_deref()))
            })
            .collect();
        match candidates.as_slice() {
            [device] if device.identity.ecid.is_some() => return Ok(device.clone()),
            [_] => return Err(invalid("the device in DFU mode does not report its ECID")),
            [] if Instant::now() < until => std::thread::sleep(Duration::from_millis(250)),
            [] => return Err(Error::new(Code::NoDevice, "no device in DFU mode").hint("put the device in DFU mode; --wait SECONDS waits for it")),
            _ => return Err(Error::new(Code::Ambiguous, "several devices in DFU mode; select --ecid or --location")),
        }
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
    reached: &'static str,
    board: String,
    location: String,
    route: String,
    through_proxy: bool,
    total_seconds: f64,
    device_seconds: f64,
}

impl Report for Booted {
    fn human(&self) -> Vec<u8> {
        format!(
            "{} on {} at {}, by the {} route{}\nTotal: {:.1} s; device work: {:.1} s\n",
            self.reached,
            self.board,
            self.location,
            self.route,
            if self.through_proxy { ", the kernel through m1n1's proxy" } else { "" },
            self.total_seconds,
            self.device_seconds,
        )
        .into_bytes()
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

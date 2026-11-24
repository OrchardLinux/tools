//! Boots the install system from DFU: `palera1n -p -k pongo.bin`, then the
//! install payload to Pongo and `bootm` (orchard's `boot_payload`). The
//! install system then shows up on USB by itself, and the watcher opens it.
//!
//! Continuity: the one DFU device must be the user's device (ECID), the one
//! Pongo that appears must carry the same ECID when it reports one, and the
//! install system's receiver checks the ECID again when it is opened.
use crate::exec::Executor;
use crate::ecid_key;
use orchard_boards::install_env;
use orchard_cache::{Code, Error, Result};
use orchard_flow::assets::palera1n_pin;
use orchard_flow::Device;
use orchard_usb::pongo::Pongo;
use orchard_usb::Mode;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PALERA1N_SECONDS: u64 = 120;
/// How long the exploit is tried again while the device stays in DFU mode.
/// On some computers it takes many attempts (seen up to 20 minutes).
const START_WINDOW: Duration = Duration::from_secs(20 * 60);
/// After a failed attempt: how long to look where the device went.
const SETTLE_SECONDS: u64 = 15;
const PONGO_SECONDS: u64 = 30;
/// A full boot uploads the patchfinder, ramdisk and overlay before iPadOS starts.
const IPADOS_SECONDS: u64 = 240;

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::NotFound, message.into())
}

/// The listed devices in `mode` that are `device`, refusing anything that
/// could be another device.
fn the_device(device: &Device, mode: Mode) -> Result<Option<orchard_usb::Device>> {
    let listed = orchard_usb::list().map_err(|e| fail(e.to_string()))?;
    let found: Vec<_> = listed.into_iter().filter(|d| d.mode == mode).collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => match one.identity.ecid.as_deref() {
            Some(ecid) if ecid_key(ecid) != device.id.0 => Err(fail("another device is connected")),
            _ => Ok(Some(one.clone())),
        },
        _ => Err(fail("more than one device is connected")),
    }
}

/// The install payload for this device: m1n1, the layer's kernel, initrd and
/// device tree, and the install tools with the device's ECID.
pub fn payload(executor: &Executor, device: &Device) -> Result<Vec<u8>> {
    let rel = executor.release()?;
    let layer = &rel
        .platform_for(&device.board)
        .ok_or_else(|| fail(format!("{} does not cover {} yet", rel.name, device.board)))?
        .layer;
    let layer = std::fs::read(executor.download(layer)?)?;
    let tree = executor.devicetree(&device.board)?;
    let boot = install_env::boot_files(&layer, &format!("{tree}.dtb"))?;
    let ecid = u64::from_str_radix(&device.id.0, 16).map_err(|_| fail("the device's ECID is not hex"))?;
    let tools = std::fs::read(executor.download(&rel.install_tools)?)?;
    let add_on = install_env::add_on(&tools, &install_env::Device { ecid })?;
    let m1n1 = std::fs::read(executor.download(&executor.orchard_tools()?.m1n1)?)?;
    let logo = rel.logo.as_ref().map(|l| executor.download(l).and_then(|p| Ok(std::fs::read(p)?))).transpose()?;
    install_env::payload(&m1n1, logo.as_deref(), &boot, &add_on)
}

/// Runs palera1n on the device in DFU: checks it is the user's device
/// first, resets its USB port on Linux if it doesn't come back after
/// checkm8, and logs palera1n's output in the cache.
fn run_palera1n(executor: &Executor, device: &Device, args: &[std::ffi::OsString], seconds: u64) -> Result<()> {
    run_palera1n_from(executor, device, Mode::Dfu, args, seconds)
}

/// palera1n on the device in `from`: DFU (the exploit, with the port reset
/// on Linux), or Pongo (palera1n carries on from there).
fn run_palera1n_from(executor: &Executor, device: &Device, from: Mode, args: &[std::ffi::OsString], seconds: u64) -> Result<()> {
    let pin = palera1n_pin(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| fail("palera1n publishes no build for this computer"))?;
    let palera1n = match crate::flags::palera1n() {
        Some(path) => path,
        None => executor.palera1n(pin)?,
    };
    let Some(found) = the_device(device, from)? else {
        return Err(fail(if from == Mode::Dfu { "the device is no longer in DFU mode" } else { "the device is not in Pongo" }));
    };
    // Resolved while the device is present: its sysfs node goes away after checkm8.
    let control = (from == Mode::Dfu)
        .then(|| crate::replug::port_control(&found.location, std::path::Path::new("/sys/bus/usb/devices")))
        .flatten();
    let (log, log_path) = executor.log_file("palera1n")?;
    // ORCHARD_PORT_RESET_SECONDS=0: no reset.
    let reset_off = crate::flags::port_reset().is_some_and(|w| w.is_zero());
    let replug = control.filter(|_| !reset_off).map(|c| crate::replug::Watcher::start(log_path.clone(), c));
    let progress = Progress::start(log_path.clone(), executor.events(), replug.as_ref().map(|w| w.done.clone()));
    // In a pseudo-terminal (`script`), so palera1n writes each line at once
    // and its progress can be followed while it runs.
    let mut command = if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/script");
        c.arg("-q").arg("/dev/null").arg(&palera1n).args(args);
        c
    } else {
        let line = std::iter::once(palera1n.as_os_str().to_owned())
            .chain(args.iter().cloned())
            .map(|a| shell_quote(&a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        let mut c = Command::new("script");
        c.args(["-q", "-e", "-c", &line, "/dev/null"]);
        c
    };
    let child = command
        // palera1n 3 runs as a command only when told to (on macOS it is an app).
        .env("RA1N_CLI", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn();
    let finish = |replug: Option<crate::replug::Watcher>| {
        if let Some(resets) = replug.map(crate::replug::Watcher::finish).filter(|n| *n > 0) {
            eprintln!("chain: USB port reset {resets}x to bring the device back after checkm8");
        }
    };
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            finish(replug);
            progress.finish();
            return Err(e.into());
        }
    };
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut looked = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(fail("palera1n did not finish"));
            }
            // From DFU mode, iOS on USB means the device restarted normally:
            // this start can't work any more, whatever palera1n still waits for.
            Ok(None) if from == Mode::Dfu && looked.elapsed() >= Duration::from_secs(1) => {
                looked = Instant::now();
                if matches!(the_device(device, Mode::Normal), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(restarted(&log_path));
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => break Err(e.into()),
        }
    };
    finish(replug);
    progress.finish();
    let status = status?;
    if !status.success() {
        return Err(fail(last_error(&log_path).unwrap_or_else(|| format!("palera1n failed ({status})"))));
    }
    Ok(())
}

/// palera1n's last error line, without colors and timestamp: "palera1n:
/// Unable to perform checkm8 …".
fn last_error(log: &std::path::Path) -> Option<String> {
    let text = String::from_utf8_lossy(&std::fs::read(log).ok()?).into_owned();
    let line = text.lines().rev().find(|l| l.contains("<Error>"))?;
    let plain = strip_ansi(line);
    let message = plain.split("<Error>").nth(1)?.trim_start_matches(':').trim().to_owned();
    (!message.is_empty()).then(|| format!("palera1n: {message}"))
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

const RESTARTED: &str = "it restarted normally instead";

/// The device left DFU mode and started iOS normally.
fn restarted(log: &std::path::Path) -> Error {
    let why = last_error(log).map(|e| format!(" ({e})")).unwrap_or_default();
    fail(format!("{RESTARTED}{why}"))
}

/// palera1n's exploit into Pongo, then Pongo on USB. A failed attempt with
/// the device still (or again) in DFU mode is tried again, up to
/// START_WINDOW, and the flow hears of each new attempt. iOS on USB ends it:
/// the device restarted, and only DFU mode by hand starts over.
fn into_pongo(executor: &Executor, device: &Device, pongo_image: &std::path::Path) -> Result<orchard_usb::Device> {
    let until = Instant::now() + START_WINDOW;
    let mut attempt = 1u32;
    loop {
        let args = [std::ffi::OsString::from("-p"), "-k".into(), pongo_image.as_os_str().to_owned()];
        let failed = match run_palera1n(executor, device, &args, PALERA1N_SECONDS) {
            Ok(()) => {
                let deadline = Instant::now() + Duration::from_secs(PONGO_SECONDS);
                loop {
                    if let Some(usb) = the_device(device, Mode::Pongo)? {
                        return Ok(usb);
                    }
                    if the_device(device, Mode::Normal)?.is_some() {
                        return Err(fail(format!("{RESTARTED} of starting Pongo")));
                    }
                    if Instant::now() > deadline {
                        break fail("Pongo did not appear on USB");
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            Err(e) => e,
        };
        if failed.to_string().contains(RESTARTED) || Instant::now() > until {
            return Err(failed);
        }
        // Where did it go? Still or again in DFU mode: once more.
        let look = Instant::now() + Duration::from_secs(SETTLE_SECONDS);
        let again = loop {
            if the_device(device, Mode::Dfu)?.is_some() {
                break true;
            }
            if the_device(device, Mode::Normal)?.is_some() || Instant::now() > look {
                break false;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        if !again {
            let normal = the_device(device, Mode::Normal)?.is_some();
            return Err(if normal { fail(format!("{RESTARTED} ({failed})")) } else { failed });
        }
        attempt += 1;
        eprintln!("chain: exploit attempt {attempt} ({failed})");
        let _ = executor.events().send(orchard_flow::Event::StartAttempt(attempt));
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Follows palera1n's log while it runs and tells the flow: the stall after
/// checkm8 (the device must be replugged at the device), its end, and the
/// start of iPadOS's boot.
struct Progress {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Progress {
    /// Follows palera1n's log: the replug dialog when the device doesn't come
    /// back after checkm8 (the same reading and wait as the Linux port reset;
    /// with a port reset running, only once it gave up), and "Booting".
    fn start(log: std::path::PathBuf, events: std::sync::mpsc::Sender<orchard_flow::Event>, resetting: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Progress {
        use crate::replug::{dropped, returned};
        use orchard_flow::Event;
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let (mut wait, mut stalled, mut booting) = (None::<(Instant, Duration)>, false, false);
            while !flag.load(Ordering::Relaxed) {
                if wait.is_none() {
                    wait = dropped(&log, crate::replug::timing()).map(|settle| (Instant::now(), settle));
                }
                let back = returned(&log);
                let gave_up = resetting.as_ref().is_none_or(|done| done.load(Ordering::Relaxed));
                if !stalled && !back && gave_up && wait.is_some_and(|(at, settle)| at.elapsed() >= settle) {
                    stalled = true;
                    let _ = events.send(Event::BootStalled);
                }
                if stalled && back {
                    stalled = false;
                    let _ = events.send(Event::BootResumed);
                }
                if !booting && std::fs::read(&log).is_ok_and(|t| String::from_utf8_lossy(&t).contains("Booting...")) {
                    booting = true;
                    let _ = events.send(Event::BootBooting);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if stalled {
                let _ = events.send(Event::BootResumed);
            }
        });
        Progress { stop, thread }
    }

    fn finish(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

/// iPadOS with the companion's overlay, in two palera1n runs: the exploit
/// and Pongo (`-p`), then `-l -o overlay.dmg` from Pongo. In one run A9
/// devices stop in Pongo (a known palera1n quirk); from Pongo it goes on.
/// palera1n boots iPadOS by itself; the companion then shows up through
/// iPadOS's USB (normal mode).
pub fn boot_ipados(executor: &Executor, device: &Device) -> Result<()> {
    let overlay = executor.overlay()?;
    // Orchard's Pongo (or `--pongo`'s), as every other boot.
    let ours = executor.pongo_image()?;
    into_pongo(executor, device, &ours)?;
    let second = vec![std::ffi::OsString::from("-l"), "-o".into(), overlay.into_os_string(), "-k".into(), ours.into_os_string()];
    run_palera1n_from(executor, device, Mode::Pongo, &second, IPADOS_SECONDS)
}

/// The launcher's loader: palera1n → Pongo → the release's m1n1 → `bootm`,
/// then m1n1's proxy on USB. m1n1 reads the device's boot partition itself;
/// nothing else comes from the host (orchard's `orchard launch`).
pub fn start_m1n1(executor: &Executor, device: &Device) -> Result<orchard_usb::Device> {
    let tools = executor.orchard_tools()?;
    let pongo_image = executor.pongo_image()?;
    let m1n1 = std::fs::read(executor.download(&tools.m1n1)?)?;
    let usb = into_pongo(executor, device, &pongo_image)?;
    let mut pongo = Pongo::open(&usb).map_err(|e| fail(e.to_string()))?;
    pongo.send(&m1n1, Duration::from_secs(60)).map_err(|e| fail(e.to_string()))?;
    let _ = pongo.command("bootm", Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(usb) = the_device(device, Mode::M1n1)? {
            return Ok(usb);
        }
        if Instant::now() > deadline {
            return Err(fail("m1n1 did not appear on USB"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// palera1n → Pongo → payload → `bootm`. Never retried by itself: a failure
/// goes back to the flow, which asks for DFU again.
pub fn boot_install_env(executor: &Executor, device: &Device) -> Result<()> {
    let payload = payload(executor, device)?;
    let pongo_image = executor.pongo_image()?;
    let usb = into_pongo(executor, device, &pongo_image)?;
    let mut pongo = Pongo::open(&usb).map_err(|e| fail(e.to_string()))?;
    pongo.send(&payload, Duration::from_secs(60)).map_err(|e| fail(e.to_string()))?;
    // bootm starts the payload; Pongo may be gone before it answers, and the
    // install system appearing is what confirms it.
    pongo.command("bootm", Duration::from_secs(10)).map_err(|e| fail(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod palera1n_errors {
    use super::*;

    #[test]
    fn the_last_error_line_without_colors() {
        let dir = std::env::temp_dir().join(format!("orchard-chain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("palera1n.log");
        // palera1n 3.0 beta, colored.
        std::fs::write(&log, " \x1b[38;5;47m[2026/11/25 00:12:08]\x1b[0m  \x1b[38;5;196m\x1b[1m<Error>\x1b[22m\x1b[0m STAGE_YOLODFU failed (rv=0, reason=)\n").unwrap();
        assert_eq!(last_error(&log).as_deref(), Some("palera1n: STAGE_YOLODFU failed (rv=0, reason=)"));
        // palera1n 2.4: the last of several.
        std::fs::write(&log, " - [11/25/26 05:07:45] <Error>: Failed issuing asynchronous upload! 13\n - [11/25/26 05:07:45] <Error>: Unable to perform checkm8: ffffffff (error code: -status_exploit)\n - [11/25/26 05:07:45] <Verbose>: DFU mode device disconnected\n").unwrap();
        assert_eq!(last_error(&log).as_deref(), Some("palera1n: Unable to perform checkm8: ffffffff (error code: -status_exploit)"));
        std::fs::write(&log, "no errors here\n").unwrap();
        assert_eq!(last_error(&log), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

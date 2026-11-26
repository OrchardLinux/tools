//! Boots the install system from DFU: `palera1n -p -k pongo.bin`, then the
//! install payload to Pongo and `bootm` (orchard's `boot_payload`). The
//! install system then shows up on USB by itself, and the watcher opens it.
//!
//! Continuity: a session follows the device from DFU mode on, at the port
//! it is on, and refuses another device there; the install system's
//! receiver checks the ECID again when it is opened.
use crate::exec::Executor;
use orchard_boards::install_env;
use orchard_cache::{Code, Error, Result};
use orchard_flow::Device;
use orchard_device::select::Selector;
use orchard_device::session::Session;
use orchard_device::usb;
use orchard_usb::{Host, Mode};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PALERA1N_SECONDS: u64 = 120;
/// How long the exploit is tried again while the device stays in DFU mode.
/// On some computers it takes many attempts (seen up to 20 minutes).
const START_WINDOW: Duration = Duration::from_secs(20 * 60);
/// After a failed attempt: how long to look where the device went.
const SETTLE_SECONDS: u64 = 15;
/// How long Pongo may take to show up on USB once palera1n has ended (seen
/// 40–42 s on an iPad Air with a Raspberry Pi 4 host, orchard#239).
const PONGO_SECONDS: u64 = 60;
/// How long a device may take to come back after checkm8 before the person
/// is asked to reconnect it (it came back by itself after 7 and 8 s on a
/// j71b over a direct USB-C cable).
const BACK_SECONDS: Duration = Duration::from_secs(12);
/// A full boot uploads the patchfinder, ramdisk and overlay before iPadOS starts.
const IPADOS_SECONDS: u64 = 240;

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::NotFound, message.into())
}

/// What the chain says when another Apple device is on USB: it stops rather
/// than let palera1n or a wait take the wrong one.
pub const ANOTHER: &str = "another device is connected";

/// A session on the flow's device, which is in DFU mode: it follows the
/// device through its modes at the port it is on, and continues the lock
/// the window holds.
pub(crate) fn session(device: &Device) -> Result<Session<Host>> {
    let selector = Selector { ecid: Some(device.id.0.clone()), location: None, mode: None };
    Session::start(Host, &selector, None)
}

/// Fails when another Apple device is on USB, in any mode: palera1n and
/// checkra1n take the first device they find.
fn alone(session: &Session<Host>) -> Result<()> {
    let listed = orchard_usb::list().map_err(|e| fail(e.to_string()))?;
    let target = orchard_usb::Target { ecid: session.identity().ecid.as_deref(), location: Some(&session.device().location) };
    match orchard_usb::others(&listed, Some(target)).is_empty() {
        true => Ok(()),
        false => Err(fail(ANOTHER)),
    }
}

/// Whether the session's device is in `mode` right now.
fn now_in(session: &mut Session<Host>, mode: Mode) -> Result<bool> {
    alone(session)?;
    match session.wait_for(mode, Duration::from_millis(1)) {
        Ok(_) => Ok(true),
        Err(e) if e.code == Code::Timeout => Ok(false),
        Err(e) => Err(e),
    }
}

/// The install payload for this device: m1n1, the layer's kernel, initrd and
/// device tree, and the install tools with the device's ECID.
pub fn payload(executor: &Executor, device: &Device) -> Result<Vec<u8>> {
    let m1n1 = std::fs::read(executor.download(&executor.orchard_tools()?.m1n1)?)?;
    payload_behind(executor, device, &m1n1)
}

/// The install payload behind `m1n1`: its plain build for Pongo, its Mach-O
/// build for a patched iBoot.
fn payload_behind(executor: &Executor, device: &Device, m1n1: &[u8]) -> Result<Vec<u8>> {
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
    let logo = rel.logo.as_ref().map(|l| executor.download(l).and_then(|p| Ok(std::fs::read(p)?))).transpose()?;
    install_env::payload(m1n1, logo.as_deref(), &boot, &add_on)
}

/// Runs palera1n on the device in DFU: checks it is the user's device
/// first, resets its USB port on Linux if it doesn't come back after
/// checkm8, and logs palera1n's output in the cache.
fn run_palera1n(executor: &Executor, session: &mut Session<Host>, args: &[std::ffi::OsString], seconds: u64) -> Result<()> {
    run_palera1n_from(executor, session, Mode::Dfu, args, seconds)
}

/// palera1n on the device in `from`: DFU (the exploit, with the port reset
/// on Linux), or Pongo (palera1n carries on from there).
fn run_palera1n_from(executor: &Executor, session: &mut Session<Host>, from: Mode, args: &[std::ffi::OsString], seconds: u64) -> Result<()> {
    let (pin, release) = crate::flags::palera1n_pin().ok_or_else(|| fail("palera1n publishes no build for this computer"))?;
    let palera1n = executor.palera1n(pin, release)?;
    run_tool(executor, session, from, "palera1n", &palera1n, args, seconds)
}

/// checkra1n's exploit and Pongo with `pongo_image`,
/// with the arguments orchard's Python CLI runs it with.
fn run_checkra1n(executor: &Executor, session: &mut Session<Host>, pongo_image: &std::path::Path, seconds: u64) -> Result<()> {
    let (pin, release) = crate::flags::checkra1n_pin().ok_or_else(|| fail("checkra1n publishes no build for this computer"))?;
    let checkra1n = executor.palera1n(pin, release)?;
    let args: Vec<std::ffi::OsString> = ["-E", "-p", "-v", "-n", "-k"].iter().map(Into::into).chain([pongo_image.as_os_str().to_owned()]).collect();
    run_tool(executor, session, Mode::Dfu, "checkra1n", &checkra1n, &args, seconds)
}

/// One run of the tool that takes the device on from `from`, its output in
/// a log of its own and followed while it runs.
fn run_tool(executor: &Executor, session: &mut Session<Host>, from: Mode, name: &str, palera1n: &std::path::Path, args: &[std::ffi::OsString], seconds: u64) -> Result<()> {
    if !now_in(session, from)? {
        return Err(fail(if from == Mode::Dfu { "the device is no longer in DFU mode" } else { "the device is not in Pongo" }));
    }
    let location = session.device().location.clone();
    // Resolved while the device is present: its sysfs node goes away after checkm8.
    // checkra1n is not helped by the port reset (orchard's Python CLI runs
    // it without).
    let control = (from == Mode::Dfu && name == "palera1n")
        .then(|| crate::replug::port_control(&location, std::path::Path::new("/sys/bus/usb/devices")))
        .flatten();
    let (log, log_path) = executor.log_file(name)?;
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
                break Err(fail(format!("{name} did not finish")));
            }
            // From DFU mode, iOS on USB means the device restarted normally:
            // this start can't work any more, whatever palera1n still waits for.
            Ok(None) if from == Mode::Dfu && looked.elapsed() >= Duration::from_secs(1) => {
                looked = Instant::now();
                match now_in(session, Mode::Normal) {
                    Ok(true) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(restarted(&log_path));
                    }
                    // Another device: palera1n mustn't go on with two on USB
                    // (it would take one running iOS to recovery).
                    Err(e) if e.to_string().contains(ANOTHER) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(e);
                    }
                    _ => {}
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
        return Err(fail(last_error(&log_path).unwrap_or_else(|| format!("{name} failed ({status})"))));
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

/// palera1n's log as readable lines: the time of day and the message, no
/// colors, no banner; a line repeated in a row once with "(×N)". The last
/// `keep` lines.
fn readable(text: &str, keep: usize) -> Vec<String> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for raw in text.lines() {
        let line = strip_ansi(raw);
        let line = line.trim();
        let (time, rest) = match (line.find('['), line.find(']')) {
            (Some(a), Some(b)) if a < b => (line[a + 1..b].split_whitespace().last().unwrap_or("").to_owned(), line[b + 1..].trim()),
            _ => continue,
        };
        let (level, message) = match rest.strip_prefix('<').and_then(|r| r.split_once('>')) {
            Some((level, m)) => (level, m.trim_start_matches(':').trim()),
            None => ("", rest),
        };
        if message.is_empty() {
            continue;
        }
        let message = if level.eq_ignore_ascii_case("error") { format!("Error: {message}") } else { message.to_owned() };
        match out.last_mut() {
            Some((last, n)) if last.split_once("  ").map(|(_, m)| m) == Some(message.as_str()) => *n += 1,
            _ => out.push((format!("{time}  {message}"), 1)),
        }
    }
    let notes = crate::replug::notes();
    let mut lines: Vec<String> = out.into_iter().map(|(l, n)| if n > 1 { format!("{l} (×{n})") } else { l }).collect();
    lines.extend(notes.into_iter().map(|n| format!("          {n}")));
    let skip = lines.len().saturating_sub(keep);
    lines.split_off(skip)
}

/// The device left DFU mode and started iOS normally.
fn restarted(log: &std::path::Path) -> Error {
    let why = last_error(log).map(|e| format!(" ({e})")).unwrap_or_default();
    fail(format!("{RESTARTED}{why}"))
}

/// The tool that takes a device from DFU mode to Pongo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Palera1n,
    /// The unpatched official checkra1n.
    Checkra1n,
}

/// The tool's exploit into Pongo, then Pongo on USB. A failed attempt with
/// the device still (or again) in DFU mode is tried again, up to
/// START_WINDOW, and the flow hears of each new attempt. iOS on USB ends it:
/// the device restarted, and only DFU mode by hand starts over.
fn into_pongo(executor: &Executor, session: &mut Session<Host>, pongo_image: &std::path::Path, with: Tool) -> Result<()> {
    let until = Instant::now() + START_WINDOW;
    let mut attempt = 1u32;
    // ORCHARD_KEEP_AWAKE: no deep CPU sleep for as long as the exploit runs.
    let _awake = crate::flags::stay_awake();
    loop {
        let args = [std::ffi::OsString::from("-p"), "-k".into(), pongo_image.as_os_str().to_owned()];
        let ran = match with {
            Tool::Checkra1n => run_checkra1n(executor, session, pongo_image, PALERA1N_SECONDS),
            Tool::Palera1n => run_palera1n(executor, session, &args, PALERA1N_SECONDS),
        };
        let failed = match ran {
            Ok(()) => {
                let deadline = Instant::now() + Duration::from_secs(PONGO_SECONDS);
                loop {
                    if now_in(session, Mode::Pongo)? {
                        return Ok(());
                    }
                    if now_in(session, Mode::Normal)? {
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
        let why = failed.to_string();
        if why.contains(RESTARTED) || why.contains(ANOTHER) || Instant::now() > until {
            return Err(failed);
        }
        // Where did it go? Still or again in DFU mode: once more.
        let look = Instant::now() + Duration::from_secs(SETTLE_SECONDS);
        let again = loop {
            // Pongo after all, later than PONGO_SECONDS: carry on with it.
            if now_in(session, Mode::Pongo)? {
                return Ok(());
            }
            if now_in(session, Mode::Dfu)? {
                break true;
            }
            if now_in(session, Mode::Normal)? || Instant::now() > look {
                break false;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        if !again {
            let normal = now_in(session, Mode::Normal)?;
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
        crate::replug::clear_notes();
        let thread = std::thread::spawn(move || {
            let (mut wait, mut stalled, mut booting) = (None::<(Instant, Duration)>, false, false);
            let mut shown: Vec<String> = Vec::new();
            while !flag.load(Ordering::Relaxed) {
                // The Details panel's lines, when they changed.
                if let Ok(text) = std::fs::read(&log) {
                    let lines = readable(&String::from_utf8_lossy(&text), 60);
                    if lines != shown {
                        shown = lines.clone();
                        let _ = events.send(Event::StartLog(lines));
                    }
                }
                if wait.is_none() {
                    // Without a port reset to try first, the notice asks the
                    // person to act: only once the device is clearly late.
                    let late = |settle: Duration| if resetting.is_none() { settle.max(BACK_SECONDS) } else { settle };
                    wait = dropped(&log, crate::replug::timing()).map(|settle| (Instant::now(), late(settle)));
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
    let mut session = session(device)?;
    // palera1n goes on from its own Pongo start.
    into_pongo(executor, &mut session, &ours, Tool::Palera1n)?;
    let second = vec![std::ffi::OsString::from("-l"), "-o".into(), overlay.into_os_string(), "-k".into(), ours.into_os_string()];
    run_palera1n_from(executor, &mut session, Mode::Pongo, &second, IPADOS_SECONDS)
}

/// The launcher's loader, by the boot route: palera1n or checkra1n → Pongo →
/// the release's m1n1 → `bootm`, or the `ibss` route; then m1n1's proxy on USB.
/// m1n1 reads the device's boot partition itself; nothing else comes from
/// the host (orchard's `orchard launch`).
pub fn start_m1n1(executor: &Executor, device: &Device) -> Result<orchard_usb::Device> {
    let mut session = session(device)?;
    if executor.boot_route(&device.board)?.name == "ibss" {
        crate::ibss::start(executor, &mut session, &device.board, None, |m1n1| Ok(m1n1.to_vec()))?;
    } else {
        let tools = executor.orchard_tools()?;
        let m1n1 = std::fs::read(executor.download(&tools.m1n1)?)?;
        start_image(executor, &mut session, &device.board, &m1n1)?;
    }
    match session.wait_for(Mode::M1n1, Duration::from_secs(30)) {
        Ok(usb) => Ok(usb.clone()),
        Err(e) if e.code == Code::Timeout => Err(fail("m1n1 did not appear on USB")),
        Err(e) => Err(e),
    }
}

/// The install system by the boot route: palera1n or checkra1n → Pongo →
/// payload → `bootm`, or the `ibss` route with the payload behind m1n1.
/// Never retried by itself: a failure goes back to the flow, which asks for
/// DFU again.
pub fn boot_install_env(executor: &Executor, device: &Device) -> Result<()> {
    let mut session = session(device)?;
    if executor.boot_route(&device.board)?.name == "ibss" {
        return crate::ibss::start(executor, &mut session, &device.board, None, |m1n1| payload_behind(executor, device, m1n1));
    }
    let payload = payload(executor, device)?;
    start_image(executor, &mut session, &device.board, &payload)
}

/// `image` by the board's boot route into Pongo and started there: m1n1,
/// alone or with a system behind it. What follows differs: the install
/// system or Linux shows up on USB by itself, or m1n1 does. bootm's own
/// answer may not come (Pongo is gone once the image runs), so only what
/// shows up next confirms it.
pub fn start_image(executor: &Executor, session: &mut Session<Host>, board: &str, image: &[u8]) -> Result<()> {
    let route = executor.boot_route(board)?;
    let with = match route.name.as_str() {
        "palera1n" => Tool::Palera1n,
        "checkra1n" => Tool::Checkra1n,
        other => return Err(Error::new(Code::Unsupported, format!("the {other} route does not go through Pongo"))),
    };
    let pongo_image = executor.pongo_image()?;
    into_pongo(executor, session, &pongo_image, with)?;
    let mut pongo = session.pongo()?;
    pongo.send(image, session.timeout(Duration::from_secs(60))?).map_err(usb::error)?;
    let _ = pongo.command("bootm", session.timeout(Duration::from_secs(10))?);
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

    #[test]
    fn readable_lines_from_both_palera1n_versions() {
        let beta = " \x1b[38;5;47m[2026/11/25 03:56:42]\x1b[0m  \x1b[38;5;51m\x1b[1m<Log>\x1b[22m\x1b[0m Found DFU mode device\n:: Made by: asdfugil\n [2026/11/25 03:56:43]  <Log> Checkmate?\n";
        assert_eq!(readable(beta, 10), vec!["03:56:42  Found DFU mode device", "03:56:43  Checkmate?"]);
        let v24 = " - [11/25/26 05:07:01] <Info>: Setting up the exploit\n - [11/25/26 05:07:02] <Verbose>: UaF race: heuristic strategy failed, setup packet was not accepted\n - [11/25/26 05:07:03] <Verbose>: UaF race: heuristic strategy failed, setup packet was not accepted\n - [11/25/26 05:07:45] <Error>: Unable to perform checkm8: ffffffff\n";
        assert_eq!(
            readable(v24, 10),
            vec!["05:07:01  Setting up the exploit", "05:07:02  UaF race: heuristic strategy failed, setup packet was not accepted (×2)", "05:07:45  Error: Unable to perform checkm8: ffffffff"]
        );
        assert_eq!(readable(v24, 1), vec!["05:07:45  Error: Unable to perform checkm8: ffffffff"]);
    }
}

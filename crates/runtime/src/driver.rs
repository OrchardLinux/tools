//! Runs the flow: polls USB, steps the flow with every event, carries out
//! its effects and sends timeouts (it keeps no state across launches). The headless runner
//! and the graphical app both drive the installer through this; they differ
//! only in how they show the flow ([`Observer`]) and where user input comes
//! from (events sent to [`Handle::events`]).
use crate::companion::{self, Job as CompanionJob};
use crate::exec::Executor;
use crate::install::{self as installing, Job};
use crate::{records, usb_access, Watcher};
use orchard_boards::catalog::Catalog;
use orchard_cache::Result;
use orchard_flow::{step, App, Effect, Event, Flow, UsbMode};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// How the installer runs.
#[derive(Debug, Clone)]
pub struct Options {
    pub app: App,
    /// Board data to use instead of the release's own (development).
    pub data: Option<PathBuf>,
    /// The release index (from the stamp).
    pub index: Option<String>,
    /// A local release build instead of the published one.
    pub local: Option<PathBuf>,
    /// A companion directory instead of the built-in one (development).
    pub companion: Option<PathBuf>,
    /// A Pongo to use instead of the release's (testing a build).
    pub pongo: Option<PathBuf>,
    /// The one device this run is for; any other is not worked on.
    pub only: Option<crate::Only>,
    /// Download and build assets; off only to watch the flow without work.
    pub fetch: bool,
    /// Act on the device: palera1n boots, splitting, installing. Off, those
    /// effects are reported and skipped, so the flow can be clicked through
    /// safely with a real device attached.
    pub live: bool,
    pub cache: PathBuf,
}

impl Options {
    pub fn new(app: App) -> Options {
        Options {
            app,
            data: None,
            index: None,
            local: None,
            companion: None,
            pongo: None,
            only: None,
            fetch: true,
            live: false,
            // orchard2's cache (the cache crate's place, or ORCHARD_CACHE),
            // shared with resize-apfs and the command line.
            cache: std::env::var_os("ORCHARD_CACHE")
                .map(PathBuf::from)
                .or_else(|| orchard_cache::store::default_dir().ok())
                .unwrap_or_else(|| std::env::temp_dir().join("orchard2")),
        }
    }

    /// The executor these options describe.
    pub fn executor(&self, events: Sender<Event>) -> Result<Executor> {
        let mut executor = Executor::new(self.data.as_deref(), &self.cache, events);
        if let Some(index) = &self.index {
            executor = executor.with_index(index);
        }
        if let Some(local) = &self.local {
            executor = executor.with_local_release(local)?;
        }
        if let Some(dir) = &self.companion {
            executor = executor.with_companion(dir);
        }
        if let Some(pongo) = &self.pongo {
            executor = executor.with_pongo(pongo);
        }
        Ok(executor)
    }
}

/// What a front end is told while the flow runs. Called on the driver's
/// thread.
pub trait Observer: Send + 'static {
    /// The flow right after an event, before its effects are carried out.
    fn stepped(&mut self, _flow: &Flow) {}
    /// The flow after every event.
    fn changed(&mut self, flow: &Flow);
    /// Each effect, before it is carried out.
    fn effect(&mut self, _effect: &Effect) {}
    /// Each USB event, before the flow sees it.
    fn usb(&mut self, _event: &Event) {}
    /// Something that went wrong outside the flow (USB access).
    fn problem(&mut self, message: &str) {
        eprintln!("{message}");
    }
    /// The flow is done (the system started and the user closed the app).
    fn quit(&mut self) {
        std::process::exit(0)
    }
}

/// A running driver.
#[derive(Clone)]
pub struct Handle {
    /// Send user input (Continue, Skip, the account …) here.
    pub events: Sender<Event>,
    pub executor: Executor,
}

/// Starts the demo script instead of the driver: no USB, no work, no device
/// (see demo.rs). Its handle has an executor only for the release's name.
pub fn start_demo(options: Options, observer: impl Observer) -> Result<Handle> {
    let (tx, rx) = mpsc::channel();
    let executor = options.executor(tx.clone())?;
    let handle = Handle { events: tx.clone(), executor };
    let app = options.app;
    std::thread::spawn(move || crate::demo::run(app, observer, rx, tx));
    Ok(handle)
}

/// Starts the driver on its own thread. Fails before starting when the
/// board data can't be had (no `--data`, no network and nothing cached).
pub fn start(options: Options, observer: impl Observer) -> Result<Handle> {
    crate::flags::log_active();
    let (tx, rx) = mpsc::channel();
    let executor = options.executor(tx.clone())?;
    let catalog = Catalog::open(&executor.data()?)?;
    let handle = Handle { events: tx.clone(), executor: executor.clone() };
    let driver = Driver {
        app: options.app,
        fetch: options.fetch,
        live: options.live,
        only: options.only.clone(),
        jobs: installing::spawn(executor.clone(), tx.clone()),
        launcher_jobs: crate::launcher::spawn(executor.clone(), tx.clone()),
        companion_jobs: companion::spawn(tx.clone(), options.cache.join("logs")),
        executor,
        events: tx,
        observer,
    };
    std::thread::spawn(move || driver.run(catalog, rx));
    Ok(handle)
}

struct Driver<O: Observer> {
    app: App,
    fetch: bool,
    live: bool,
    only: Option<crate::Only>,
    executor: Executor,
    events: Sender<Event>,
    jobs: Sender<Job>,
    launcher_jobs: Sender<crate::launcher::Job>,
    companion_jobs: Sender<CompanionJob>,
    observer: O,
}

impl<O: Observer> Driver<O> {
    fn feed(&mut self, flow: Flow, event: Event) -> Flow {
        // Failures carry their reason only in the event: keep it.
        if matches!(event, Event::ChainFailed(_) | Event::AssetFailed(..) | Event::SplitRefused(_) | Event::InstallFinished { ok: false } | Event::SplitFinished { ok: false }) {
            self.observer.problem(&format!("failed: {event:?}"));
        }
        let (next, effects) = step(self.app, flow, event);
        self.observer.stepped(&next);
        for effect in effects {
            self.observer.effect(&effect);
            let acts_on_device = matches!(
                effect,
                Effect::BootInstallEnv(_)
                    | Effect::BootIpadosWithOverlay(_)
                    | Effect::StartSplit { .. }
                    | Effect::PrepareInstall
                    | Effect::StartInstall(_)
                    | Effect::RestartStream
                    | Effect::ListEntries(_)
                    | Effect::BootEntry(_)
            );
            if acts_on_device && !self.live {
                self.observer.problem(&format!("not live: skipped {effect:?} (run with --live to act on the device)"));
                continue;
            }
            match effect {
                Effect::Notify(text) => records::notify(&text),
                Effect::RequestUsbAccess => {
                    let events = self.events.clone();
                    std::thread::spawn(move || match usb_access::request() {
                        Ok(()) => drop(events.send(Event::UsbAccessGranted)),
                        Err(e) => eprintln!("usb access: {e}"),
                    });
                }
                Effect::RestartIpad => {
                    // Before quitting: wait (briefly) until the companion took it.
                    let (tx, rx) = mpsc::channel();
                    if self.companion_jobs.send(CompanionJob::Restart(tx)).is_ok() {
                        let _ = rx.recv_timeout(Duration::from_secs(3));
                    }
                }
                Effect::Quit => self.observer.quit(),
                Effect::PrepareInstall => drop(self.jobs.send(Job::Prepare)),
                Effect::StartInstall(account) => drop(self.jobs.send(Job::Start(account))),
                Effect::RestartStream => drop(self.jobs.send(Job::Restart)),
                Effect::ListEntries(_) => {
                    if let Some(device) = next.state.device() {
                        drop(self.launcher_jobs.send(crate::launcher::Job::List(device.clone())));
                    }
                }
                Effect::BootEntry(id) => drop(self.launcher_jobs.send(crate::launcher::Job::Boot(id))),
                Effect::StartSplit { omarchy_gb } => drop(self.companion_jobs.send(CompanionJob::Split { omarchy_gb })),
                Effect::RefreshLayout => drop(self.companion_jobs.send(CompanionJob::Refresh)),
                Effect::BootInstallEnv(_) | Effect::BootIpadosWithOverlay(_) => {
                    self.executor.run(&effect, next.state.device())
                }
                effect if self.fetch => self.executor.run(&effect, next.state.device()),
                _ => {}
            }
        }
        self.observer.changed(&next);
        next
    }

    fn run(mut self, catalog: Catalog, rx: Receiver<Event>) {
        let mut watcher = Watcher::default();
        // Devices whose lockdown failure was already logged (it retries every poll).
        let mut lockdown_logged = std::collections::BTreeSet::new();
        // When each unanswered device may be asked again: not every poll,
        // which floods iPadOS with half-open connections while it doesn't answer.
        let mut lockdown_next: std::collections::BTreeMap<String, Instant> = std::collections::BTreeMap::new();
        // Lockdown answers come from a thread of their own: a device that
        // doesn't answer must not hold up the loop (USB, events) for its timeout.
        let (asked_tx, asked_rx) = mpsc::channel::<(String, std::result::Result<crate::lockdown::Answers, String>)>();
        let mut asking = std::collections::BTreeSet::new();
        let mut unresponsive = false;
        // The devices the flow was last told about (Event::Others).
        let mut others_told: Vec<orchard_flow::Other> = Vec::new();
        // Sightings in normal or recovery mode wait a moment: a device passes
        // through them on its way to DFU, and screens shouldn't flicker.
        let mut pending: std::collections::BTreeMap<orchard_flow::DeviceId, (Event, Instant)> = std::collections::BTreeMap::new();
        let mut flow = self.feed(Flow::new(), Event::Started);
        // Linux: the user's account can't reach the device until the udev rule is in.
        if usb_access::needed() {
            flow = self.feed(flow, Event::UsbAccessDenied);
        }
        // When the current wait began, for its timeout: a new state, or a
        // timeout starting to apply within one (the grace period on 1.2).
        let wait_key = |flow: &Flow| (std::mem::discriminant(&flow.state), orchard_flow::timeout(&flow.state).is_some());
        let mut waiting = (wait_key(&flow), Instant::now(), false);
        let mut next_poll = Instant::now();
        // Where the device named for this run (Options::only) was last seen.
        let mut named_at: Option<String> = None;
        // The devices this run holds, so that no other work touches them.
        let mut held = crate::held::Held::default();
        loop {
            let kind = wait_key(&flow);
            if kind != waiting.0 {
                waiting = (kind, Instant::now(), false);
            }
            if let Some(limit) = orchard_flow::timeout(&flow.state) {
                if !waiting.2 && waiting.1.elapsed() >= limit {
                    waiting.2 = true;
                    flow = self.feed(flow, Event::Timeout);
                }
            }
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + Duration::from_millis(500);
                match orchard_usb::list() {
                    Ok(devices) => {
                        // Answers that came in since the last poll.
                        while let Ok((udid, answers)) = asked_rx.try_recv() {
                            asking.remove(&udid);
                            match answers {
                                Ok(answers) => watcher.learn(&udid, answers),
                                // usbmuxd may not have it yet: ask again in a while.
                                Err(e) => {
                                    if lockdown_logged.insert(udid.clone()) {
                                        self.observer.problem(&format!("lockdown: {e} (retrying)"));
                                    }
                                }
                            }
                        }
                        // A device plugged in running iPadOS: ask it who it is,
                        // one question at a time, at most every 5 s.
                        for usb in devices.iter().filter(|d| d.mode == orchard_usb::Mode::Normal) {
                            let due = |u: &str| lockdown_next.get(u).is_none_or(|t| Instant::now() >= *t);
                            if let Some(udid) = usb.identity.udid.clone().filter(|u| !watcher.knows(u) && due(u) && !asking.contains(u)) {
                                lockdown_next.insert(udid.clone(), Instant::now() + Duration::from_secs(5));
                                asking.insert(udid.clone());
                                let tx = asked_tx.clone();
                                std::thread::spawn(move || {
                                    let answers = crate::lockdown::ask(std::path::Path::new(crate::usbmux::SOCKET), &udid).map_err(|e| e.to_string());
                                    let _ = tx.send((udid, answers));
                                });
                            }
                        }
                        // An iPad running iPadOS that was asked and never answered.
                        let silent = devices.iter().any(|d| {
                            d.mode == orchard_usb::Mode::Normal
                                && d.identity.udid.as_deref().is_some_and(|u| !watcher.knows(u) && lockdown_logged.contains(u))
                        });
                        if silent != unresponsive {
                            unresponsive = silent;
                            flow = self.feed(flow, Event::Unresponsive(silent));
                        }
                        // With one device named, the flow hears of no other.
                        // A device that does not tell its ECID (the installed
                        // system, Pongo) is the named one at the port it was at.
                        if let Some(only) = &self.only {
                            let told = devices.iter().find(|d| watcher.ecid(d).is_some_and(|ecid| only.is(d, Some(&ecid))));
                            if let Some(device) = told {
                                named_at = Some(device.location.clone());
                            }
                        }
                        let (named, unnamed): (Vec<_>, Vec<_>) = devices.iter().cloned().partition(|d| {
                            self.only.as_ref().is_none_or(|only| {
                                let ecid = watcher.ecid(d);
                                only.is(d, ecid.as_deref()) || (ecid.is_none() && named_at.as_deref() == Some(d.location.as_str()))
                            })
                        });
                        // A device running iOS says who it is a moment after it
                        // shows up: until then it is not known to be another.
                        let asked = |d: &orchard_usb::Device| {
                            d.mode == orchard_usb::Mode::Normal
                                && self.only.as_ref().is_some_and(|only| only.ecid.is_some())
                                && watcher.ecid(d).is_none()
                        };
                        let mut unnamed: Vec<_> = unnamed.into_iter().filter(|d| orchard_usb::apple_device(d) && !asked(d)).collect();
                        // One piece of work on a device at a time: a device in
                        // DFU mode is taken before the flow hears of it, and one
                        // that other work holds is left to it.
                        let working_at = flow.state.working().and_then(|d| watcher.location(&d.id)).map(str::to_owned);
                        let in_dfu: Vec<&str> = named.iter().filter(|d| d.mode == orchard_usb::Mode::Dfu).map(|d| d.location.as_str()).collect();
                        let look = held.look(&in_dfu, working_at.as_deref());
                        // A device the flow took up as it found it running.
                        let adopted = working_at.as_deref().and_then(|port| held.adopt(port));
                        for problem in look.problems.iter().chain(adopted.iter()) {
                            self.observer.problem(problem);
                        }
                        let (named, taken): (Vec<_>, Vec<_>) = named
                            .into_iter()
                            .partition(|d| !(d.mode == orchard_usb::Mode::Dfu && look.in_use.iter().any(|(port, _)| *port == d.location)));
                        let in_use: Vec<(orchard_usb::Device, String)> = taken
                            .into_iter()
                            .map(|d| {
                                let holder = look.in_use.iter().find(|(port, _)| *port == d.location).map(|(_, who)| who.clone()).unwrap_or_default();
                                (d, holder)
                            })
                            .collect();
                        unnamed.extend(in_use.iter().map(|(d, _)| d.clone()));
                        // What the boot ROM says of a unit, for what an install
                        // remembers of it afterwards.
                        for usb in named.iter().filter(|d| d.mode == orchard_usb::Mode::Dfu) {
                            if let (Some(ecid), Some(serial)) = (usb.identity.ecid.as_deref(), usb.identity.serial_number.as_deref()) {
                                self.executor.note_serial_number(&crate::ecid_key(ecid), serial);
                            }
                        }
                        let seen_events = watcher.see(&catalog, &named);
                        // Other Apple devices besides the one being worked on:
                        // neither app goes on until they're gone.
                        {
                            let target = flow.state.device().map(|d| d.id.clone());
                            let mut others = if target.is_none() && !unnamed.is_empty() {
                                // Only devices that were not named are there.
                                listed_devices(&catalog, &watcher, &unnamed, None)
                            } else if watcher.others(&devices, target.as_ref()).is_empty() {
                                Vec::new()
                            } else {
                                listed_devices(&catalog, &watcher, &devices, flow.state.device())
                            };
                            // A device other work holds says so in place of its mode.
                            for (device, holder) in &in_use {
                                let name = listed_devices(&catalog, &watcher, std::slice::from_ref(device), None).pop().map(|o| o.name);
                                if let Some(other) = name.and_then(|name| others.iter_mut().find(|o| o.name == name && o.held_by.is_none())) {
                                    // "(pid 12: orchard2 load)": the program.
                                    let program = holder.trim_matches(|c| c == '(' || c == ')').split_once(": ").map_or("", |(_, program)| program);
                                    other.held_by = Some(program.to_owned());
                                }
                            }
                            if others != others_told {
                                others_told = others.clone();
                                flow = self.feed(flow, Event::Others(others));
                            }
                        }
                        let mut ready = Vec::new();
                        for event in seen_events {
                            match &event {
                                Event::Attached { device, mode: UsbMode::Normal | UsbMode::Recovery } => {
                                    pending.insert(device.id.clone(), (event.clone(), Instant::now()));
                                }
                                // Gone again before it counted: as if never seen.
                                Event::Detached { id } if pending.contains_key(id) => {
                                    pending.remove(id);
                                }
                                Event::Attached { device, .. } => {
                                    pending.remove(&device.id);
                                    ready.push(event);
                                }
                                _ => ready.push(event),
                            }
                        }
                        let due: Vec<_> = pending.iter().filter(|(_, (_, at))| at.elapsed() >= Duration::from_secs(1)).map(|(id, _)| id.clone()).collect();
                        for id in due {
                            ready.push(pending.remove(&id).unwrap().0);
                        }
                        for event in ready {
                            self.observer.usb(&event);
                            // The companion and the install system are the installer's; the
                            // launcher leaves them alone (an installer may be running too).
                            let installer = self.app == App::Installer;
                            if let (true, Event::Attached { device, mode: UsbMode::Normal }) = (installer, &event) {
                                let udid = watcher.normal_device(&device.id).and_then(|u| u.identity.udid.clone());
                                if let (Some(udid), Some(token)) = (udid, self.executor.companion_token()) {
                                    let _ = self.companion_jobs.send(CompanionJob::Attach { udid, device: device.clone(), token });
                                }
                            }
                            if let (true, Event::Attached { device, mode: UsbMode::Install }) = (installer, &event) {
                                if let Some(usb) = watcher.install_system(&device.id) {
                                    let _ = self.jobs.send(Job::Open { usb: Box::new(usb.clone()), device: device.clone() });
                                }
                            }
                            flow = self.feed(flow, event);
                        }
                    }
                    Err(e) => {
                        self.observer.problem(&format!("usb: {e}"));
                        flow = self.feed(flow, Event::UsbAccessDenied);
                    }
                }
            }
            // User input and work results, as they come; USB every half second.
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => flow = self.feed(flow, event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

/// Every Apple device on USB as the "more than one device" screen lists it:
/// its name where it can be told (the device being installed on, a board
/// the boot ROM names, the model iOS told), and what it runs.
fn listed_devices(
    catalog: &Catalog,
    watcher: &crate::Watcher,
    devices: &[orchard_usb::Device],
    target: Option<&orchard_flow::Device>,
) -> Vec<orchard_flow::Other> {
    // Whether a listed device is the one being installed on (orchard_usb::others).
    let is_target = |d: &orchard_usb::Device| target.is_some_and(|t| watcher.others(std::slice::from_ref(d), Some(&t.id)).is_empty());
    devices
        .iter()
        .filter(|d| orchard_usb::apple_device(d))
        .map(|d| {
            let name = target.filter(|_| is_target(d)).map(|t| t.name.clone());
            let name = name
                .or_else(|| crate::identify(catalog, d).map(|s| s.device.name).filter(|n| !n.starts_with("Unknown")))
                .or_else(|| watcher.told_product(d).and_then(|p| catalog.board_for_product(&p).ok()).map(|b| b.display_name))
                .or_else(|| d.product.clone().filter(|p| !p.starts_with("Apple Mobile Device")))
                .unwrap_or_else(|| "Apple device".into());
            let mode = match d.mode {
                orchard_usb::Mode::Normal if name.starts_with("iPad") => "ipados",
                orchard_usb::Mode::Normal => "ios",
                mode => mode.as_str(),
            };
            orchard_flow::Other { name, mode: mode.into(), held_by: None }
        })
        .collect()
}

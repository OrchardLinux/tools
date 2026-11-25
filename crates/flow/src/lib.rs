//! The installer's and launcher's state: one statechart, one update function.
//!
//! [`update`] is the only way the state changes. It never does I/O: it returns
//! [`Effect`]s for the app to carry out, and the results come back as
//! [`Event`]s. [`view`] derives everything the window shows from the state.
//! See the README for the non-goals.

pub mod assets;
mod view;
pub use assets::{Asset, AssetState, Assets, Cause};
pub use view::{view, view_flow, DeviceView, Glyph, Screen, ScreenKind, Tint};

#[cfg(test)]
mod tests;

/// Which app runs the flow. They share Connect; the installer prepares and
/// installs, the launcher lists and starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Installer,
    Launcher,
}

/// The device's identity as USB reports it: stable across modes and replugs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub String);

/// One drawing per button layout, not per device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    IpadHome,
    IpadNoHome,
    IphoneHome,
    IphoneNoHome,
}

/// The DFU button sequence; the tabs on the waiting screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// Home + Power: iPads with a Home button, iPhone 6s and earlier.
    HomePower,
    /// Volume Down + Side: iPhone 7 and 7 Plus.
    VolumeDownSide,
    /// Volume Up, Volume Down, Side: no Home button, iPhone 8 and later.
    VolumeSequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Supported,
    Experimental,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: DeviceId,
    /// The catalog's board name, such as `j71b`.
    pub board: String,
    pub name: String,
    pub model: Model,
    pub sequence: Sequence,
    pub support: Support,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbMode {
    Dfu,
    Recovery,
    Normal,
    /// The install system's receiver (`05ac:4146`).
    Install,
    /// The installed system, booted after the install finished (`05ac:4145`).
    Installed,
}

/// A device seen on USB that is not (yet) in DFU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub device: Device,
    pub mode: UsbMode,
}

/// A partition outside iPadOS that the install would erase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub filesystem: String,
    pub gb: u32,
}

/// The storage as iPadOS (split) or the install environment (install) reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub total_gb: u32,
    pub ipados_gb: u32,
    pub ipados_used_gb: u32,
    /// Everything after iPadOS; overwritten by the install.
    pub outside: Vec<Partition>,
    /// The bytes after iPadOS, where they were read off the disk; 0 where
    /// only whole GB are known.
    pub outside_bytes: u64,
}

impl Layout {
    /// The most iPadOS can give up while keeping its used space.
    pub fn max_omarchy_gb(&self) -> u32 {
        self.total_gb.saturating_sub(self.ipados_used_gb + HEADROOM_GB)
    }
    pub fn outside_gb(&self) -> u32 {
        self.total_gb - self.ipados_gb
    }
    /// Whether the space after iPadOS, as it is, holds the system. In real
    /// bytes where they are known: 9.5 GB reads as 10 GB and is too little.
    pub fn has_room(&self) -> bool {
        match self.outside_bytes {
            0 => self.outside_gb() >= MIN_OMARCHY_GB,
            bytes => bytes >= MIN_SYSTEM_BYTES,
        }
    }
}

/// Space iPadOS keeps free on top of what it uses.
pub const HEADROOM_GB: u32 = 2;
/// The least the install system takes, until the release states its unpacked
/// size: orchard's install layout needs an 8 GiB root and a 1 GiB boot
/// partition (9.7 GB).
pub const MIN_OMARCHY_GB: u32 = 10;
/// The same as orchard's install counts it: an 8 GiB root and a 1 GiB boot
/// partition. Space that is there already is measured against this.
pub const MIN_SYSTEM_BYTES: u64 = 9 << 30;
pub const RECOMMENDED_OMARCHY_GB: u32 = 32;
/// Room iPadOS keeps beyond what it uses, in the suggested split (the hard
/// limit is [`HEADROOM_GB`]).
pub const SUGGESTED_BUFFER_GB: u32 = 10;

/// A boot entry, as the launcher lists it; grouped by system, newest kernel first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub kernel: String,
    pub default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub username: String,
    pub password: String,
    pub password_again: String,
    pub pin: String,
    pub pin_again: String,
    /// "Allow SSH from this computer": import this computer's keys.
    pub ssh: bool,
}

impl Account {
    pub fn valid(&self) -> bool {
        !self.username.is_empty()
            && self.username.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            && self.password.chars().count() >= 8
            && self.password == self.password_again
            && self.pin.len() == 6
            && self.pin.chars().all(|c| c.is_ascii_digit())
            && self.pin == self.pin_again
    }
}

/// Whether the device's link is up while work runs on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Up,
    /// Unplugged or silent. The work goes on on the device; a replug of the same
    /// device re-attaches without starting over.
    Lost,
}

/// Why the user is asked for DFU mode again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Again {
    /// After a split: install from DFU.
    Install,
    /// The storage is not split yet (skipped by mistake): split it first.
    Split,
    /// The device restarted while splitting; DFU lets the installer look.
    CheckSplit,
    /// The install failed; retry from DFU.
    Retry,
    /// Booting iPadOS with the overlay failed; retry from DFU.
    RetryPrepare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    // Connect
    /// `leaving`: the device 1.2 showed dropped off USB, most likely because
    /// the user is doing the DFU combination; 1.2 stays for a grace period.
    WaitingForDfu { tab: Sequence, seen: Vec<Seen>, leaving: Option<Seen> },
    SeveralInDfu { devices: Vec<Device> },
    Unsupported { device: Device },
    Found { device: Device, confirm_skip: bool },
    UsbAccessNeeded,
    // Prepare: split the storage in iPadOS
    WaitingForIpados { device: Device, slow: bool },
    ChooseSplit { device: Device, layout: Layout, omarchy_gb: u32, approved: bool },
    Splitting { device: Device, omarchy_gb: u32, link: Link, reopened: bool },
    SplitDone { device: Device, omarchy_gb: u32 },
    // Install
    DfuAgain { device: Device, why: Again, seen: Option<UsbMode> },
    CreateAccount {
        device: Device,
        account: Account,
        env: Option<Layout>,
        erase_approved: bool,
        /// The install environment is taking longer than usual.
        slow: bool,
    },
    ConfirmErase { device: Device, account: Account, layout: Layout, approved: bool },
    NotPrepared { device: Device },
    Installing {
        device: Device,
        percent: u8,
        link: Link,
        /// The image verified: the device finishes by itself, cable or not.
        streamed: bool,
        /// The finishing step the device reported last.
        step: Option<String>,
    },
    InstallFailed { device: Device },
    Starting { device: Device, title: String },
    // Launcher
    /// The user pressed Start: palera1n, Pongo and m1n1 start the device and
    /// read what's installed.
    Launching { device: Device },
    ChooseSystem { device: Device, entries: Vec<Entry>, selected: usize },
    NothingInstalled { device: Device },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    // User
    /// The launcher never starts an entry by itself (see `Flow::ask_entry`).
    AskEntry,
    SelectTab(Sequence),
    Continue,
    Skip,
    ConfirmSkip,
    CancelSkip,
    SetOmarchySize(u32),
    Approve(bool),
    EditAccount(Account),
    SelectEntry(usize),
    Retry,
    /// Back out of a confirmation (the erase dialog).
    Cancel,
    // USB
    Attached { device: Device, mode: UsbMode },
    Detached { id: DeviceId },
    /// The Apple devices on USB when there are others besides the device
    /// being installed on (or several before one is chosen); empty once
    /// there aren't.
    Others(Vec<Other>),
    UsbAccessDenied,
    UsbAccessGranted,
    // Work
    IpadosReady { layout: Layout },
    /// Whether a device running iPadOS is on USB but doesn't answer (it
    /// can't be identified, so it isn't shown as found).
    Unresponsive(bool),
    /// palera1n sits after checkm8: the device needs its cable plugged in
    /// again at the device (palera1n's "Checkmate?" without the device back).
    BootStalled,
    /// It came back.
    BootResumed,
    /// The exploit didn't take and the device is still in DFU mode: palera1n
    /// runs again, this attempt (2, 3, …).
    StartAttempt(u32),
    /// palera1n's log of the start under way, as readable lines (time and
    /// message), for the Details panel.
    StartLog(Vec<String>),
    /// palera1n boots iPadOS (its "Booting..."): from here iPadOS starts.
    BootBooting,
    /// The companion's connection dropped while the device still runs
    /// iPadOS; the app reconnects by itself.
    CompanionLost,
    /// It is back.
    CompanionBack,
    SplitFinished { ok: bool },
    /// The companion refused the split or it stopped, with the reason: as
    /// a failed split, and the reason shows on the split screen.
    SplitRefused(String),
    EnvReady { layout: Layout },
    /// Stream progress, from the receiver's `stream` events.
    InstallProgress(u8),
    /// The stream verified (`Stream` answered `Ok`).
    Streamed,
    /// A finishing step started on the device.
    InstallStep(String),
    /// After a lost link the receiver answered again; its `Info` says
    /// whether the stream was cut off.
    Reconnected { stream_incomplete: bool },
    InstallFinished { ok: bool },
    /// The install failed, with the reason (the device's or the host's): as
    /// a failed install, and the reason shows on 3.9.
    InstallRefused(String),
    /// An install system found with the plan written but no account staged
    /// (the installer stopped before it): ask for the account to go on.
    AccountNeeded,
    EntriesListed(Vec<Entry>),
    /// The device's helper reports a split running that this run didn't
    /// start (the installer was closed during it): watch it again (2.4b).
    SplitRunning { device: Device, omarchy_gb: u32 },
    // Assets
    Started,
    AssetReady(Asset),
    AssetFailed(Asset, assets::Cause, String),
    RetryAssets,
    /// The current wait took longer than usual.
    Timeout,
    /// palera1n or a later stage of the chain failed before the device was up.
    ChainFailed(String),
    /// The user confirmed quitting (the quit dialog).
    Quit,
    /// The launcher: m1n1 took the entry and is starting it (the USB link may go).
    EntryStarted,
}

/// Work for the app to start. Names only; this crate does none of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    BootIpadosWithOverlay(DeviceId),
    StartSplit { omarchy_gb: u32 },
    /// Read iPadOS's storage again ("Check again" after freeing space).
    RefreshLayout,
    BootInstallEnv(DeviceId),
    /// Start transferring the system while the user fills in the account.
    /// Inspect, Plan and stage the layer and firmware: everything but the
    /// account, while the user fills it in.
    PrepareInstall,
    /// Stage the account's secrets, then stream the root image.
    StartInstall(Account),
    /// Stream the root image again from the start, after it was cut off.
    RestartStream,
    ListEntries(DeviceId),
    BootEntry(String),
    RequestUsbAccess,
    Notify(String),
    /// Fetch or build an asset; the board's assets use the device in the state.
    Fetch(Asset),
    /// Restart the iPad through the companion (quitting from the temporary iPadOS).
    RestartIpad,
    Quit,
}

/// The whole state: the flow's screen state and the asset lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub state: State,
    pub assets: Assets,
    /// The install environment asked to write, but an asset was not ready yet.
    pub write_waiting: bool,
    /// The install was started while the preparation waited: it follows it.
    pub start_waiting: Option<Account>,
    /// palera1n was to boot iPadOS before the overlay was built: it boots
    /// once it is.
    pub boot_waiting: Option<Effect>,
    /// Whether the companion is connected (from the first layout on).
    pub companion: bool,
    /// Why the last split didn't happen, until the user changes the split.
    pub split_problem: Option<String>,
    /// Why the install failed (3.9).
    pub install_problem: Option<String>,
    /// The account is asked again to continue an install found under way.
    pub resuming: bool,
    /// palera1n waits for the device to be replugged (the dialog shows).
    pub boot_stalled: bool,
    /// palera1n has started booting iPadOS (2.1 instead of 2.1a).
    pub booting: bool,
    /// A device running iPadOS is connected but doesn't answer (1.1's hint).
    pub unresponsive: bool,
    /// The split erases partitions: its confirmation dialog is open.
    pub confirming_split: bool,
    /// The launcher's chosen entry was handed to m1n1 and is starting.
    pub entry_started: bool,
    /// The launcher asks which entry to start even when there is one: a
    /// command that only lists them, or that names the one to start.
    pub ask_entry: bool,
    /// The exploit's attempt under way (0 or 1: the first).
    pub start_attempt: u32,
    /// Why the last start didn't work (palera1n's words), for DFU mode again.
    pub start_problem: Option<String>,
    /// palera1n's log of the start under way (Details).
    pub start_log: Vec<String>,
    /// Every Apple device on USB while more than the one being installed on
    /// is (the installer refuses to go on); empty otherwise.
    pub others: Vec<Other>,
    /// Starts of the device held back while other devices are on USB; they
    /// go once those are gone, if the flow still waits for them.
    pub held: Vec<Effect>,
}

/// How copy names the app: "The installer" or "Orchard" (the launcher).
pub fn app_name(app: App) -> &'static str {
    match app {
        App::Installer => "The installer",
        App::Launcher => "Orchard",
    }
}

/// The title of the screen (and the CLI's line) while more than one Apple
/// device is on USB.
pub const SEVERAL_TITLE: &str = "More than one device connected";

/// An Apple device on USB, as the "more than one device" screen lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Other {
    pub name: String,
    /// What it runs: dfu, recovery, ios, pongo, m1n1, linux.
    pub mode: String,
    /// Other work holds it (its lock): the program, such as `orchard2
    /// load`, or nothing where that is not known. None: nobody does.
    pub held_by: Option<String>,
}

/// What the screen says when nothing is connected but devices that other
/// work holds: there is nothing to unplug, only to wait for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InUse {
    pub title: &'static str,
    pub hint: String,
}

impl Flow {
    /// The words for devices in use, when that is all there is: the run
    /// works on none, and every device connected is held by other work.
    pub fn in_use(&self) -> Option<InUse> {
        if self.state.working().is_some() || self.others.is_empty() || self.others.iter().any(|o| o.held_by.is_none()) {
            return None;
        }
        let one = self.others.len() == 1;
        let mut programs: Vec<&str> = self.others.iter().filter_map(|o| o.held_by.as_deref()).filter(|p| !p.is_empty()).collect();
        programs.dedup();
        let who = match programs.as_slice() {
            [program] => (*program).to_owned(),
            _ => "Another Orchard program".to_owned(),
        };
        Some(InUse {
            title: if one { "This device is in use" } else { "These devices are in use" },
            hint: format!("{who} is working on {}. This goes on when it has finished.", if one { "it" } else { "them" }),
        })
    }

    /// Whether the app refuses to go on because other Apple devices are on
    /// USB: from waiting for DFU mode up to the install system (the install
    /// itself runs over its own connection), or in the launcher up to the
    /// installed system starting.
    pub fn refusing(&self) -> bool {
        use State::*;
        !self.others.is_empty()
            && matches!(
                self.state,
                WaitingForDfu { .. }
                    | SeveralInDfu { .. }
                    | Unsupported { .. }
                    | Found { .. }
                    | WaitingForIpados { .. }
                    | SplitDone { .. }
                    | DfuAgain { .. }
                    | NotPrepared { .. }
                    | InstallFailed { .. }
                    | CreateAccount { env: None, .. }
                    | Launching { .. }
                    | ChooseSystem { .. }
                    | NothingInstalled { .. }
            )
    }

    /// Whether a held start of the device still applies: the flow still
    /// waits for what it starts.
    fn starting(&self, app: App) -> bool {
        match app {
            App::Installer => matches!(self.state, State::WaitingForIpados { .. } | State::CreateAccount { env: None, .. }),
            App::Launcher => matches!(self.state, State::Launching { .. }),
        }
    }

    pub fn new() -> Flow {
        Flow { ask_entry: false, state: State::start(), assets: Assets::default(), write_waiting: false, start_waiting: None, boot_waiting: None, companion: false, split_problem: None, install_problem: None, resuming: false, boot_stalled: false, booting: false, unresponsive: false, confirming_split: false, entry_started: false, start_attempt: 0, start_problem: None, start_log: Vec::new(), others: Vec::new(), held: Vec::new() }
    }

    /// The asset the current step waits for, for "Getting your iPad ready…".
    pub fn waiting_for(&self) -> Option<Asset> {
        let needed: &[Asset] = match self.state {
            State::WaitingForIpados { .. } => &[Asset::Palera1n, Asset::Overlay],
            State::CreateAccount { .. } | State::Installing { .. } => {
                &[Asset::Palera1n, Asset::Chain, Asset::RootImage, Asset::PlatformLayer, Asset::Firmware, Asset::KernelPackage]
            }
            _ => &[],
        };
        needed.iter().copied().find(|&a| *self.assets.get(a) != AssetState::Ready)
    }
}

impl Default for Flow {
    fn default() -> Self {
        Flow::new()
    }
}

/// How long a waiting state may take before the flow shows that it's taking
/// longer than usual (the runtime then sends [`Event::Timeout`] once).
/// iPadOS with the overlay: palera1n's boot, iPadOS starting and the
/// companion coming up. The install system: palera1n's boot and Linux.
pub fn timeout(state: &State) -> Option<std::time::Duration> {
    use std::time::Duration;
    match state {
        // The helper answers about 20 s after palera1n is done; well past that
        // the screen offers to start over.
        State::WaitingForIpados { slow: false, .. } => Some(Duration::from_secs(90)),
        // The DFU combination takes 20-25 s with the device gone in between.
        State::WaitingForDfu { leaving: Some(_), .. } => Some(Duration::from_secs(30)),
        State::CreateAccount { env: None, slow: false, .. } => Some(Duration::from_secs(150)),
        _ => None,
    }
}

/// The only way the whole state changes: the screen flow, plus the asset lane
/// that starts work as early as its inputs allow and holds the write back
/// until the system and the board's files are ready.
/// The system this installer installs, from its release ("Omarchy A"); set
/// once at start. Nothing about it is compiled in.
static SYSTEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn set_system_name(name: &str) {
    let _ = SYSTEM.set(name.to_owned());
}

/// The system's name, or a neutral stand-in before the release is known.
pub fn system_name() -> &'static str {
    SYSTEM.get().map(String::as_str).unwrap_or("the system")
}

/// Where a start is, read from its log lines (the newest that says): the
/// Details panel's "Stage". Written for an iPad (see [`for_device`]).
pub fn start_stage(lines: &[String]) -> Option<&'static str> {
    lines.iter().rev().find_map(|l| {
        let l = l.as_str();
        Some(if l.contains("USB port reset") {
            "Resetting the USB port"
        } else if l.contains("Booting PongoOS") || l.contains("Entered download mode") || l.contains("Download mode device found") || l.contains("Found download mode device") {
            "Starting Pongo"
        } else if l.contains("Booting") {
            "Starting iPadOS"
        } else if l.contains("Checkmate") {
            "Checkmate · waiting for the iPad on USB"
        } else if l.contains("Setting up the exploit") || l.contains("setup stage") || l.contains("UaF race") || l.contains("checkm8") {
            "Setting up the exploit"
        } else if l.contains("DFU mode device") || l.contains("Waiting for DFU") {
            "Connecting to the iPad"
        } else {
            return None;
        })
    })
}

/// The kind of device on the cable (0 iPad, 1 iPhone, 2 not known yet), for
/// the words: set whenever the flow knows its device.
static DEVICE_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

pub fn set_device_model(model: Model) {
    let kind = if matches!(model, Model::IphoneHome | Model::IphoneNoHome) { 1 } else { 0 };
    DEVICE_KIND.store(kind, std::sync::atomic::Ordering::Relaxed);
}

/// "iPad" or "iPhone".
pub fn device_word() -> &'static str {
    if DEVICE_KIND.load(std::sync::atomic::Ordering::Relaxed) == 1 { "iPhone" } else { "iPad" }
}

/// "iPadOS" or "iOS".
pub fn device_os() -> &'static str {
    if DEVICE_KIND.load(std::sync::atomic::Ordering::Relaxed) == 1 { "iOS" } else { "iPadOS" }
}

/// TEXT for the device kind: iPad words, or iPhone and iOS for kind 1.
fn words_for(kind: u8, text: &str) -> String {
    let (word, os) = if kind == 1 { ("iPhone", "iOS") } else { ("iPad", "iPadOS") };
    text.replace("iPadOS", "\u{0}").replace("iPad", word).replace('\u{0}', os)
}

/// TEXT, written for an iPad, for the device on the cable: "iPadOS" and
/// "iPad" become "iOS" and "iPhone" for an iPhone. Each distinct result is
/// kept for the program's life, so it serves where a &'static str is due.
pub fn for_device(text: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static SEEN: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let out = words_for(DEVICE_KIND.load(std::sync::atomic::Ordering::Relaxed), text);
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(HashMap::new);
    if let Some(s) = seen.get(&out) {
        return s;
    }
    let leaked: &'static str = Box::leak(out.clone().into_boxed_str());
    seen.insert(out, leaked);
    leaked
}

pub fn step(app: App, flow: Flow, event: Event) -> (Flow, Vec<Effect>) {
    let (flow, effects) = step_inner(app, flow, event);
    // The device on the cable; on the waiting screen the one seen there, if any.
    let seen = match &flow.state {
        State::WaitingForDfu { seen, .. } if seen.len() == 1 => Some(&seen[0].device),
        _ => None,
    };
    if let Some(device) = flow.state.device().or(seen) {
        set_device_model(device.model);
    }
    (flow, effects)
}

fn step_inner(app: App, mut flow: Flow, event: Event) -> (Flow, Vec<Effect>) {
    let mut out = Vec::new();
    let fetch = |assets: Vec<Asset>| assets.into_iter().map(Effect::Fetch);
    match event {
        Event::Started => {
            // The launcher installs nothing: only what starts the device.
            let first: &[Asset] = if app == App::Launcher { &[Asset::Palera1n, Asset::Chain] } else { &assets::AT_START };
            out.extend(fetch(flow.assets.start(first)));
            return (flow, out);
        }
        Event::AskEntry => {
            flow.ask_entry = true;
            return (flow, out);
        }
        Event::EntriesListed(entries)
            if flow.ask_entry && entries.len() == 1 && matches!(flow.state, State::Launching { .. }) =>
        {
            if let State::Launching { device } = flow.state.clone() {
                flow.state = State::ChooseSystem { device, entries, selected: 0 };
            }
            return (flow, out);
        }
        // Nothing the user confirms goes on while other devices are on USB
        // (the screen that asks is covered by "More than one device").
        Event::Continue if flow.refusing() => return (flow, out),
        Event::Others(others) => {
            flow.others = others;
            if flow.others.is_empty() {
                let held = std::mem::take(&mut flow.held);
                route(&mut flow, held, &mut out);
            }
            return (flow, out);
        }
        Event::AssetReady(asset) => {
            flow.assets.set(asset, AssetState::Ready);
            if flow.boot_waiting.is_some() && flow.assets.ready_to_boot_ipados() {
                let waiting: Vec<Effect> = flow.boot_waiting.take().into_iter().collect();
                route(&mut flow, waiting, &mut out);
            }
            if flow.write_waiting && flow.assets.ready_to_write() {
                flow.write_waiting = false;
                out.push(Effect::PrepareInstall);
                // An install started meanwhile goes right after.
                if let Some(account) = flow.start_waiting.take() {
                    out.push(Effect::StartInstall(account));
                }
            }
            return (flow, out);
        }
        Event::InstallRefused(reason) => {
            flow.install_problem = Some(reason);
            return step(app, flow, Event::InstallFinished { ok: false });
        }
        Event::AccountNeeded if matches!(flow.state, State::WaitingForDfu { .. }) => flow.resuming = true,
        // A start that didn't work: its reason shows on DFU mode again (both
        // apps; the launcher's start runs from Launching until m1n1 took the entry).
        Event::ChainFailed(ref reason)
            if matches!(flow.state, State::WaitingForIpados { .. } | State::CreateAccount { env: None, .. })
                || (app == App::Launcher && matches!(flow.state, State::Launching { .. } | State::Starting { .. })) =>
        {
            flow.start_problem = Some(reason.clone());
        }
        Event::SplitRefused(reason) => {
            flow.split_problem = Some(reason);
            return step(app, flow, Event::SplitFinished { ok: false });
        }
        Event::Unresponsive(on) => {
            flow.unresponsive = on;
            return (flow, out);
        }
        Event::BootStalled => {
            flow.boot_stalled = true;
            return (flow, out);
        }
        Event::BootResumed => {
            flow.boot_stalled = false;
            return (flow, out);
        }
        Event::StartAttempt(n) => {
            flow.start_attempt = n;
            flow.boot_stalled = false;
            return (flow, out);
        }
        Event::StartLog(lines) => {
            flow.start_log = lines;
            return (flow, out);
        }
        Event::BootBooting => {
            flow.booting = true;
            return (flow, out);
        }
        Event::EntryStarted => {
            flow.entry_started = true;
            return (flow, out);
        }
        Event::CompanionLost => {
            flow.companion = false;
            return (flow, out);
        }
        Event::CompanionBack => {
            flow.companion = true;
            return (flow, out);
        }
        // A split starts only while the companion can take it.
        Event::Continue if matches!(flow.state, State::ChooseSplit { .. }) && !flow.companion => return (flow, out),
        // A split that erases partitions is confirmed in a dialog first.
        Event::Continue
            if !flow.confirming_split && matches!(&flow.state, State::ChooseSplit { layout, .. } if !layout.outside.is_empty()) =>
        {
            flow.confirming_split = true;
            return (flow, out);
        }
        Event::Cancel if flow.confirming_split => {
            flow.confirming_split = false;
            return step(app, flow, Event::Approve(false));
        }
        Event::AssetFailed(asset, cause, reason) => {
            flow.assets.set(asset, AssetState::Failed(cause, reason));
            return (flow, out);
        }
        Event::RetryAssets => {
            let failed: Vec<Asset> = assets::ALL
                .iter()
                .copied()
                .filter(|&a| matches!(flow.assets.get(a), AssetState::Failed(..)))
                .collect();
            for &a in &failed {
                flow.assets.set(a, AssetState::NotStarted);
            }
            out.extend(fetch(flow.assets.start(&failed)));
            return (flow, out);
        }
        _ => {}
    }
    if matches!(event, Event::IpadosReady { .. }) {
        flow.companion = true;
    }
    // A changed split is a new attempt: the old reason no longer applies.
    if matches!(event, Event::SetOmarchySize(_) | Event::Approve(_)) && matches!(flow.state, State::ChooseSplit { .. }) {
        flow.split_problem = None;
    }
    let (state, effects) = update(app, flow.state, event);
    flow.state = state;
    if !matches!(flow.state, State::ChooseSplit { .. } | State::Splitting { .. } | State::WaitingForIpados { .. }) {
        flow.split_problem = None;
    }
    if !matches!(flow.state, State::InstallFailed { .. }) {
        flow.install_problem = None;
    }
    if !matches!(flow.state, State::CreateAccount { .. }) {
        flow.resuming = false;
    }
    // palera1n's state belongs to the boot under way: the screens it runs behind.
    let booting_screen =
        matches!(flow.state, State::WaitingForIpados { .. } | State::CreateAccount { env: None, .. } | State::Launching { .. });
    if !booting_screen {
        flow.boot_stalled = false;
    }
    if !matches!(flow.state, State::WaitingForIpados { .. }) {
        flow.booting = false;
    }
    if !matches!(flow.state, State::Starting { .. }) {
        flow.entry_started = false;
    }
    if !matches!(flow.state, State::DfuAgain { .. }) {
        flow.start_problem = None;
    }
    if !booting_screen {
        // palera1n's attempts and log belong to the start under way.
        flow.start_attempt = 0;
        flow.start_log.clear();
    }
    if !matches!(flow.state, State::ChooseSplit { .. }) {
        flow.confirming_split = false;
    }
    // A held boot only counts while the flow still waits for it.
    if !matches!(flow.state, State::WaitingForIpados { .. }) {
        flow.boot_waiting = None;
    }
    if !flow.starting(app) {
        flow.held.clear();
    }
    // The board is known from Found on: start its assets once.
    // (The launcher installs nothing: its Chain started at the start.)
    if app == App::Installer && flow.state.device().is_some() && !matches!(flow.state, State::Unsupported { .. }) {
        out.extend(fetch(flow.assets.start(&assets::WITH_BOARD)));
    }
    route(&mut flow, effects, &mut out);
    (flow, out)
}

/// Sends the effects of a step, holding back what must wait: a boot of iPadOS
/// before its overlay is built, a write before its files are in, and any
/// start of the device while other Apple devices are on USB.
fn route(flow: &mut Flow, effects: Vec<Effect>, out: &mut Vec<Effect>) {
    let fetch = |assets: Vec<Asset>| assets.into_iter().map(Effect::Fetch);
    for effect in effects {
        match effect {
            Effect::BootIpadosWithOverlay(_) | Effect::BootInstallEnv(_) | Effect::ListEntries(_) if !flow.others.is_empty() => {
                if matches!(effect, Effect::BootIpadosWithOverlay(_)) {
                    out.extend(fetch(flow.assets.start(&[Asset::Overlay])));
                }
                flow.held.push(effect);
            }
            Effect::BootIpadosWithOverlay(_) => {
                out.extend(fetch(flow.assets.start(&[Asset::Overlay])));
                // Not before the overlay is built (and palera1n is here).
                if flow.assets.ready_to_boot_ipados() {
                    out.push(effect);
                } else {
                    flow.boot_waiting = Some(effect);
                }
            }
            Effect::PrepareInstall if !flow.assets.ready_to_write() => flow.write_waiting = true,
            // Never ahead of a preparation that still waits for its files.
            Effect::StartInstall(account) if flow.write_waiting => flow.start_waiting = Some(account),
            _ => out.push(effect),
        }
    }
}

impl State {
    pub fn start() -> State {
        State::WaitingForDfu { tab: Sequence::HomePower, seen: Vec::new(), leaving: None }
    }

    /// The current wait is taking longer than usual: show the trouble link.
    pub fn slow(&self) -> bool {
        matches!(self, State::WaitingForIpados { slow: true, .. } | State::CreateAccount { slow: true, .. })
    }

    /// The device this state is about, if any.
    /// The device work goes on on. One whose work is over (its system
    /// starts by itself, nothing can be done on it, or it begins again from
    /// DFU mode) is the screen's, but free for other work.
    pub fn working(&self) -> Option<&Device> {
        use State::*;
        match self {
            Unsupported { .. } | InstallFailed { .. } | NothingInstalled { .. } | Starting { .. } => None,
            _ => self.device(),
        }
    }

    pub fn device(&self) -> Option<&Device> {
        use State::*;
        match self {
            WaitingForDfu { .. } | SeveralInDfu { .. } | UsbAccessNeeded => None,
            Unsupported { device }
            | Found { device, .. }
            | WaitingForIpados { device, .. }
            | ChooseSplit { device, .. }
            | Splitting { device, .. }
            | SplitDone { device, .. }
            | DfuAgain { device, .. }
            | CreateAccount { device, .. }
            | ConfirmErase { device, .. }
            | NotPrepared { device }
            | Installing { device, .. }
            | InstallFailed { device }
            | Starting { device, .. }
            | Launching { device }
            | ChooseSystem { device, .. }
            | NothingInstalled { device } => Some(device),
        }
    }
}

/// The only way the state changes.
pub fn update(app: App, state: State, event: Event) -> (State, Vec<Effect>) {
    use Event as E;
    use State as S;
    let none = Vec::new;
    match (state, event) {
        // USB access is needed before anything else can happen.
        // 1.10 asks first; the password prompt comes with "Allow access".
        (_, E::UsbAccessDenied) => (S::UsbAccessNeeded, none()),
        (S::UsbAccessNeeded, E::Continue) => (S::UsbAccessNeeded, vec![Effect::RequestUsbAccess]),
        (S::UsbAccessNeeded, E::UsbAccessGranted) => (S::start(), none()),

        // ---- Connect ----
        (S::WaitingForDfu { seen, leaving, .. }, E::SelectTab(tab)) => (S::WaitingForDfu { tab, seen, leaving }, none()),
        (S::WaitingForDfu { mut seen, tab, .. }, E::Attached { device, mode }) => match mode {
            UsbMode::Dfu => found(device),
            _ => {
                seen.retain(|s| s.device.id != device.id);
                let tab = if seen.is_empty() { device.sequence } else { tab };
                seen.push(Seen { device, mode });
                (S::WaitingForDfu { tab, seen, leaving: None }, none())
            }
        },
        (S::WaitingForDfu { tab, mut seen, leaving }, E::Detached { id }) => {
            let gone = seen.iter().position(|s| s.device.id == id).map(|i| seen.remove(i));
            // The last device shown dropped off: keep showing it for a while.
            let leaving = if seen.is_empty() { gone.or(leaving) } else { None };
            (S::WaitingForDfu { tab, seen, leaving }, none())
        }
        // An install system already running when the installer started (or
        // came back): continue by its phase (the install worker reads it).
        // Relaunched while the iPad still runs this install's iPadOS with the
        // companion: pick up at the split instead of booting it again.
        (S::WaitingForDfu { seen, tab, leaving }, E::IpadosReady { layout }) => match seen.iter().find(|s| s.mode == UsbMode::Normal) {
            Some(s) => {
                let device = s.device.clone();
                let omarchy_gb = suggested_size(&layout);
                (S::ChooseSplit { device, layout, omarchy_gb, approved: false }, none())
            }
            None => (S::WaitingForDfu { seen, tab, leaving }, none()),
        },
        // Nothing planned yet (phase ready): the account form, as after a boot.
        (S::WaitingForDfu { seen, tab, leaving }, E::EnvReady { layout }) => match seen.iter().find(|s| s.mode == UsbMode::Install) {
            Some(s) => {
                let device = s.device.clone();
                if !layout.has_room() {
                    return (S::NotPrepared { device }, none());
                }
                let account = Account::default();
                if !layout.outside.is_empty() {
                    (S::CreateAccount { device, account, env: Some(layout), erase_approved: false, slow: false }, none())
                } else {
                    (S::CreateAccount { device, account, env: Some(layout), erase_approved: false, slow: false }, vec![Effect::PrepareInstall])
                }
            }
            None => (S::WaitingForDfu { seen, tab, leaving }, none()),
        },
        (S::WaitingForDfu { seen, .. }, event @ (E::Reconnected { .. } | E::AccountNeeded | E::InstallFinished { .. }))
            if seen.iter().any(|s| s.mode == UsbMode::Install) =>
        {
            let device = seen.iter().find(|s| s.mode == UsbMode::Install).unwrap().device.clone();
            match event {
                // The plan is written and the account staged: stream (again) from the start.
                E::Reconnected { stream_incomplete: true } => (
                    S::Installing { device, percent: 0, link: Link::Up, streamed: false, step: None },
                    vec![Effect::RestartStream],
                ),
                // The image is in: the device finishes by itself (Streamed follows).
                E::Reconnected { stream_incomplete: false } => {
                    (S::Installing { device, percent: 100, link: Link::Up, streamed: false, step: None }, none())
                }
                E::AccountNeeded => (
                    S::CreateAccount {
                        device,
                        account: Account::default(),
                        env: Some(Layout { total_gb: 0, ipados_gb: 0, ipados_used_gb: 0, outside: Vec::new(), outside_bytes: 0 }),
                        erase_approved: true,
                        slow: false,
                    },
                    none(),
                ),
                E::InstallFinished { ok: true } => (
                    S::Starting { device, title: system_name().into() },
                    vec![Effect::Notify(format!("{} is installed.", system_name()))],
                ),
                _ => (S::InstallFailed { device }, none()),
            }
        }
        // It didn't come back in time: back to waiting.
        (S::WaitingForDfu { tab, seen, leaving: Some(_) }, E::Timeout) => (S::WaitingForDfu { tab, seen, leaving: None }, none()),
        (S::Found { device: first, .. } | S::Unsupported { device: first }, E::Attached { device, mode: UsbMode::Dfu })
            if first.id != device.id =>
        {
            (S::SeveralInDfu { devices: vec![first, device] }, none())
        }
        (S::SeveralInDfu { mut devices }, E::Attached { device, mode: UsbMode::Dfu }) => {
            if !devices.iter().any(|d| d.id == device.id) {
                devices.push(device);
            }
            (S::SeveralInDfu { devices }, none())
        }
        (S::SeveralInDfu { mut devices }, E::Detached { id }) => {
            devices.retain(|d| d.id != id);
            match devices.len() {
                0 => (S::start(), none()),
                1 => found(devices.remove(0)),
                _ => (S::SeveralInDfu { devices }, none()),
            }
        }
        (S::Found { device, .. } | S::Unsupported { device }, E::Detached { id }) if device.id == id => {
            (S::start(), none())
        }
        // The launcher starts the device only when asked to (L.1a's Start).
        (S::Found { device, .. }, E::Continue) if app == App::Launcher => launch(device),
        (S::Found { device, .. }, E::Continue) => {
            let id = device.id.clone();
            (S::WaitingForIpados { device, slow: false }, vec![Effect::BootIpadosWithOverlay(id)])
        }
        (S::Found { device, .. }, E::Skip) => (S::Found { device, confirm_skip: true }, none()),
        (S::Found { device, .. }, E::CancelSkip) => (S::Found { device, confirm_skip: false }, none()),
        (S::Found { device, confirm_skip: true }, E::ConfirmSkip) => install_from_dfu(device),

        // ---- Prepare ----
        (S::WaitingForIpados { device, .. }, E::IpadosReady { layout }) => {
            let omarchy_gb = suggested_size(&layout);
            (S::ChooseSplit { device, layout, omarchy_gb, approved: false }, none())
        }
        // The iPad left the temporary iPadOS for DFU (e.g. restarted into it):
        // its companion is gone, so start from Found again.
        (S::ChooseSplit { device: d, .. }, E::Attached { device, mode: UsbMode::Dfu }) if d.id == device.id => found(device),
        (S::ChooseSplit { device, layout, approved, .. }, E::SetOmarchySize(gb)) => {
            let omarchy_gb = gb.min(layout.max_omarchy_gb());
            (S::ChooseSplit { device, layout, omarchy_gb, approved }, none())
        }
        // "Check again": the companion reads the storage anew (IpadosReady).
        (state @ S::ChooseSplit { .. }, E::Retry) => (state, vec![Effect::RefreshLayout]),
        (S::ChooseSplit { device, layout, omarchy_gb, .. }, E::Approve(approved)) => {
            (S::ChooseSplit { device, layout, omarchy_gb, approved }, none())
        }
        (S::ChooseSplit { device, layout, omarchy_gb, approved: true }, E::Continue)
            if omarchy_gb >= MIN_OMARCHY_GB && omarchy_gb <= layout.max_omarchy_gb() =>
        {
            (
                S::Splitting { device, omarchy_gb, link: Link::Up, reopened: false },
                vec![Effect::StartSplit { omarchy_gb }],
            )
        }
        (S::ChooseSplit { device, .. }, E::IpadosReady { layout }) => {
            // "Check again" after the user freed up space.
            let omarchy_gb = suggested_size(&layout);
            (S::ChooseSplit { device, layout, omarchy_gb, approved: false }, none())
        }
        (S::Splitting { device, omarchy_gb, reopened, .. }, E::Detached { id }) if device.id == id => {
            (S::Splitting { device, omarchy_gb, link: Link::Lost, reopened }, none())
        }
        (S::Splitting { device: d, omarchy_gb, reopened, .. }, E::Attached { device, mode }) if d.id == device.id => {
            match mode {
                // Still running iPadOS: re-attach, never restart the chain.
                UsbMode::Normal => (S::Splitting { device, omarchy_gb, link: Link::Up, reopened }, none()),
                // It reset: the result is unknown until the installer looks.
                _ => (S::DfuAgain { device, why: Again::CheckSplit, seen: None }, none()),
            }
        }
        (S::Splitting { device, omarchy_gb, .. }, E::SplitFinished { ok: true }) => {
            (
                S::SplitDone { device, omarchy_gb },
                vec![Effect::Notify("The storage is split.".into())],
            )
        }
        // The engine stopped without the iPad restarting: the storage is as
        // it was. The companion reads the layout again (IpadosReady) and the
        // split is chosen anew; the split screen shows why it stopped.
        (S::Splitting { device, link: Link::Up, .. }, E::SplitFinished { ok: false }) => {
            (S::WaitingForIpados { device, slow: false }, none())
        }
        (S::Splitting { device, .. }, E::SplitFinished { ok: false }) => {
            (S::DfuAgain { device, why: Again::CheckSplit, seen: None }, none())
        }
        (S::SplitDone { device, .. }, E::Continue) => (S::DfuAgain { device, why: Again::Install, seen: None }, none()),
        (_, E::SplitRunning { device, omarchy_gb }) => {
            (S::Splitting { device, omarchy_gb, link: Link::Lost, reopened: true }, none())
        }

        // ---- DFU again ----
        // The launcher's route after a start that didn't work: again, as the
        // installer does (Start was pressed for this device).
        (S::DfuAgain { device: d, .. }, E::Attached { device, mode: UsbMode::Dfu }) if app == App::Launcher && d.id == device.id => {
            launch(device)
        }
        (S::DfuAgain { device: d, why, .. }, E::Attached { device, mode: UsbMode::Dfu }) if d.id == device.id => {
            match why {
                Again::Install | Again::Retry => install_from_dfu(device),
                Again::Split | Again::CheckSplit | Again::RetryPrepare => {
                    let id = device.id.clone();
                    (S::WaitingForIpados { device, slow: false }, vec![Effect::BootIpadosWithOverlay(id)])
                }
            }
        }

        // ---- Install ----
        (S::CreateAccount { device, env, erase_approved, slow, .. }, E::EditAccount(account)) => {
            (S::CreateAccount { device, account, env, erase_approved, slow }, none())
        }
        (S::CreateAccount { device, account, erase_approved, .. }, E::EnvReady { layout }) => {
            if !layout.has_room() {
                return (S::NotPrepared { device }, none());
            }
            if !layout.outside.is_empty() && !erase_approved {
                // Nothing is written before the erase is approved, which is
                // asked when the user presses Install (no interruption while
                // typing).
                return (S::CreateAccount { device, account, env: Some(layout), erase_approved, slow: false }, none());
            }
            (
                S::CreateAccount { device, account, env: Some(layout), erase_approved, slow: false },
                vec![Effect::PrepareInstall],
            )
        }
        (S::ConfirmErase { device, account, layout, .. }, E::Approve(approved)) => {
            (S::ConfirmErase { device, account, layout, approved }, none())
        }
        // Approved: prepare (write the plan, stage the files) and install.
        (S::ConfirmErase { device, account, approved: true, .. }, E::Continue) => (
            S::Installing { device, percent: 0, link: Link::Up, streamed: false, step: None },
            vec![Effect::PrepareInstall, Effect::StartInstall(account)],
        ),
        (S::ConfirmErase { device, account, layout, .. }, E::Cancel) => {
            (S::CreateAccount { device, account, env: Some(layout), erase_approved: false, slow: false }, none())
        }
        // Install with partitions to erase: confirm first (a dialog over the form).
        (S::CreateAccount { device, account, env: Some(layout), erase_approved: false, .. }, E::Continue)
            if account.valid() && !layout.outside.is_empty() =>
        {
            (S::ConfirmErase { device, account, layout, approved: false }, none())
        }
        (S::CreateAccount { device, account, env: Some(_), .. }, E::Continue) if account.valid() => (
            S::Installing { device, percent: 0, link: Link::Up, streamed: false, step: None },
            vec![Effect::StartInstall(account)],
        ),
        (S::NotPrepared { device }, E::Continue) => (S::DfuAgain { device, why: Again::Split, seen: None }, none()),
        // Already in DFU when the next step would ask for it: take the step.
        (S::NotPrepared { device: d }, E::Attached { device, mode: UsbMode::Dfu }) if d.id == device.id => {
            let id = device.id.clone();
            (S::WaitingForIpados { device, slow: false }, vec![Effect::BootIpadosWithOverlay(id)])
        }
        (S::SplitDone { device: d, .. } | S::InstallFailed { device: d }, E::Attached { device, mode: UsbMode::Dfu }) if d.id == device.id => {
            install_from_dfu(device)
        }
        // The install system went away (restarted into DFU): boot it again,
        // keeping what was typed.
        (S::CreateAccount { device: d, account, env: Some(_), .. } | S::ConfirmErase { device: d, account, .. }, E::Attached { device, mode: UsbMode::Dfu })
            if d.id == device.id =>
        {
            let id = device.id.clone();
            (S::CreateAccount { device, account, env: None, erase_approved: false, slow: false }, vec![Effect::BootInstallEnv(id)])
        }
        // The image was in and the installed system is up: the device's "done"
        // got lost on the way, the install finished anyway.
        (S::Installing { device: d, streamed: true, .. }, E::Attached { device, mode: UsbMode::Installed }) if d.id == device.id => (
            S::Starting { device, title: system_name().into() },
            vec![Effect::Notify(format!("{} is installed.", system_name()))],
        ),
        (S::Installing { device, link, streamed, step, .. }, E::InstallProgress(percent)) => {
            (S::Installing { device, percent: percent.min(100), link, streamed, step }, none())
        }
        (S::Installing { device, link, step, .. }, E::Streamed) => {
            (S::Installing { device, percent: 100, link, streamed: true, step }, none())
        }
        (S::Installing { device, link, .. }, E::InstallStep(step)) => {
            (S::Installing { device, percent: 100, link, streamed: true, step: Some(step) }, none())
        }
        (S::Installing { device, percent, streamed, step, .. }, E::Detached { id }) if device.id == id => {
            (S::Installing { device, percent, link: Link::Lost, streamed, step }, none())
        }
        // The installed system came up: the device finished while unplugged.
        (S::Installing { device: d, streamed: true, .. }, E::Attached { device, mode: UsbMode::Installed }) if d.id == device.id => {
            (S::Starting { device, title: system_name().into() }, vec![Effect::Notify(format!("{} is installed.", system_name()))])
        }
        (S::Installing { device: d, percent, streamed, step, .. }, E::Attached { device, .. }) if d.id == device.id => {
            (S::Installing { device, percent, link: Link::Up, streamed, step }, none())
        }
        (S::Installing { device, streamed, step, .. }, E::Reconnected { stream_incomplete: true }) if !streamed => {
            (S::Installing { device, percent: 0, link: Link::Up, streamed, step }, vec![Effect::RestartStream])
        }
        (S::Installing { device, percent, streamed, step, .. }, E::Reconnected { .. }) => {
            (S::Installing { device, percent, link: Link::Up, streamed, step }, none())
        }
        (S::Installing { device, .. }, E::InstallFinished { ok: true }) => {
            (S::Starting { device, title: system_name().into() }, vec![Effect::Notify(format!("{} is installed.", system_name()))])
        }
        (S::Installing { device, .. }, E::InstallFinished { ok: false }) => (S::InstallFailed { device }, none()),
        // The install system couldn't be opened or prepared: no endless wait on 3.2.
        (S::CreateAccount { device, .. }, E::InstallFinished { ok: false }) => (S::InstallFailed { device }, none()),
        (S::InstallFailed { device }, E::Retry) => (S::DfuAgain { device, why: Again::Retry, seen: None }, none()),

        // ---- Launcher ----
        // Detaches while launching change nothing: the chain (palera1n, Pongo,
        // m1n1) drops the device off USB a few times, and only its own result
        // moves on.
        (S::Launching { device }, E::EntriesListed(entries)) => match entries.len() {
            0 => (S::NothingInstalled { device }, none()),
            1 => {
                let entry = &entries[0];
                (
                    S::Starting { device, title: entry.title.clone() },
                    vec![Effect::BootEntry(entry.id.clone())],
                )
            }
            _ => {
                let selected = entries.iter().position(|e| e.default).unwrap_or(0);
                (S::ChooseSystem { device, entries, selected }, none())
            }
        },
        (S::ChooseSystem { device, entries, .. }, E::SelectEntry(i)) if i < entries.len() => {
            (S::ChooseSystem { device, entries, selected: i }, none())
        }
        (S::ChooseSystem { device, entries, selected }, E::Continue) => {
            let entry = entries[selected].clone();
            (S::Starting { device, title: entry.title }, vec![Effect::BootEntry(entry.id)])
        }

        // ---- Waits that take long, chains that fail, other modes ----
        // Detaches while booting are normal (the device re-enumerates between
        // stages); a real loss shows up as a timeout.
        (S::WaitingForIpados { device, .. }, E::Timeout) => (S::WaitingForIpados { device, slow: true }, none()),
        // The install system never reported in: DFU mode again, boot once more.
        (S::CreateAccount { device, env: None, slow: true, .. }, E::Retry) => {
            (S::DfuAgain { device, why: Again::Retry, seen: None }, none())
        }
        // iPadOS never answered: put it in DFU mode again and boot once more.
        (S::WaitingForIpados { device, slow: true }, E::Retry) => (S::DfuAgain { device, why: Again::RetryPrepare, seen: None }, none()),
        (S::CreateAccount { device, account, env: None, erase_approved, .. }, E::Timeout) => {
            (S::CreateAccount { device, account, env: None, erase_approved, slow: true }, none())
        }
        (S::WaitingForIpados { device, .. }, E::ChainFailed(_)) => {
            (S::DfuAgain { device, why: Again::RetryPrepare, seen: None }, none())
        }
        (S::CreateAccount { device, env: None, .. }, E::ChainFailed(_)) => {
            (S::DfuAgain { device, why: Again::Retry, seen: None }, none())
        }
        // The launcher's start (palera1n, Pongo, m1n1, the entry) failed: the same.
        (S::Launching { device } | S::Starting { device, .. }, E::ChainFailed(_)) if app == App::Launcher => {
            (S::DfuAgain { device, why: Again::Retry, seen: None }, none())
        }
        (S::DfuAgain { device: d, why, .. }, E::Attached { device, mode }) if d.id == device.id => {
            (S::DfuAgain { device, why, seen: Some(mode) }, none())
        }
        (state @ S::Starting { .. }, E::Continue) => (state, vec![Effect::Quit]),
        // Quitting from the temporary iPadOS restarts the iPad into normal iPadOS.
        (state @ (S::WaitingForIpados { .. } | S::ChooseSplit { .. } | S::SplitDone { .. }), E::Quit) => {
            (state, vec![Effect::RestartIpad, Effect::Quit])
        }
        (state, E::Quit) => (state, vec![Effect::Quit]),

        // Anything else leaves the state as it is.
        (state, _) => (state, none()),
    }
}

/// A device just entered DFU while the app waited for it.
fn found(device: Device) -> (State, Vec<Effect>) {
    if device.support == Support::Unsupported {
        return (State::Unsupported { device }, Vec::new());
    }
    (State::Found { device, confirm_skip: false }, Vec::new())
}

/// The launcher starts the device in DFU mode: m1n1, then what's installed.
fn launch(device: Device) -> (State, Vec<Effect>) {
    let id = device.id.clone();
    (State::Launching { device }, vec![Effect::ListEntries(id)])
}

/// The device is in DFU and the storage is (believed to be) split: boot the
/// install environment and ask for the account meanwhile.
fn install_from_dfu(device: Device) -> (State, Vec<Effect>) {
    let id = device.id.clone();
    (
        State::CreateAccount {
            device,
            account: Account::default(),
            env: None,
            erase_approved: false,
            slow: false,
        },
        vec![Effect::BootInstallEnv(id)],
    )
}

/// The split the slider opens at: the existing one if the storage was split
/// before, otherwise the recommended size if it fits.
/// Where the divider starts: the existing split when there is one; else half
/// the storage, leaving iPadOS what it uses plus [`SUGGESTED_BUFFER_GB`], and
/// never below the minimum (within what fits at all).
fn suggested_size(layout: &Layout) -> u32 {
    let max = layout.max_omarchy_gb();
    if layout.outside_gb() > 0 {
        return layout.outside_gb().min(max);
    }
    let half = layout.total_gb / 2;
    let leaving = layout.total_gb.saturating_sub(layout.ipados_used_gb + SUGGESTED_BUFFER_GB);
    half.min(leaving).max(MIN_OMARCHY_GB).min(max)
}

#[cfg(test)]
mod device_words {
    use super::*;

    #[test]
    fn a_start_that_didnt_work_keeps_its_reason_for_dfu_mode_again() {
        let device = Device { id: DeviceId("e".into()), board: "j71b".into(), name: "iPad".into(), model: Model::IpadHome, sequence: Sequence::HomePower, support: Support::Supported };
        let flow = Flow { state: State::WaitingForIpados { device, slow: false }, ..Flow::new() };
        let (flow, _) = step(App::Installer, flow, Event::StartAttempt(3));
        assert_eq!(flow.start_attempt, 3);
        let (flow, _) = step(App::Installer, flow, Event::ChainFailed("palera1n: Unable to perform checkm8".into()));
        assert!(matches!(flow.state, State::DfuAgain { why: Again::RetryPrepare, .. }));
        assert_eq!(flow.start_problem.as_deref(), Some("palera1n: Unable to perform checkm8"));
        assert_eq!(flow.start_attempt, 0);
    }

    #[test]
    fn texts_follow_the_device() {
        assert_eq!(words_for(1, "Your iPad restarts into iPadOS."), "Your iPhone restarts into iOS.");
        assert_eq!(words_for(0, "Your iPad restarts into iPadOS."), "Your iPad restarts into iPadOS.");
    }
}

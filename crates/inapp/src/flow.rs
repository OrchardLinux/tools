//! The installer's state: one stage at a time, changed only by `handle`, with
//! no I/O. What the device and the work report comes in as events; what has
//! to happen goes out as effects, which the engine carries out.
//!
//! The stages follow the screens. Everything else the screens show is read
//! from the last storage report, the release and the downloads, never kept
//! twice.
use orchard_boards::install_plan::{BOOT_BYTES, MIN_ROOT_BYTES};
use serde::Serialize;

const GB: u64 = 1_000_000_000;
const MIB: u64 = 1 << 20;
/// Room iOS keeps beyond what it uses, where the divider starts (the
/// tethered installer's rule).
const SUGGESTED_BUFFER: u64 = 10 * GB;
/// Free space iOS keeps on top of what a shrink leaves it and what is still
/// to be downloaded.
const HEADROOM: u64 = GB;
/// A split the engine has not confirmed for this long is shown as reconnecting.
const QUIET_SECONDS: f64 = 15.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub name: String,
    /// `iPhone`, `iPad` or `iPod touch`.
    pub kind: String,
    /// `iOS` or `iPadOS`.
    pub os: String,
}

/// A partition after iOS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outside {
    #[serde(skip)]
    pub id: String,
    pub name: String,
    pub filesystem: String,
    pub bytes: u64,
}

/// The storage as last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    /// Names this exact layout; a split is asked for against it.
    pub revision: String,
    pub sector: u64,
    /// iOS's partition.
    pub ios_bytes: u64,
    pub ios_used_bytes: u64,
    /// The least iOS says it can shrink to.
    pub ios_min_bytes: Option<u64>,
    /// All the space after iOS, free space included.
    pub outside_bytes: u64,
    pub outside: Vec<Outside>,
    /// Why this layout cannot be installed to, when it can't.
    pub issue: Option<String>,
}

impl Storage {
    pub fn total_bytes(&self) -> u64 {
        self.ios_bytes + self.outside_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Running,
    Completed,
    Stopped,
}

/// Which of the three lines of the splitting screen the engine is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SplitStep {
    Check,
    Resize,
    Verify,
}

/// The engine's split, as last reported.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    pub id: String,
    pub status: OperationStatus,
    pub message: Option<String>,
    pub target_ios_bytes: u64,
    pub step: SplitStep,
    /// Seconds since it started and since the engine last confirmed it.
    pub elapsed_seconds: f64,
    pub confirmed_seconds_ago: Option<f64>,
}

/// What is installed, known once the release and the board data are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub system: String,
    pub device_name: String,
    /// Whether this device can be installed to.
    pub supported: bool,
    pub user: String,
    /// The least space the system needs after iOS.
    pub needed_bytes: u64,
    /// What the downloads still take on iOS's side.
    pub download_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Downloads {
    Downloading,
    Ready,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub password: String,
    pub pin: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// What the app opens with. The user goes on once the storage, the
    /// release and the board are known.
    Welcome,
    Unsupported,
    Split { refused: Option<String> },
    /// `operation` is the engine's id for the split, once it has started.
    Splitting { target_ios_bytes: u64, operation: Option<String> },
    SplitDone,
    Account,
    Writing { percent: u8 },
    Finishing,
    Installed { seconds: f64 },
    Failed { message: String, detail: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Target(Target),
    Storage { storage: Storage, operation: Option<Operation> },
    /// The engine took the split and named it.
    SplitStarted(String),
    SplitRefused(String),
    Downloads(Downloads),
    /// A download or the firmware build failed in a way waiting won't mend.
    DownloadFailed(String),
    /// Bytes still to download changed.
    Remaining(u64),
    Progress(u8),
    FinishingUp,
    Installed { seconds: f64 },
    InstallFailed { message: String, detail: String },
    // What the user does.
    Split { ios_bytes: u64 },
    Keep,
    Check,
    Continue,
    Install(Account),
    Retry,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Have the engine resize iOS and erase the partitions after it.
    Split { ios_bytes: u64, revision: String, replacing: Vec<String> },
    /// Have the engine read the storage again now.
    Refresh,
    /// Downloads run, or wait (while iOS is resized).
    Downloads(bool),
    Install(Account),
}

/// A request the stage or the storage does not allow; nothing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

fn refused<T>(message: &str) -> Result<T, Refused> {
    Err(Refused(message.into()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub stage: Stage,
    pub device: Device,
    pub target: Option<Target>,
    pub storage: Option<Storage>,
    pub operation: Option<Operation>,
    pub downloads: Downloads,
    /// The system's name until the release says it (the installer's stamp).
    pub system: Option<String>,
    /// Why the downloads cannot finish, until the user tries again.
    pub broken: Option<String>,
}

impl Flow {
    pub fn new(device: Device) -> Flow {
        Flow { stage: Stage::Welcome, device, target: None, storage: None, operation: None, downloads: Downloads::Downloading, system: None, broken: None }
    }

    /// The limits of a split, from the storage and the release.
    fn limits(&self) -> Option<Limits> {
        let (storage, target) = (self.storage.as_ref()?, self.target.as_ref()?);
        let total = storage.total_bytes();
        let needed = target.needed_bytes;
        // iOS's own minimum, plus what is still to arrive on its side.
        let min = storage.ios_min_bytes.map(|m| align_up(m + target.download_bytes + HEADROOM, storage.sector));
        let max = align_down(total.saturating_sub(needed), storage.sector);
        let fits = storage.outside_bytes >= needed;
        let floor = min.unwrap_or(storage.ios_used_bytes);
        let suggested = if fits {
            storage.ios_bytes
        } else {
            let system = (total / 2).min(total.saturating_sub(storage.ios_used_bytes + SUGGESTED_BUFFER)).max(needed);
            // Whole gigabytes, as the divider moves.
            (total.saturating_sub(system) / GB * GB).clamp(floor.min(max), max)
        };
        Some(Limits { min, max, suggested, needed, fits })
    }

    pub fn handle(&mut self, event: Event) -> Result<Vec<Effect>, Refused> {
        let mut effects = Vec::new();
        match event {
            Event::Target(target) => {
                let supported = target.supported;
                self.target = Some(target);
                if self.stage == Stage::Welcome {
                    if !supported {
                        self.stage = Stage::Unsupported;
                    } else if !self.split_runs() {
                        // As early as it can be known that they are wanted.
                        effects.push(Effect::Downloads(true));
                    }
                }
            }
            Event::Storage { storage, operation } => {
                self.storage = Some(storage);
                self.operation = operation;
                // A split that was running before this helper started is
                // followed at once, whatever the app was showing.
                if let (Stage::Welcome, Some(op)) = (&self.stage, &self.operation) {
                    if op.status == OperationStatus::Running {
                        self.stage = Stage::Splitting { target_ios_bytes: op.target_ios_bytes, operation: Some(op.id.clone()) };
                        effects.push(Effect::Downloads(false));
                    }
                }
                self.follow_split(&mut effects);
            }
            Event::SplitStarted(id) => {
                if let Stage::Splitting { operation, .. } = &mut self.stage {
                    *operation = Some(id);
                }
            }
            Event::SplitRefused(why) => {
                if matches!(self.stage, Stage::Splitting { .. }) {
                    self.stage = Stage::Split { refused: Some(why) };
                    effects.push(Effect::Downloads(true));
                }
            }
            Event::Downloads(state) => self.downloads = state,
            Event::DownloadFailed(detail) => self.broken = Some(detail),
            Event::Remaining(bytes) => {
                if let Some(target) = &mut self.target {
                    target.download_bytes = bytes;
                }
            }
            Event::Progress(percent) => {
                if matches!(self.stage, Stage::Writing { .. }) {
                    self.stage = Stage::Writing { percent: percent.min(100) };
                }
            }
            Event::FinishingUp => {
                if matches!(self.stage, Stage::Writing { .. }) {
                    self.stage = Stage::Finishing;
                }
            }
            Event::Installed { seconds } => {
                if matches!(self.stage, Stage::Writing { .. } | Stage::Finishing) {
                    self.stage = Stage::Installed { seconds };
                }
            }
            Event::InstallFailed { message, detail } => {
                if matches!(self.stage, Stage::Writing { .. } | Stage::Finishing) {
                    self.stage = Stage::Failed { message, detail };
                }
            }
            Event::Split { ios_bytes } => {
                let (Stage::Split { .. }, Some(storage), Some(limits)) = (&self.stage, &self.storage, self.limits()) else {
                    return refused("The storage can't be split now.");
                };
                let ios_bytes = align_down(ios_bytes, storage.sector);
                if ios_bytes > limits.max || limits.min.is_some_and(|min| ios_bytes < min) {
                    return refused("That size is outside what the storage allows.");
                }
                effects.push(Effect::Downloads(false));
                effects.push(Effect::Split {
                    ios_bytes,
                    revision: storage.revision.clone(),
                    replacing: storage.outside.iter().map(|p| p.id.clone()).collect(),
                });
                self.stage = Stage::Splitting { target_ios_bytes: ios_bytes, operation: None };
            }
            Event::Keep => {
                if !matches!(self.stage, Stage::Split { .. }) || !self.limits().is_some_and(|l| l.fits) {
                    return refused("The storage has no room for the system yet.");
                }
                self.stage = Stage::Account;
            }
            Event::Check => {
                if !matches!(self.stage, Stage::Split { .. }) {
                    return refused("The storage can't be checked now.");
                }
                self.stage = Stage::Split { refused: None };
                effects.push(Effect::Refresh);
            }
            Event::Continue => match self.stage {
                Stage::Welcome => {
                    let (Some(_), Some(storage)) = (&self.target, &self.storage) else {
                        return refused("The storage is still being checked.");
                    };
                    self.stage = match &storage.issue {
                        Some(issue) => Stage::Failed { message: "The storage isn't laid out the way the installer knows.".into(), detail: issue.clone() },
                        None => Stage::Split { refused: None },
                    };
                }
                Stage::SplitDone => self.stage = Stage::Account,
                _ => return refused("There is nothing to continue from."),
            },
            Event::Install(account) => {
                if self.stage != Stage::Account {
                    return refused("The install can't start now.");
                }
                if self.downloads != Downloads::Ready {
                    return refused("The download hasn't finished yet.");
                }
                if account.password.chars().count() < 8 || account.password.contains(['\n', '\0']) {
                    return refused("The password needs 8 characters or more.");
                }
                if account.pin.len() != 6 || !account.pin.bytes().all(|b| b.is_ascii_digit()) {
                    return refused("The PIN needs exactly 6 digits.");
                }
                self.stage = Stage::Writing { percent: 0 };
                effects.push(Effect::Install(account));
            }
            Event::Retry => {
                if !matches!(self.stage, Stage::Failed { .. }) {
                    return refused("There is nothing to try again.");
                }
                // The storage decides anew: a failed install may have left
                // partitions, which the next one erases.
                self.broken = None;
                self.stage = if self.limits().is_some_and(|l| l.fits) { Stage::Account } else { Stage::Split { refused: None } };
                effects.push(Effect::Downloads(true));
            }
        }
        // Without its downloads the install cannot start: said where the
        // user would wait for them.
        if let (Stage::Account, Some(detail)) = (&self.stage, &self.broken) {
            let message = format!("Something went wrong while downloading it. {} and your data weren't touched.", self.device.os);
            self.stage = Stage::Failed { message, detail: detail.clone() };
        }
        Ok(effects)
    }

    fn split_runs(&self) -> bool {
        self.operation.as_ref().is_some_and(|op| op.status == OperationStatus::Running)
    }

    /// Whether everything the split screen needs is known.
    fn checked(&self) -> bool {
        self.target.is_some() && self.storage.is_some()
    }

    /// The split's end, as the engine reports it for the operation it named.
    fn follow_split(&mut self, effects: &mut Vec<Effect>) {
        let (Stage::Splitting { operation: Some(id), .. }, Some(op)) = (&self.stage, &self.operation) else { return };
        if &op.id != id {
            return;
        }
        match op.status {
            OperationStatus::Running => {}
            OperationStatus::Completed => {
                self.stage = Stage::SplitDone;
                effects.push(Effect::Downloads(true));
            }
            OperationStatus::Stopped => {
                self.stage = Stage::Split { refused: Some(op.message.clone().unwrap_or_else(|| "The split stopped.".into())) };
                effects.push(Effect::Downloads(true));
            }
        }
    }

    /// What the app shows (PROTOCOL.md).
    pub fn view(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let screen = match &self.stage {
            Stage::Welcome => "welcome",
            Stage::Unsupported => "unsupported",
            Stage::Split { .. } => "split",
            Stage::Splitting { .. } => "splitting",
            Stage::SplitDone => "split_done",
            Stage::Account => "account",
            Stage::Writing { .. } | Stage::Finishing => "installing",
            Stage::Installed { .. } => "installed",
            Stage::Failed { .. } => "failed",
        };
        let mut device = json!(self.device);
        // The model's name comes with the board data; until then there is none.
        device["name"] = json!(self.target.as_ref().map(|t| t.device_name.clone()));
        let storage = self.storage.as_ref().map(|s| {
            json!({"total_bytes": s.total_bytes(), "ios_bytes": s.ios_bytes, "ios_used_bytes": s.ios_used_bytes,
                   "outside_bytes": s.outside_bytes, "outside": s.outside})
        });
        let mut view = json!({
            "screen": screen,
            "system": self.target.as_ref().map(|t| t.system.clone()).or_else(|| self.system.clone()),
            "supported": self.target.as_ref().map(|t| t.supported),
            "device": device,
            "storage": storage,
            "welcome": Value::Null, "split": Value::Null, "splitting": Value::Null, "account": Value::Null,
            "installing": Value::Null, "installed": Value::Null, "failed": Value::Null,
        });
        match &self.stage {
            Stage::Welcome => view["welcome"] = json!({"checked": self.checked()}),
            Stage::Split { refused } => {
                if let Some(l) = self.limits() {
                    view["split"] = json!({"min_ios_bytes": l.min, "max_ios_bytes": l.max, "suggested_ios_bytes": l.suggested,
                                           "needed_bytes": l.needed, "fits": l.fits, "refused": refused});
                }
            }
            Stage::Splitting { target_ios_bytes, operation } => {
                let op = self.operation.as_ref().filter(|op| Some(&op.id) == operation.as_ref());
                let at = op.map_or(SplitStep::Check, |op| op.step);
                let grows = self.storage.as_ref().is_some_and(|s| *target_ios_bytes > s.ios_bytes);
                let state = |step: SplitStep| match step.cmp(&at) {
                    std::cmp::Ordering::Less => "done",
                    std::cmp::Ordering::Equal => "active",
                    std::cmp::Ordering::Greater => "pending",
                };
                let os = &self.device.os;
                let resize = if grows { format!("Grow {os}") } else { format!("Shrink {os}") };
                let quiet = op.and_then(|op| op.confirmed_seconds_ago);
                view["splitting"] = json!({
                    "target_ios_bytes": target_ios_bytes,
                    "steps": [{"title": "Check storage", "state": state(SplitStep::Check)},
                              {"title": resize, "state": state(SplitStep::Resize)},
                              {"title": "Check the result", "state": state(SplitStep::Verify)}],
                    "confirmed_seconds_ago": quiet.map(|s| s.round() as u64),
                    "elapsed_seconds": op.map_or(0, |op| op.elapsed_seconds.round() as u64),
                    "reconnecting": quiet.is_some_and(|s| s > QUIET_SECONDS),
                });
            }
            Stage::Account => {
                let erases = self.storage.as_ref().map(|s| s.outside.clone()).unwrap_or_default();
                view["account"] = json!({"user": self.target.as_ref().map(|t| t.user.clone()), "download": self.downloads, "erases": erases});
            }
            Stage::Writing { percent } => view["installing"] = json!({"phase": "writing", "percent": percent}),
            Stage::Finishing => view["installing"] = json!({"phase": "finishing", "percent": 100}),
            Stage::Installed { seconds } => view["installed"] = json!({"seconds": (seconds * 10.0).round() / 10.0}),
            Stage::Failed { message, detail } => view["failed"] = json!({"message": message, "detail": detail}),
            _ => {}
        }
        view
    }
}

struct Limits {
    min: Option<u64>,
    max: u64,
    suggested: u64,
    needed: u64,
    fits: bool,
}

fn align_down(value: u64, step: u64) -> u64 {
    value / step.max(1) * step.max(1)
}

fn align_up(value: u64, step: u64) -> u64 {
    value.div_ceil(step.max(1)) * step.max(1)
}

/// The least space the system needs after iOS: the boot partition, a root
/// partition that holds what the release unpacks with a quarter to spare and
/// is never under orchard's minimum, and the megabytes alignment can cost.
pub fn needed_bytes(image_expanded: u64, layer_expanded: u64) -> u64 {
    let root = MIN_ROOT_BYTES.max((image_expanded + layer_expanded) / 4 * 5);
    BOOT_BYTES + root + 2 * MIB
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        Device { name: "iPhone".into(), kind: "iPhone".into(), os: "iOS".into() }
    }

    fn target() -> Target {
        Target { system: "Omarchy A".into(), device_name: "iPhone XR".into(), supported: true, user: "tablet".into(), needed_bytes: needed_bytes(3_339_198_464, 26_815_994), download_bytes: 0 }
    }

    fn storage(ios: u64, outside: u64) -> Storage {
        Storage { revision: "r1".into(), sector: 4096, ios_bytes: ios * GB, ios_used_bytes: 41 * GB, ios_min_bytes: Some(44 * GB), outside_bytes: outside * GB, outside: vec![], issue: None }
    }

    fn operation(status: OperationStatus) -> Operation {
        Operation { id: "op".into(), status, message: None, target_ios_bytes: 96 * GB, step: SplitStep::Resize, elapsed_seconds: 12.0, confirmed_seconds_ago: Some(4.0) }
    }

    fn at_split(ios: u64, outside: u64) -> Flow {
        let mut flow = Flow::new(device());
        let effects = flow.handle(Event::Target(target())).unwrap();
        assert_eq!(effects, [Effect::Downloads(true)], "downloads start as soon as the device is known to be supported");
        flow.handle(Event::Storage { storage: storage(ios, outside), operation: None }).unwrap();
        assert_eq!(flow.view()["welcome"]["checked"], true);
        flow.handle(Event::Continue).unwrap();
        flow
    }

    fn account() -> Account {
        Account { password: "correct horse".into(), pin: "123456".into() }
    }

    #[test]
    fn the_welcome_is_left_only_once_everything_is_known() {
        let mut flow = Flow::new(device());
        assert!(flow.handle(Event::Storage { storage: storage(128, 0), operation: None }).unwrap().is_empty());
        let view = flow.view();
        assert_eq!((view["screen"].clone(), view["welcome"]["checked"].clone(), view["device"]["name"].clone()), ("welcome".into(), false.into(), serde_json::Value::Null));
        assert!(flow.handle(Event::Continue).is_err());
        flow.handle(Event::Target(target())).unwrap();
        assert_eq!(flow.view()["welcome"]["checked"], true);
        flow.handle(Event::Continue).unwrap();
        assert_eq!(flow.stage, Stage::Split { refused: None });
        let view = flow.view();
        assert_eq!((view["device"]["name"].clone(), view["supported"].clone()), ("iPhone XR".into(), true.into()));
    }

    #[test]
    fn an_unsupported_device_downloads_nothing() {
        let mut flow = Flow::new(device());
        flow.handle(Event::Storage { storage: storage(128, 0), operation: None }).unwrap();
        let effects = flow.handle(Event::Target(Target { supported: false, ..target() })).unwrap();
        assert!(effects.is_empty());
        assert_eq!(flow.view()["screen"], "unsupported");
    }

    #[test]
    fn the_divider_starts_at_half_or_at_the_existing_split() {
        let view = at_split(128, 0).view();
        assert_eq!(view["split"]["suggested_ios_bytes"], 64 * GB);
        assert_eq!(view["split"]["fits"], false);
        assert_eq!(view["split"]["min_ios_bytes"], align_up(45 * GB, 4096));
        let view = at_split(96, 32).view();
        assert_eq!(view["split"]["suggested_ios_bytes"], 96 * GB);
        assert_eq!(view["split"]["fits"], true);
        assert_eq!(view["storage"]["total_bytes"], 128 * GB);
    }

    #[test]
    fn a_full_device_has_no_room() {
        let mut flow = at_split(128, 0);
        flow.storage.as_mut().unwrap().ios_min_bytes = Some(124 * GB);
        let split = &flow.view()["split"];
        assert!(split["max_ios_bytes"].as_u64() < split["min_ios_bytes"].as_u64(), "not enough free space");
        assert!(flow.handle(Event::Split { ios_bytes: 110 * GB }).is_err());
    }

    #[test]
    fn a_split_pauses_the_downloads_and_ends_with_the_engines_word() {
        let mut flow = at_split(128, 0);
        let effects = flow.handle(Event::Split { ios_bytes: 96 * GB }).unwrap();
        assert_eq!(effects[0], Effect::Downloads(false));
        assert!(matches!(&effects[1], Effect::Split { ios_bytes, revision, .. } if *ios_bytes == 96 * GB && revision == "r1"));
        // An old operation's report before the engine named this one decides nothing.
        flow.handle(Event::Storage { storage: storage(128, 0), operation: Some(operation(OperationStatus::Completed)) }).unwrap();
        assert!(matches!(flow.stage, Stage::Splitting { .. }));
        flow.handle(Event::SplitStarted("op".into())).unwrap();
        flow.handle(Event::Storage { storage: storage(128, 0), operation: Some(operation(OperationStatus::Running)) }).unwrap();
        let view = flow.view();
        assert_eq!(view["splitting"]["steps"][1]["state"], "active");
        assert_eq!(view["splitting"]["steps"][1]["title"], "Shrink iOS");
        assert_eq!(view["splitting"]["reconnecting"], false);
        let effects = flow.handle(Event::Storage { storage: storage(96, 32), operation: Some(operation(OperationStatus::Completed)) }).unwrap();
        assert_eq!((flow.stage.clone(), effects), (Stage::SplitDone, vec![Effect::Downloads(true)]));
        flow.handle(Event::Continue).unwrap();
        assert_eq!(flow.stage, Stage::Account);
    }

    #[test]
    fn a_refused_or_stopped_split_returns_to_the_split_screen_with_the_reason() {
        let mut flow = at_split(128, 0);
        flow.handle(Event::Split { ios_bytes: 96 * GB }).unwrap();
        let effects = flow.handle(Event::SplitRefused("Storage changed.".into())).unwrap();
        assert_eq!(effects, [Effect::Downloads(true)]);
        assert_eq!(flow.view()["split"]["refused"], "Storage changed.");
        flow.handle(Event::Split { ios_bytes: 96 * GB }).unwrap();
        flow.handle(Event::SplitStarted("op".into())).unwrap();
        let stopped = Operation { message: Some("APFS refused.".into()), ..operation(OperationStatus::Stopped) };
        flow.handle(Event::Storage { storage: storage(128, 0), operation: Some(stopped) }).unwrap();
        assert_eq!(flow.stage, Stage::Split { refused: Some("APFS refused.".into()) });
    }

    #[test]
    fn a_split_found_running_is_followed() {
        let mut flow = Flow::new(device());
        let effects = flow.handle(Event::Storage { storage: storage(128, 0), operation: Some(operation(OperationStatus::Running)) }).unwrap();
        assert_eq!(effects, [Effect::Downloads(false)]);
        assert!(flow.handle(Event::Target(target())).unwrap().is_empty(), "downloads rest while the split runs");
        assert_eq!(flow.stage, Stage::Splitting { target_ios_bytes: 96 * GB, operation: Some("op".into()) });
    }

    #[test]
    fn a_quiet_split_shows_as_reconnecting() {
        let mut flow = at_split(128, 0);
        flow.handle(Event::Split { ios_bytes: 96 * GB }).unwrap();
        flow.handle(Event::SplitStarted("op".into())).unwrap();
        let quiet = Operation { confirmed_seconds_ago: Some(40.0), ..operation(OperationStatus::Running) };
        flow.handle(Event::Storage { storage: storage(128, 0), operation: Some(quiet) }).unwrap();
        assert_eq!(flow.view()["splitting"]["reconnecting"], true);
    }

    #[test]
    fn keeping_needs_room_and_install_needs_the_downloads() {
        let mut flow = at_split(128, 0);
        assert!(flow.handle(Event::Keep).is_err());
        let mut flow = at_split(96, 32);
        flow.handle(Event::Keep).unwrap();
        assert_eq!(flow.view()["account"]["download"], "downloading");
        assert!(flow.handle(Event::Install(account())).is_err());
        flow.handle(Event::Downloads(Downloads::Ready)).unwrap();
        assert!(flow.handle(Event::Install(Account { pin: "12345".into(), ..account() })).is_err());
        assert!(flow.handle(Event::Install(Account { password: "short".into(), ..account() })).is_err());
        assert_eq!(flow.handle(Event::Install(account())).unwrap(), [Effect::Install(account())]);
        flow.handle(Event::Progress(64)).unwrap();
        assert_eq!(flow.view()["installing"], serde_json::json!({"phase": "writing", "percent": 64}));
        flow.handle(Event::FinishingUp).unwrap();
        flow.handle(Event::Installed { seconds: 60.44 }).unwrap();
        assert_eq!(flow.view()["installed"]["seconds"], 60.4);
    }

    #[test]
    fn a_failed_install_can_be_tried_again() {
        let mut flow = at_split(96, 32);
        flow.handle(Event::Keep).unwrap();
        flow.handle(Event::Downloads(Downloads::Ready)).unwrap();
        flow.handle(Event::Install(account())).unwrap();
        flow.handle(Event::InstallFailed { message: "m".into(), detail: "d".into() }).unwrap();
        assert_eq!(flow.view()["failed"]["detail"], "d");
        assert_eq!(flow.handle(Event::Retry).unwrap(), [Effect::Downloads(true)]);
        assert_eq!(flow.stage, Stage::Account);
    }

    #[test]
    fn a_download_that_cannot_finish_is_said_on_the_account_screen() {
        let mut flow = at_split(96, 32);
        flow.handle(Event::DownloadFailed("SHA-256 is x".into())).unwrap();
        assert_eq!(flow.stage, Stage::Split { refused: None }, "the split goes on");
        flow.handle(Event::Keep).unwrap();
        assert_eq!(flow.view()["failed"]["detail"], "SHA-256 is x");
        flow.handle(Event::Retry).unwrap();
        assert_eq!((flow.stage.clone(), flow.broken.clone()), (Stage::Account, None));
    }

    #[test]
    fn partitions_after_ios_are_replaced_by_a_split_and_listed_before_an_install() {
        let mut flow = at_split(64, 64);
        flow.storage.as_mut().unwrap().outside = vec![Outside { id: "p2".into(), name: "BOOT".into(), filesystem: "FAT32".into(), bytes: 1 << 30 }];
        let effects = flow.handle(Event::Split { ios_bytes: 80 * GB }).unwrap();
        assert!(matches!(&effects[1], Effect::Split { replacing, .. } if replacing == &["p2".to_string()]));
        let mut flow = at_split(64, 64);
        flow.storage.as_mut().unwrap().outside = vec![Outside { id: "p2".into(), name: "BOOT".into(), filesystem: "FAT32".into(), bytes: 1 << 30 }];
        flow.handle(Event::Keep).unwrap();
        assert_eq!(flow.view()["account"]["erases"][0]["name"], "BOOT");
    }

    #[test]
    fn the_system_needs_boot_and_orchards_smallest_root() {
        assert_eq!(needed_bytes(3_339_198_464, 26_815_994), (9 << 30) + 2 * MIB);
        assert_eq!(needed_bytes(8 << 30, 0), (11 << 30) + 2 * MIB);
    }
}

//! Runs the flow: turns what the helper passes in (the app's requests, the
//! resize engine's storage reports) and what its own threads find into flow
//! events, and carries out the flow's effects. Downloads, the firmware build
//! and the install each run on a thread of their own; resizing iOS is the
//! helper's (resize-apfs's engine), asked for through `foreign`.
use crate::disk::Disk;
use crate::download::{self, hex};
use crate::finish::Kernel;
use crate::flow::{self, Account, Device, Downloads, Effect, Event, Flow, Operation, OperationStatus, Outside, SplitStep, Storage, Target};
use crate::image::Map;
use crate::install::{self, Inputs, Step};
use orchard_boards::catalog::Catalog;
use orchard_boards::firmware_build::{self, Request};
use orchard_boards::kernel_package;
use orchard_boards::release::{self, Platform, Receipt, Release};
use orchard_cache::{Code, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Seconds between 1970 and 2001, where the resize engine's dates count from.
const REFERENCE_DATE: f64 = 978_307_200.0;
const RETRY: Duration = Duration::from_secs(3);

/// What the helper starts the engine with.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// The cache, in the tethered installer's layout.
    pub cache: PathBuf,
    /// The disk (`/dev/disk0`), or a file standing in for it.
    pub disk: PathBuf,
    /// Where e2fsprogs' programs are.
    pub tools: PathBuf,
    /// The release index to install from.
    pub index: String,
    /// The system's name, shown until the release has been read.
    #[serde(default)]
    pub system: Option<String>,
    /// Apple's product type, such as `iPhone11,8`.
    pub machine: String,
    /// `iPhone`, `iPad` or `iPod touch`, and `iOS` or `iPadOS`.
    pub kind: String,
    pub os: String,
    #[serde(default)]
    pub timezone: Option<String>,
    /// Install on a board the release does not cover, with the kernel of its
    /// chip's page size. For bring-up: such a system has no device tree.
    #[serde(default)]
    pub allow_unsupported: bool,
}

/// What a partition holds and the name its filesystem has.
type Held = (Option<&'static str>, Option<String>);

/// The release and the board, read once.
struct Prepared {
    release: Release,
    platform: Option<Platform>,
    board: Option<String>,
    devicetree: Option<String>,
    data: PathBuf,
    has_firmware: bool,
}

/// Everything the install is made from, in the cache and checked.
struct Assets {
    image: PathBuf,
    map: Map,
    layer: Vec<u8>,
    package: Option<Vec<u8>>,
    firmware: Vec<(String, Vec<u8>)>,
}

pub struct Engine {
    config: Config,
    flow: Mutex<Flow>,
    foreign: Box<dyn Fn(&str) + Send + Sync>,
    prepared: Mutex<Option<Arc<Prepared>>>,
    assets: Mutex<Option<Arc<Assets>>>,
    downloading: AtomicBool,
    /// Raised by Try again: a download that failed for good starts over.
    attempt: AtomicU64,
    /// The filesystems of the partitions after iOS, for the layout last probed.
    probed: Mutex<(String, Vec<Held>)>,
    log: Mutex<Option<std::fs::File>>,
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

impl Engine {
    /// Starts the engine and its downloads. `foreign` is called with the
    /// effects the helper carries out, as JSON (`{"split": …}`, `{"refresh": {}}`).
    pub fn start(config: Config, foreign: Box<dyn Fn(&str) + Send + Sync>) -> Arc<Engine> {
        let _ = std::fs::create_dir_all(&config.cache);
        let log = std::fs::OpenOptions::new().create(true).append(true).open(config.cache.join("installer.log")).ok();
        let device = Device { name: config.kind.clone(), kind: config.kind.clone(), os: config.os.clone() };
        let engine = Arc::new(Engine {
            flow: Mutex::new(Flow { system: config.system.clone(), ..Flow::new(device) }),
            config,
            foreign,
            prepared: Mutex::new(None),
            assets: Mutex::new(None),
            downloading: AtomicBool::new(false),
            attempt: AtomicU64::new(0),
            probed: Mutex::new((String::new(), Vec::new())),
            log: Mutex::new(log),
        });
        engine.say(&format!("started for {} ({})", engine.config.machine, engine.config.index));
        let worker = engine.clone();
        std::thread::spawn(move || worker.work());
        engine
    }

    /// A line in the helper's log. Never the account's secrets.
    pub fn say(&self, line: &str) {
        let t = now();
        let (s, ms) = (t as u64 % 86_400, (t.fract() * 1000.0) as u64);
        let text = format!("{:02}:{:02}:{:02}.{ms:03} {line}\n", s / 3600, s / 60 % 60, s % 60);
        if let Some(file) = self.log.lock().expect("log lock").as_mut() {
            let _ = file.write_all(text.as_bytes());
        }
        eprint!("{text}");
    }

    /// The end of the log, for the app's Share log.
    pub fn log_tail(&self) -> String {
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(self.config.cache.join("installer.log")) {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = file.seek(SeekFrom::Start(len.saturating_sub(64 << 10)));
            let mut raw = Vec::new();
            let _ = file.read_to_end(&mut raw);
            text = String::from_utf8_lossy(&raw).into_owned();
        }
        text
    }

    fn dispatch(self: &Arc<Self>, event: Event) -> std::result::Result<(), flow::Refused> {
        let effects = {
            let mut flow = self.flow.lock().expect("flow lock");
            let before = flow.stage.clone();
            let effects = flow.handle(event)?;
            if std::mem::discriminant(&flow.stage) != std::mem::discriminant(&before) {
                // Stages carry no secrets; the account travels in the effect.
                self.say(&format!("stage: {:?}", flow.stage));
            }
            effects
        };
        for effect in effects {
            match effect {
                Effect::Split { ios_bytes, revision, replacing } => {
                    (self.foreign)(&json!({"split": {"ios_bytes": ios_bytes, "revision": revision, "replacing": replacing}}).to_string())
                }
                Effect::Refresh => (self.foreign)(&json!({"refresh": {}}).to_string()),
                Effect::Downloads(run) => {
                    self.downloading.store(run, Ordering::SeqCst);
                    self.attempt.fetch_add(u64::from(run), Ordering::SeqCst);
                }
                Effect::Install(account) => {
                    let engine = self.clone();
                    std::thread::spawn(move || engine.install(&account));
                }
            }
        }
        Ok(())
    }

    pub fn view(&self) -> Value {
        self.flow.lock().expect("flow lock").view()
    }

    /// One request of the app (PROTOCOL.md), answered with the view.
    pub fn request(self: &Arc<Self>, request: &Value) -> Value {
        let refused = |message: &str| json!({"error": {"message": message}});
        let Some(object) = request.as_object() else { return refused("A request is an object.") };
        let named: Vec<(&String, &Value)> = object.iter().filter(|(key, _)| *key != "token").collect();
        let [(name, args)] = named[..] else { return refused("A request names exactly one thing to do.") };
        let event = match name.as_str() {
            "view" => return json!({"ok": self.view()}),
            "log" => return json!({"ok": {"text": self.log_tail()}}),
            "split" => match args["ios_bytes"].as_u64() {
                Some(ios_bytes) => Event::Split { ios_bytes },
                None => return refused("A split needs ios_bytes."),
            },
            "keep" => Event::Keep,
            "check" => Event::Check,
            "continue" => Event::Continue,
            "retry" => Event::Retry,
            "install" => match (args["password"].as_str(), args["pin"].as_str()) {
                (Some(password), Some(pin)) => Event::Install(Account { password: password.into(), pin: pin.into() }),
                _ => return refused("An install needs the password and the PIN."),
            },
            _ => return refused("Unknown request."),
        };
        match self.dispatch(event) {
            Ok(()) => json!({"ok": self.view()}),
            Err(flow::Refused(why)) => refused(&why),
        }
    }

    /// The resize engine's snapshot (its `ServiceSnapshot`, as it encodes it).
    pub fn storage(self: &Arc<Self>, snapshot: &Value) {
        match self.read_snapshot(snapshot) {
            Ok((storage, operation)) => {
                let _ = self.dispatch(Event::Storage { storage, operation });
            }
            Err(e) => self.say(&format!("unreadable storage report: {e}")),
        }
    }

    pub fn split_started(self: &Arc<Self>, operation: &str) {
        let _ = self.dispatch(Event::SplitStarted(operation.into()));
    }

    pub fn split_refused(self: &Arc<Self>, message: &str) {
        self.say(&format!("split refused: {message}"));
        let _ = self.dispatch(Event::SplitRefused(message.into()));
    }

    fn read_snapshot(&self, snapshot: &Value) -> Result<(Storage, Option<Operation>)> {
        let bad = |what: &str| Error::new(Code::Format, format!("the storage report has no {what}"));
        let layout = &snapshot["layout"];
        let int = |value: &Value, key: &str| value[key].as_u64().ok_or_else(|| bad(key));
        let sector = int(layout, "sectorSize")?;
        let backing = layout["backingPartitionID"].as_str().ok_or_else(|| bad("backingPartitionID"))?;
        let partitions = layout["partitions"].as_array().ok_or_else(|| bad("partitions"))?;
        let ios = partitions.iter().find(|p| p["id"].as_str() == Some(backing)).ok_or_else(|| bad("APFS partition"))?;
        let (ios_first, ios_last) = (int(ios, "firstLBA")?, int(ios, "lastLBA")?);
        let revision = layout["revision"].as_str().ok_or_else(|| bad("revision"))?.to_owned();
        let mut issue = layout["issue"].as_str().map(str::to_owned);
        let following: Vec<&Value> = partitions.iter().filter(|p| p["firstLBA"].as_u64().is_some_and(|first| first > ios_last)).collect();
        // What the partitions hold, read once per layout.
        let mut probed = self.probed.lock().expect("probe lock");
        if probed.0 != revision {
            let disk = Disk::open(&self.config.disk).ok();
            probed.1 = following.iter().map(|p| disk.as_ref().map_or((None, None), |d| d.describe(node_number(p), p["firstLBA"].as_u64().unwrap_or(0)))).collect();
            probed.0 = revision.clone();
        }
        let mut outside = Vec::new();
        for (p, (filesystem, label)) in following.iter().zip(probed.1.iter()) {
            let kind = p["typeGUID"].as_str().unwrap_or("");
            if !install::ours(kind) {
                issue.get_or_insert_with(|| "A partition after iOS is not a Linux partition.".into());
            }
            let name = label.clone().filter(|l| !l.is_empty()).or_else(|| p["name"].as_str().filter(|n| !n.is_empty()).map(str::to_owned));
            outside.push(Outside {
                id: p["id"].as_str().unwrap_or("").to_owned(),
                name: name.unwrap_or_else(|| "Linux".into()),
                filesystem: filesystem.unwrap_or("unknown").into(),
                bytes: (int(p, "lastLBA")? - int(p, "firstLBA")? + 1) * sector,
            });
        }
        let storage = Storage {
            revision,
            sector,
            ios_bytes: (ios_last - ios_first + 1) * sector,
            ios_used_bytes: int(layout, "allocatedBytes")?,
            ios_min_bytes: layout["minimumContainerBytes"].as_u64(),
            outside_bytes: int(layout, "lastUsableLBA")?.saturating_sub(ios_last) * sector,
            outside,
            issue,
        };
        let op = &snapshot["operation"];
        let operation = match op["id"].as_str() {
            None => None,
            Some(id) => {
                let steps = op["plan"]["steps"].as_array().cloned().unwrap_or_default();
                let at = steps.get(op["stepIndex"].as_u64().unwrap_or(0) as usize).and_then(Value::as_str).unwrap_or("verifyLayout");
                let unix = |key: &str| op[key].as_f64().map(|t| t + REFERENCE_DATE);
                let status = match op["status"].as_str() {
                    Some("running") => OperationStatus::Running,
                    Some("completed") => OperationStatus::Completed,
                    _ => OperationStatus::Stopped,
                };
                let message = op["message"].as_str().map(str::to_owned).or_else(|| {
                    (op["status"].as_str() == Some("unknown")).then(|| "The split was interrupted and its outcome is not known.".to_owned())
                });
                Some(Operation {
                    id: id.to_owned(),
                    status,
                    message,
                    target_ios_bytes: op["plan"]["targetBytes"].as_u64().unwrap_or(0),
                    step: match at {
                        "checking" | "preparing" => SplitStep::Check,
                        "verifyLayout" => SplitStep::Verify,
                        _ => SplitStep::Resize,
                    },
                    elapsed_seconds: unix("startedAt").map_or(0.0, |t| (now() - t).max(0.0)),
                    confirmed_seconds_ago: unix("lastConfirmedAt").map(|t| (now() - t).max(0.0)),
                })
            }
        };
        Ok((storage, operation))
    }

    /// The worker: the release and the board first, then every download,
    /// waiting while the flow says downloads rest.
    fn work(self: Arc<Self>) {
        let prepared = loop {
            match self.prepare() {
                Ok(prepared) => break Arc::new(prepared),
                Err(e) => {
                    self.say(&format!("reading the release: {e}"));
                    std::thread::sleep(RETRY);
                }
            }
        };
        *self.prepared.lock().expect("prepared lock") = Some(prepared.clone());
        let release = &prepared.release;
        let sizes = (release.root.expanded_bytes.unwrap_or(0), prepared.platform.as_ref().and_then(|p| p.layer.expanded_bytes).unwrap_or(0));
        let _ = self.dispatch(Event::Target(Target {
            system: release.name.clone(),
            device_name: self.device_name(&prepared),
            supported: prepared.platform.is_some() && prepared.devicetree.is_some(),
            user: release.account_user.clone().unwrap_or_default(),
            needed_bytes: flow::needed_bytes(sizes.0, sizes.1),
            download_bytes: release.root.bytes - download::have(&self.config.cache, &release.root),
        }));
        if prepared.platform.is_none() || prepared.devicetree.is_none() {
            return;
        }
        let mut failed_at = None;
        loop {
            if !self.downloading.load(Ordering::SeqCst) || failed_at == Some(self.attempt.load(Ordering::SeqCst)) {
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            match self.fetch_all(&prepared) {
                Ok(Some(assets)) => {
                    *self.assets.lock().expect("assets lock") = Some(Arc::new(assets));
                    self.say("everything is downloaded");
                    let _ = self.dispatch(Event::Downloads(Downloads::Ready));
                    return;
                }
                // Paused for the split.
                Ok(None) => {}
                Err(e) if e.code == Code::Network => {
                    self.say(&format!("download: {e}"));
                    let _ = self.dispatch(Event::Downloads(Downloads::Offline));
                    std::thread::sleep(RETRY);
                }
                Err(e) => {
                    self.say(&format!("download failed: {e}"));
                    failed_at = Some(self.attempt.load(Ordering::SeqCst));
                    let _ = self.dispatch(Event::DownloadFailed(e.to_string()));
                }
            }
        }
    }

    fn device_name(&self, prepared: &Prepared) -> String {
        let named = prepared.board.as_ref().and_then(|board| Catalog::open(&prepared.data).ok()?.board(board).ok()).map(|b| b.display_name);
        named.filter(|n| !n.is_empty()).unwrap_or_else(|| self.config.kind.clone())
    }

    fn prepare(&self) -> Result<Prepared> {
        let text = |bytes: Vec<u8>, what: &str| String::from_utf8(bytes).map_err(|_| Error::new(Code::Format, format!("{what} is not UTF-8")));
        let index = text(download::small(&self.config.index)?, "the index")?;
        let release = if release::is_release(&index) {
            release::parse_release(&index, release::base_of(&self.config.index))?
        } else {
            let manifest = release::parse_index(&index, release::base_of(&self.config.index))?;
            let bytes = download::small(&manifest.url)?;
            if bytes.len() as u64 != manifest.bytes || hex(&sha2::Sha256::digest(&bytes)) != manifest.sha256 {
                return Err(Error::new(Code::Format, "the release manifest differs from what the index pins"));
            }
            release::parse_release(&text(bytes, "the release")?, release::base_of(&manifest.url))?
        };
        self.say(&format!("release {} {}", release.name, release.version));
        let receipt = release.board_data.clone().ok_or_else(|| Error::new(Code::Unsupported, "this release ships no board data"))?;
        let data = self.config.cache.join("board-data").join(&receipt.sha256);
        if !data.join(".complete").is_file() {
            let archive = std::fs::read(download::fetch(&self.config.cache, &receipt, &|| true, &|_| {})?.expect("never paused"))?;
            let partial = data.with_extension("partial");
            let _ = std::fs::remove_dir_all(&partial);
            unpack_board_data(&archive, &partial)?;
            std::fs::write(partial.join(".complete"), &receipt.sha256)?;
            let _ = std::fs::remove_dir_all(&data);
            std::fs::rename(&partial, &data)?;
        }
        let catalog = Catalog::open(&data)?;
        let Ok(board) = catalog.board_for_product(&self.config.machine) else {
            self.say(&format!("{} is not in the board data", self.config.machine));
            return Ok(Prepared { release, platform: None, board: None, devicetree: None, data, has_firmware: false });
        };
        let listed = ["supported", "experimental"].contains(&board.status.as_str());
        let mut platform = release.platform_for(&board.name).filter(|_| listed).cloned();
        let mut devicetree = board.dtb.clone().filter(|_| platform.is_some());
        if platform.is_none() && self.config.allow_unsupported {
            // No bring-up yet: the kernel of the chip's page size, and the
            // device tree name a later kernel would bring.
            if let Ok(chip) = catalog.chip(board.chip) {
                let name = format!("apple-{}k", chip.page_size / 1024);
                platform = release.platforms.iter().find(|p| p.name == name).cloned();
                devicetree = Some(format!("{}-{}", chip.soc, board.name));
                self.say(&format!("{} has no bring-up; installing the {name} platform for it as asked", board.name));
            }
        }
        self.say(&format!("board {} ({}), status {:?}, platform {:?}", board.name, board.display_name, board.status, platform.as_ref().map(|p| &p.name)));
        let has_firmware = board.recipe(orchard_boards::catalog::Kind::Peripheral).is_ok();
        Ok(Prepared { release, platform, board: Some(board.name), devicetree, data, has_firmware })
    }

    /// Every file of the install, smallest first; `None` when paused.
    fn fetch_all(self: &Arc<Self>, prepared: &Prepared) -> Result<Option<Assets>> {
        let cache = &self.config.cache;
        let release = &prepared.release;
        let go = || self.downloading.load(Ordering::SeqCst);
        let platform = prepared.platform.as_ref().expect("checked by the worker");
        let board = prepared.board.as_deref().expect("checked by the worker");
        let _ = self.dispatch(Event::Downloads(Downloads::Downloading));
        let Some(map) = download::fetch(cache, &release.root_map, &go, &|_| {})? else { return Ok(None) };
        let map = Map::parse(&std::fs::read(map)?)?;
        let Some(layer) = download::fetch(cache, &platform.layer, &go, &|_| {})? else { return Ok(None) };
        let layer = std::fs::read(layer)?;
        let package = match kernel_package::resolve(&release.base, &layer, &platform.layer.sha256)? {
            Some(package) => {
                self.say(&format!("kernel package {}", package.file));
                let receipt = Receipt { url: package.url.clone(), bytes: package.bytes, sha256: package.sha256.clone(), expanded_bytes: None };
                let Some(path) = download::fetch(cache, &receipt, &go, &|_| {})? else { return Ok(None) };
                Some(std::fs::read(path)?)
            }
            None => {
                self.say("no kernel package wraps this platform layer; its files go in as they are");
                None
            }
        };
        let firmware = if prepared.has_firmware {
            if !go() {
                return Ok(None);
            }
            let out = cache.join("firmware").join(board);
            if !out.join(".complete").is_file() {
                self.say("building the board's firmware from Apple's");
                let _ = std::fs::remove_dir_all(&out);
                firmware_build::build(&prepared.data, Some(&cache.join("store")), board, &out, &Request::Peripheral { skip_unsupported: false })?;
                std::fs::write(out.join(".complete"), b"")?;
            }
            firmware_files(&out)?
        } else {
            Vec::new()
        };
        let remaining = AtomicU64::new(release.root.bytes - download::have(cache, &release.root));
        let last = Mutex::new(Instant::now());
        let got = |n: u64| {
            let left = remaining.fetch_sub(n, Ordering::SeqCst) - n;
            let mut last = last.lock().expect("progress lock");
            if last.elapsed() >= Duration::from_secs(1) || left == 0 {
                *last = Instant::now();
                let _ = self.dispatch(Event::Remaining(left));
            }
        };
        let Some(image) = download::fetch(cache, &release.root, &go, &got)? else { return Ok(None) };
        let _ = self.dispatch(Event::Remaining(0));
        Ok(Some(Assets { image, map, layer, package, firmware }))
    }

    fn install(self: Arc<Self>, account: &Account) {
        let started = Instant::now();
        let phase = Mutex::new("writing it");
        let result = (|| -> Result<install::Record> {
            let prepared = self.prepared.lock().expect("prepared lock").clone().ok_or_else(|| Error::new(Code::NotFound, "the release is not read yet"))?;
            let assets = self.assets.lock().expect("assets lock").clone().ok_or_else(|| Error::new(Code::NotFound, "the downloads are not complete"))?;
            let platform = prepared.platform.as_ref().ok_or_else(|| Error::new(Code::Unsupported, "no platform for this board"))?;
            let devicetree = prepared.devicetree.as_deref().ok_or_else(|| Error::new(Code::Unsupported, "no device tree for this board"))?;
            let disk = Disk::open(&self.config.disk)?;
            let work = install::work_dir(&self.config.cache);
            let _ = std::fs::remove_dir_all(&work);
            std::fs::create_dir_all(&work)?;
            let inputs = Inputs {
                name: &prepared.release.name,
                cmdline: prepared.release.cmdline.as_deref(),
                account: prepared.release.account.as_ref(),
                secrets: account,
                image: &assets.image,
                image_receipt: &prepared.release.root,
                map: &assets.map,
                layer_expanded: platform.layer.expanded_bytes.unwrap_or(0),
                kernel: match &assets.package {
                    Some(package) => Kernel::Package(package),
                    None => Kernel::Layer(&assets.layer),
                },
                devicetree,
                firmware: &assets.firmware,
                timezone: self.config.timezone.as_deref(),
                tools: &self.config.tools,
                work: &work,
                now: now() as u64,
            };
            let shown = AtomicU64::new(u64::MAX);
            let record = install::run(&disk, &inputs, &|step| match step {
                Step::Writing(percent) => {
                    if shown.swap(percent as u64, Ordering::SeqCst) != percent as u64 {
                        let _ = self.dispatch(Event::Progress(percent));
                    }
                }
                Step::Finishing(name) => {
                    self.say(&format!("install: {name} ({:.1} s)", started.elapsed().as_secs_f64()));
                    *phase.lock().expect("phase lock") = "finishing it";
                    let _ = self.dispatch(Event::FinishingUp);
                }
            })?;
            Ok(record)
        })();
        // Its presence says an install runs (the helper's --busy).
        let _ = std::fs::remove_dir_all(install::work_dir(&self.config.cache));
        match result {
            Ok(record) => {
                let seconds = started.elapsed().as_secs_f64();
                self.say(&format!("installed in {seconds:.1} s: root {}, boot {}, entry {:?}", record.root_partuuid, record.boot_partuuid, record.entry));
                // The image is on its partition; iOS gets the space back.
                if let Some(assets) = self.assets.lock().expect("assets lock").take() {
                    let _ = std::fs::remove_file(&assets.image);
                }
                let _ = self.dispatch(Event::Installed { seconds });
            }
            Err(e) => {
                self.say(&format!("install failed: {e}"));
                let message = format!("Something went wrong while {}. {} and your data weren't touched.", phase.lock().expect("phase lock"), self.config.os);
                let _ = self.dispatch(Event::InstallFailed { message, detail: e.to_string() });
            }
        }
    }
}

/// A partition's number, from its node's name (`disk0s2`).
fn node_number(partition: &Value) -> u32 {
    partition["bsdName"].as_str().and_then(|name| name.rsplit('s').next()?.parse().ok()).unwrap_or(0)
}

/// The built firmware outputs below `dir`, as (path below the root, bytes).
fn firmware_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.join("lib/firmware")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dir) {
                out.push((rel.to_string_lossy().into_owned(), std::fs::read(&path)?));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Unpacks a board data archive (tar.gz) into `dir`: only board data, only
/// plain relative paths (the tethered installer's rule).
fn unpack_board_data(archive: &[u8], dir: &Path) -> Result<()> {
    const KEPT: &[&str] = &["devices/", "chips/", "platforms/", "features.toml"];
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive).take(256 << 20).read_to_end(&mut tar)?;
    let mut kept = 0;
    for file in orchard_formats::tar::read(&tar)?.files {
        let path = file.path.trim_start_matches("./");
        let plain = !path.is_empty() && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..");
        if !plain || !KEPT.iter().any(|k| path == k.trim_end_matches('/') || path.starts_with(k)) {
            continue;
        }
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, file.data)?;
        kept += 1;
    }
    if kept == 0 {
        return Err(Error::new(Code::Format, "the board data archive holds no board data"));
    }
    Ok(())
}

use sha2::Digest as _;

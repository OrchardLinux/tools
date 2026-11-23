//! Carries out the install over the install system's raw USB receiver
//! (`docs/install-protocol.md`): reads the layout, plans, stages the layer and
//! firmware while the user fills in the account, then stages the secrets and
//! streams the root image. Runs on its own thread; everything it learns goes
//! back to the flow as events.
use crate::exec::Executor;
use crate::release::Release;
use orchard_boards::install_env;
use orchard_boards::install_plan::{self, Layout as Gpt, Sizes, XBOOTLDR};
use orchard_cache::{Code, Error, Result};
use orchard_flow::{Account, Device, Event, Layout, Partition};
use orchard_usb::install::{self as proto, Failure, Phase, Session};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};

/// A wall-clock stamp for timing lines (HH:MM:SS.mmm, UTC), so the
/// installer's and the app's lines line up.
pub fn stamp() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let s = d.as_secs() % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, s / 60 % 60, s % 60, d.subsec_millis())
}

/// One timing line: what, how much, how long, how fast.
fn timed(what: &str, bytes: u64, started: std::time::Instant) {
    let secs = started.elapsed().as_secs_f64();
    let mb = bytes as f64 / 1e6;
    eprintln!("{} timing: {what}: {mb:.1} MB in {secs:.1} s ({:.1} MB/s)", stamp(), mb / secs.max(0.001));
}

/// Where staged files land on the device.
const STAGING: &str = "/run/orchard";
const LAYER: &str = "platform.tar.gz";
const FIRMWARE: &str = "firmware.tar.gz";
const SECRETS: &str = "secrets.json";

pub enum Job {
    /// The install system of `device` showed up: open it and report.
    Open { usb: Box<orchard_usb::Device>, device: Device },
    /// Plan and stage everything but the account.
    Prepare,
    /// Stage the secrets and stream the image.
    Start(Account),
    /// Stream again from the start after a cut-off.
    Restart,
}

/// Starts the worker; jobs go in, flow events come out.
pub fn spawn(executor: Executor, events: Sender<Event>) -> Sender<Job> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || Worker { executor, events, usb: None, session: None, device: None, gpt: None, protocol_retried: false, silence_retried: false, current: None }.run(rx));
    tx
}

struct Worker {
    executor: Executor,
    /// The install system's USB device, to start a fresh session on.
    usb: Option<orchard_usb::Device>,
    events: Sender<Event>,
    session: Option<Session>,
    device: Option<Device>,
    gpt: Option<Gpt>,
    /// A fresh session was already tried after a protocol error.
    protocol_retried: bool,
    /// Likewise after the install system went silent.
    silence_retried: bool,
    /// The plan or start job running, to run again on a fresh session that
    /// finds nothing written yet (the old one broke before the plan landed).
    current: Option<Interrupted>,
}

#[derive(Clone)]
enum Interrupted {
    Prepare,
    Start(Account),
}

/// The flow's view of the disk: iPadOS is what the installer never touches;
/// everything else, free space included, becomes Omarchy's.
pub fn flow_layout(gpt: &Gpt) -> Layout {
    let gb = |blocks: u64| ((blocks * gpt.disk.sector) as f64 / 1e9).round() as u32;
    let total = if gpt.disk.bytes > 0 { (gpt.disk.bytes as f64 / 1e9).round() as u32 } else {
        gb(gpt.partitions.iter().map(|p| p.blocks).sum::<u64>() + gpt.free.iter().map(|f| f.blocks).sum::<u64>())
    };
    let ours = |p: &install_plan::Partition| {
        let kind = p.kind.to_ascii_lowercase();
        kind == install_plan::ROOT_ARM64 || kind == install_plan::LINUX_DATA || kind == XBOOTLDR
    };
    let ipados: u64 = gpt.partitions.iter().filter(|p| !ours(p)).map(|p| p.blocks).sum();
    let outside = gpt
        .partitions
        .iter()
        .filter(|p| ours(p))
        .map(|p| Partition {
            name: p
                .label
                .clone()
                .filter(|l| !l.is_empty())
                .or_else(|| Some(p.name.clone()).filter(|n| !n.is_empty()))
                .unwrap_or_else(|| format!("partition {}", p.number)),
            filesystem: p.fs.clone().unwrap_or_else(|| "unknown".into()),
            gb: gb(p.blocks),
        })
        .collect();
    Layout { total_gb: total, ipados_gb: gb(ipados), ipados_used_gb: 0, outside }
}

impl Worker {
    fn run(mut self, jobs: Receiver<Job>) {
        for job in jobs {
            let result = match job {
                Job::Open { usb, device } => self.open(&usb, device),
                Job::Prepare => {
                    self.current = Some(Interrupted::Prepare);
                    self.prepare()
                }
                Job::Start(account) => {
                    self.current = Some(Interrupted::Start(account.clone()));
                    self.start(&account)
                }
                Job::Restart => self.stream(),
            };
            if let Err(e) = result {
                self.report_failure(e);
            }
            self.current = None;
        }
    }

    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn report_failure(&mut self, e: Fail) {
        match e {
            // Silent while still on USB: one fresh session, then it is reported
            // (a cable pulled shows up on USB by itself; the device keeps going).
            Fail::Proto(Failure::Usb(e)) if e.kind == orchard_usb::Kind::Timeout => {
                self.session = None;
                if self.silence_retried {
                    self.send(Event::InstallRefused("the install system stopped answering".into()));
                } else {
                    self.silence_retried = true;
                    self.reopen();
                }
            }
            // Disconnected. Unplugged: USB reports it and the flow follows.
            // Still attached (the session broke, the device didn't): one
            // fresh session, then it is reported, never left hanging silently.
            Fail::Proto(f) if f.lost() => {
                eprintln!("install: the session was lost: {f}");
                self.session = None;
                let attached = self.usb.as_ref().is_some_and(|usb| {
                    orchard_usb::list().is_ok_and(|all| all.iter().any(|d| d.location == usb.location && d.system == usb.system))
                });
                if !attached {
                    return;
                }
                if self.silence_retried {
                    self.send(Event::InstallRefused(format!("the connection to the install system broke: {f}")));
                } else {
                    self.silence_retried = true;
                    self.reopen();
                }
            }
            // The framing is lost: only a fresh session (SET_INTERFACE) helps.
            // Once: a second one in a row is reported, not retried for ever.
            Fail::Proto(Failure::Refused(r)) if r.code == "protocol" && self.protocol_retried => {
                eprintln!("install: protocol error again: {}", r.message);
                self.send(Event::InstallRefused(format!("the install system refused the installer's messages: {}", r.message)));
            }
            Fail::Proto(Failure::Refused(r)) if r.code == "protocol" => {
                eprintln!("install: protocol error: {}", r.message);
                self.protocol_retried = true;
                self.session = None;
                if let (Some(usb), Some(device)) = (self.usb.clone(), self.device.clone()) {
                    if let Err(e) = self.open(&usb, device) {
                        self.report_failure(e);
                    }
                }
            }
            Fail::Proto(Failure::Refused(r)) if r.code == "layout-changed" => {
                // Inspect again and let the flow decide anew.
                if let Err(e) = self.inspect() {
                    self.send(Event::ChainFailed(e.to_string()));
                }
            }
            e => {
                eprintln!("install: {e}");
                self.send(Event::InstallRefused(e.to_string()));
            }
        }
    }

    /// A fresh session on the same install system. When it finds nothing
    /// written yet, the interrupted plan or start job runs again on it (later
    /// phases are picked up by the events open sends).
    fn reopen(&mut self) {
        let (Some(usb), Some(device)) = (self.usb.clone(), self.device.clone()) else { return };
        let result = match self.current.take() {
            None => self.open(&usb, device).map(drop),
            // The job carries on by itself on the fresh session; no phase
            // events, so nothing else starts streaming alongside it.
            Some(job) => self.connect(&usb, device).and_then(|info| match (info.phase, job) {
                (Phase::Ready, Interrupted::Prepare) => self.inspect_quietly().and_then(|_| self.prepare()),
                (Phase::Ready, Interrupted::Start(account)) => {
                    self.inspect_quietly()?;
                    self.prepare()?;
                    self.start(&account)
                }
                (Phase::Planned | Phase::StreamIncomplete, Interrupted::Prepare) => self.stage(),
                (Phase::Planned | Phase::StreamIncomplete, Interrupted::Start(account)) => self.start(&account),
                _ => {
                    self.report(info);
                    Ok(())
                }
            }),
        };
        if let Err(e) = result {
            self.report_failure(e);
        }
    }

    fn session(&mut self) -> std::result::Result<&mut Session, Fail> {
        self.session.as_mut().ok_or(Fail::Other(Error::new(Code::NotFound, "the install system is not connected")))
    }

    /// Opens the session and reports by the device's phase.
    fn open(&mut self, usb: &orchard_usb::Device, device: Device) -> std::result::Result<(), Fail> {
        let info = self.connect(usb, device)?;
        if info.phase == Phase::Ready {
            return self.inspect();
        }
        self.report(info);
        Ok(())
    }

    /// The disk again, for a job that carries on (the flow already has it).
    fn inspect_quietly(&mut self) -> std::result::Result<(), Fail> {
        let value = self.session()?.inspect()?;
        self.gpt = Some(Gpt::from_inspect(&value)?);
        Ok(())
    }

    /// A session on the install system that `usb` is, checked to be `device`'s.
    fn connect(&mut self, usb: &orchard_usb::Device, device: Device) -> std::result::Result<orchard_usb::install::Info, Fail> {
        let mut session = Session::open(usb)?;
        let events = self.events.clone();
        session.on_event(move |e| {
            let event = match e {
                proto::Event::Stream { written, bytes } if *bytes > 0 => {
                    Event::InstallProgress(((*written as f64 / *bytes as f64) * 100.0) as u8)
                }
                proto::Event::Step(step) => {
                    eprintln!("{} timing: finishing step {step}", stamp());
                    Event::InstallStep(step.clone())
                }
                proto::Event::Done(_) => {
                    eprintln!("{} timing: done", stamp());
                    Event::InstallFinished { ok: true }
                }
                proto::Event::Failed(f) => Event::InstallRefused(f.message.clone()),
                _ => return,
            };
            let _ = events.send(event);
        });
        let info = session.info()?;
        if info.ecid.as_deref().map(crate::ecid_key).as_deref() != Some(device.id.0.as_str()) || info.board != device.board {
            return Err(Fail::Other(Error::new(Code::Format, "the install system answers for another device")));
        }
        self.usb = Some(usb.clone());
        self.session = Some(session);
        self.device = Some(device);
        Ok(info)
    }

    /// Tells the flow where the install stands, by the device's phase (past
    /// ready).
    fn report(&mut self, info: orchard_usb::install::Info) {
        match info.phase {
            Phase::Ready => {}
            // The plan is written: with the account staged, the image is
            // streamed (again) from the start; without it, the flow asks for
            // it (an installer that stopped in between) or is still asking.
            Phase::Planned | Phase::StreamIncomplete => {
                if info.staged.iter().any(|f| f == SECRETS) {
                    self.send(Event::Reconnected { stream_incomplete: true });
                } else {
                    self.send(Event::AccountNeeded);
                }
            }
            Phase::Finishing => {
                self.send(Event::Reconnected { stream_incomplete: false });
                self.send(Event::Streamed);
            }
            Phase::Done => {
                self.send(Event::InstallFinished { ok: true });
            }
            Phase::Failed => {
                let why = info.error.map(|e| e.message).unwrap_or_else(|| "The install system stopped.".into());
                self.send(Event::InstallRefused(why));
            }
        }
    }

    fn inspect(&mut self) -> std::result::Result<(), Fail> {
        let value = self.session()?.inspect()?;
        let gpt = Gpt::from_inspect(&value)?;
        let layout = flow_layout(&gpt);
        self.gpt = Some(gpt);
        self.send(Event::EnvReady { layout });
        Ok(())
    }

    fn prepare(&mut self) -> std::result::Result<(), Fail> {
        let release = self.executor.release()?;
        let device = self.device.clone().ok_or(Fail::Other(Error::new(Code::NotFound, "no device")))?;
        let gpt = self.gpt.clone().ok_or(Fail::Other(Error::new(Code::NotFound, "no layout yet")))?;
        let platform = release
            .platform_for(&device.board)
            .ok_or_else(|| Error::new(Code::NotFound, format!("{} does not cover this board yet", release.name)))?
            .clone();
        // The layer is fetched now (stage puts it), so a missing download fails early.
        self.executor.download(&platform.layer)?;
        let firmware = firmware_files(&self.executor.firmware_dir(&device.board))?;
        let mixin = install_env::firmware_mixin(&device.board, &firmware)?;
        let mixin_sha = sha256(&mixin);
        let map = std::fs::read(self.executor.download(&release.root_map)?).map_err(|e| Fail::Other(e.into()))?;
        let devicetree = self.executor.devicetree(&device.board)?;
        let mut plan = build_plan(&release, &gpt, &devicetree, &map, &platform.layer, (mixin.len() as u64, &mixin_sha))?;
        // Resolved as an asset before PrepareInstall; None when no package
        // is published for exactly this layer.
        let kernel = self.executor.kernel_package(&device.board)?;
        if let Some((pkg, _)) = &kernel {
            plan["kernel_package"] = json!({
                "path": format!("{STAGING}/{}", pkg.file),
                "base": pkg.base,
                "bytes": pkg.bytes,
                "sha256": pkg.sha256,
                "version": pkg.version,
            });
        }
        let session = self.session()?;
        let t = std::time::Instant::now();
        let nodes = session.plan(&plan)?;
        eprintln!("{} timing: plan written in {:.1} s; partitions {nodes}", stamp(), t.elapsed().as_secs_f64());
        self.stage()
    }

    /// Puts the files the plan names that the device doesn't hold yet (its
    /// `staged` list): after the plan, and again before streaming, so an
    /// install picked up after a restart has them too.
    fn stage(&mut self) -> std::result::Result<(), Fail> {
        let release = self.executor.release()?;
        let device = self.device.clone().ok_or(Fail::Other(Error::new(Code::NotFound, "no device")))?;
        let platform = release
            .platform_for(&device.board)
            .ok_or_else(|| Error::new(Code::NotFound, format!("{} does not cover this board yet", release.name)))?
            .clone();
        let staged = self.session()?.info()?.staged;
        let has = |name: &str| staged.iter().any(|f| f == name);
        if !has(LAYER) {
            let layer_path = self.executor.download(&platform.layer)?;
            let t = std::time::Instant::now();
            put_file(self.session()?, LAYER, &layer_path, platform.layer.bytes, &platform.layer.sha256)?;
            timed("put platform layer", platform.layer.bytes, t);
        }
        if !has(FIRMWARE) {
            let firmware = firmware_files(&self.executor.firmware_dir(&device.board))?;
            let mixin = install_env::firmware_mixin(&device.board, &firmware)?;
            let mixin_sha = sha256(&mixin);
            let t = std::time::Instant::now();
            self.session()?.put(FIRMWARE, mixin.len() as u64, &mixin_sha, &mut mixin.as_slice())?;
            timed("put firmware", mixin.len() as u64, t);
        }
        if let Some((pkg, path)) = self.executor.kernel_package(&device.board)? {
            if !has(&pkg.file) {
                let t = std::time::Instant::now();
                put_file(self.session()?, &pkg.file, &path, pkg.bytes, &pkg.sha256)?;
                timed("put kernel package", pkg.bytes, t);
            }
        }
        Ok(())
    }

    fn start(&mut self, account: &Account) -> std::result::Result<(), Fail> {
        eprintln!("{} timing: install pressed", stamp());
        // Anything the plan needs that isn't on the device yet (a resumed install).
        self.stage()?;
        let secrets = json!({"password": account.password, "pin": account.pin}).to_string().into_bytes();
        let sha = sha256(&secrets);
        self.session()?.put(SECRETS, secrets.len() as u64, &sha, &mut secrets.as_slice())?;
        self.stream()
    }

    fn stream(&mut self) -> std::result::Result<(), Fail> {
        let release = self.executor.release()?;
        let root = self.executor.download(&release.root)?;
        let mut file = std::fs::File::open(&root).map_err(|e| Fail::Other(e.into()))?;
        let t = std::time::Instant::now();
        eprintln!("{} timing: stream starts", stamp());
        self.session()?.stream(release.root.bytes, &release.root.sha256, &mut file)?;
        timed("stream root image (verified on the device)", release.root.bytes, t);
        self.send(Event::Streamed);
        Ok(())
    }
}

/// The plan for this device: the installer's target, the release's image,
/// layer and entry, the firmware mixin and the account without its secrets.
fn build_plan(
    release: &Release,
    gpt: &Gpt,
    devicetree: &str,
    map: &[u8],
    layer: &crate::release::Receipt,
    (firmware_bytes, firmware_sha): (u64, &str),
) -> Result<Value> {
    let target = install_plan::choose(gpt)?;
    let map: Value = serde_json::from_slice(map).map_err(|_| Error::new(Code::Format, "the root map is not JSON"))?;
    let image = json!({
        "bytes": release.root.bytes,
        "sha256": release.root.sha256,
        "expanded_bytes": release.root.expanded_bytes,
        // root.map.json as it is; the device makes orchard-write's map.
        "map": map,
    });
    let layer_json = json!({
        "path": format!("{STAGING}/{LAYER}"),
        "bytes": layer.bytes,
        "sha256": layer.sha256,
        "expanded_bytes": layer.expanded_bytes,
    });
    let firmware = json!({"path": format!("{STAGING}/{FIRMWARE}"), "bytes": firmware_bytes, "sha256": firmware_sha});
    let entry = install_plan::entry(&release.name, release.cmdline.as_deref(), &format!("{devicetree}.dtb"));
    let account = release.account.clone().map(|mut a| {
        a["ssh_keys"] = json!([]);
        a
    });
    let sizes = Sizes {
        image_expanded: release.root.expanded_bytes.unwrap_or(0),
        layer_expanded: layer.expanded_bytes.unwrap_or(0),
    };
    install_plan::plan(gpt, &target, sizes, image, layer_json, entry, Some(firmware), account, &mut uuid4)
}

fn put_file(session: &mut Session, name: &str, path: &Path, bytes: u64, sha: &str) -> std::result::Result<(), Fail> {
    let mut file = std::fs::File::open(path).map_err(|e| Fail::Other(e.into()))?;
    session.put(name, bytes, sha, &mut file).map_err(Fail::Proto)
}

/// The built firmware outputs below `dir`, as (destination, bytes).
fn firmware_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let root = dir.join("lib/firmware");
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dir) {
                out.push((rel.to_string_lossy().replace('\\', "/"), std::fs::read(&path)?));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// A random version-4 UUID.
fn uuid4() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

enum Fail {
    Proto(Failure),
    Other(Error),
}

impl From<Failure> for Fail {
    fn from(f: Failure) -> Fail {
        Fail::Proto(f)
    }
}

impl From<Error> for Fail {
    fn from(e: Error) -> Fail {
        Fail::Other(e)
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Proto(e) => e.fmt(f),
            Fail::Other(e) => e.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_layout_counts_linux_and_boot_as_omarchys_and_the_rest_as_ipados() {
        let gpt = Gpt::from_inspect(&json!({
            "disk": {"path": "/dev/nvme0n1", "sector": 4096, "bytes": 128_000_000_000u64, "table_sha256": "t"},
            "partitions": [
                {"number": 1, "start": 6, "blocks": 15_625_000, "type": "7c3457ef-0000-11aa-aa11-00306543ecac", "partuuid": "a", "fs": "apfs", "fs_uuid": null, "label": null},
                {"number": 2, "start": 15_625_006, "blocks": 244_140, "name": "BOOT", "type": XBOOTLDR, "partuuid": "b", "fs": "vfat", "fs_uuid": "1A2B-3C4D", "label": null},
                {"number": 3, "start": 15_869_146, "blocks": 15_380_854, "name": "omarchy-a", "type": install_plan::ROOT_ARM64, "partuuid": "c", "fs": "ext4", "fs_uuid": "u", "label": "omarchy-a"}
            ],
            "free": []
        }))
        .unwrap();
        let layout = flow_layout(&gpt);
        assert_eq!((layout.total_gb, layout.ipados_gb), (128, 64));
        let names: Vec<&str> = layout.outside.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["BOOT", "omarchy-a"]);
        assert_eq!(layout.outside[1].filesystem, "ext4");
    }

    #[test]
    fn uuids_are_version_4() {
        let u = uuid4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
    }
}

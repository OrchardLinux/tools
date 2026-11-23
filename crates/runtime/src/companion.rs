//! Talks to the companion's helper on an iPad running iPadOS with the
//! companion overlay (companion/PROTOCOL.md), through usbmux. Reports the
//! storage layout and the split's progress to the flow as events.
use crate::usbmux;
use orchard_cache::{Code, Error, Result};
use orchard_flow::{Device, Event, Layout, Partition};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub const PORT: u16 = 49720;
const PROTOCOL: u64 = 1;
const PING: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_secs(5);

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::Network, message.into())
}

/// A connection to the helper.
pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

/// What the helper says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub ecid: Option<String>,
    pub machine: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePartition {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(rename = "firstLBA")]
    pub first_lba: i64,
    #[serde(rename = "lastLBA")]
    pub last_lba: i64,
    pub contents: String,
}

/// resize-apfs's `StorageLayout`, the parts the installer uses.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLayout {
    pub revision: String,
    pub sector_size: i64,
    #[serde(rename = "lastUsableLBA")]
    pub last_usable_lba: i64,
    pub partitions: Vec<StoragePartition>,
    #[serde(rename = "backingPartitionID")]
    pub backing_partition_id: String,
    pub container_bytes: i64,
    pub allocated_bytes: i64,
    pub minimum_container_bytes: Option<i64>,
    #[serde(default)]
    pub issue: Option<String>,
    #[serde(skip)]
    backing: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
    /// The plan it carries out (only its target is read).
    #[serde(default)]
    pub plan: Option<OperationPlan>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationPlan {
    pub target_bytes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub layout: StorageLayout,
    pub operation: Option<Operation>,
}

impl StorageLayout {
    fn apfs(&self) -> Option<&StoragePartition> {
        self.backing.map(|i| &self.partitions[i])
    }

    fn resolve(mut self, raw: &Value) -> Result<StorageLayout> {
        let ids: Vec<String> = raw["partitions"]
            .as_array()
            .map(|a| a.iter().map(|p| p["id"].as_str().unwrap_or_default().to_owned()).collect())
            .unwrap_or_default();
        self.backing = ids.iter().position(|id| *id == self.backing_partition_id);
        if self.apfs().is_none_or(|p| p.kind != "apfs") || self.sector_size <= 0 {
            return Err(fail("the companion's layout has no APFS container"));
        }
        Ok(self)
    }

    /// Bytes from the start of iPadOS's container to the end of the disk:
    /// what is split between iPadOS and Omarchy A.
    pub fn splittable_bytes(&self) -> i64 {
        let apfs = self.apfs().unwrap();
        (self.last_usable_lba - apfs.first_lba + 1) * self.sector_size
    }

    /// The flow's view: iPadOS keeps at least what APFS reports as its
    /// smallest size; everything after it becomes Omarchy A's.
    pub fn flow_layout(&self) -> Layout {
        let gb = |b: i64| (b.max(0) as f64 / 1e9).round() as u32;
        let up = |b: i64| (b.max(0) as f64 / 1e9).ceil() as u32;
        let apfs = self.apfs().unwrap();
        let used = self.allocated_bytes.max(self.minimum_container_bytes.unwrap_or(0));
        let outside = self
            .partitions
            .iter()
            .filter(|p| p.first_lba > apfs.last_lba)
            .map(|p| Partition {
                name: p.name.clone(),
                filesystem: p.contents.clone(),
                gb: gb((p.last_lba - p.first_lba + 1) * self.sector_size),
            })
            .collect();
        Layout {
            total_gb: gb(self.splittable_bytes()),
            ipados_gb: gb((apfs.last_lba - apfs.first_lba + 1) * self.sector_size),
            ipados_used_gb: up(used),
            outside,
        }
    }

    /// The partitions after APFS: the split removes them all (the user
    /// approved erasing them), leaving iPadOS and free space after it.
    pub fn following(&self) -> Vec<String> {
        let apfs = self.apfs().unwrap();
        self.partitions.iter().filter(|p| p.first_lba > apfs.last_lba).map(|p| p.id.clone()).collect()
    }

    /// The container size that leaves `omarchy_gb` for Omarchy A.
    pub fn apfs_bytes_for(&self, omarchy_gb: u32) -> Result<i64> {
        let bytes = self.splittable_bytes() - i64::from(omarchy_gb) * 1_000_000_000;
        let floor = self.allocated_bytes.max(self.minimum_container_bytes.unwrap_or(0));
        if bytes < floor {
            return Err(Error::new(Code::InvalidInput, "iPadOS can't give up that much space"));
        }
        Ok(bytes / self.sector_size * self.sector_size)
    }
}

impl Client {
    /// Connects to the helper of the device with this UDID and proves the
    /// overlay's token.
    pub fn connect(socket: &Path, udid: &str, token: &str) -> Result<(Client, Hello)> {
        let stream = usbmux::connect(socket, udid, PORT)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut client = Client { reader: BufReader::new(stream.try_clone()?), stream };
        let hello = client.request(json!({"hello": {"token": token, "protocol": PROTOCOL}}))?;
        if hello["protocol"].as_u64() != Some(PROTOCOL) {
            return Err(fail("the companion speaks another protocol version"));
        }
        let ecid = hello["ecid"].as_str().map(crate::ecid_key);
        let machine = hello["machine"].as_str().unwrap_or_default().to_owned();
        Ok((client, Hello { ecid, machine }))
    }

    fn request(&mut self, body: Value) -> Result<Value> {
        let mut line = body.to_string().into_bytes();
        line.push(b'\n');
        self.stream.write_all(&line)?;
        let mut answer = String::new();
        if self.reader.read_line(&mut answer)? == 0 {
            return Err(fail("the companion closed the connection"));
        }
        let value: Value = serde_json::from_str(&answer).map_err(|_| fail("the companion's answer is not JSON"))?;
        if let Some(ok) = value.get("ok") {
            return Ok(ok.clone());
        }
        let message = value["error"]["message"].as_str().unwrap_or("the companion refused the request");
        Err(Error::new(Code::InvalidInput, message.to_owned()))
    }

    /// The helper's log (read-only).
    pub fn log(&mut self) -> Result<String> {
        Ok(self.request(json!({"log": {}}))?["text"].as_str().unwrap_or_default().to_owned())
    }

    /// Restart the iPad (the installer quits from the temporary iPadOS).
    pub fn restart(&mut self) -> Result<()> {
        self.request(json!({"restart": {}})).map(drop)
    }

    pub fn ping(&mut self) -> Result<()> {
        self.request(json!({"ping": {}})).map(drop)
    }

    /// The snapshot as the helper sends it (diagnostics).
    pub fn snapshot_raw(&mut self) -> Result<Value> {
        self.request(json!({"snapshot": {}}))
    }

    pub fn snapshot(&mut self) -> Result<Snapshot> {
        let raw = self.request(json!({"snapshot": {}}))?;
        let mut snapshot: Snapshot =
            serde_json::from_value(raw.clone()).map_err(|e| fail(format!("unreadable snapshot: {e}")))?;
        snapshot.layout = snapshot.layout.resolve(&raw["layout"])?;
        Ok(snapshot)
    }

    pub fn split(&mut self, apfs_bytes: i64, revision: &str, replacing: &[String]) -> Result<()> {
        let mut body = json!({"apfs_bytes": apfs_bytes, "revision": revision});
        if !replacing.is_empty() {
            body["replacing"] = json!(replacing);
        }
        self.request(json!({ "split": body })).map(drop)
    }
}

pub enum Job {
    /// The device runs iPadOS: connect to its companion.
    Attach { udid: String, device: Device, token: String },
    /// Split, leaving `omarchy_gb` for Omarchy A.
    Split { omarchy_gb: u32 },
    /// Read the storage again and report it.
    Refresh,
    /// Restart the iPad; answers when the companion took the request (or not).
    Restart(Sender<bool>),
}

/// Starts the companion worker; jobs go in, flow events come out.
/// The helper's log is copied into `logs` while connected.
pub fn spawn(events: Sender<Event>, logs: std::path::PathBuf) -> Sender<Job> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || run(rx, events, logs));
    tx
}

/// The iPad the worker keeps a connection to.
struct Target {
    udid: String,
    device: Device,
    token: String,
}

/// A live connection and what it knows.
struct Session {
    client: Client,
    layout: StorageLayout,
    splitting: bool,
}

/// How often a lost or not yet started companion is tried again.
const RETRY: Duration = Duration::from_secs(2);

/// How often the helper's log is copied while connected.
const LOG_EVERY: Duration = Duration::from_secs(30);

/// Copies the helper's log to `logs/companion-<udid>.log` (best effort).
fn save_log(client: &mut Client, logs: &Path, udid: &str) {
    match client.log() {
        Ok(text) => {
            let _ = std::fs::create_dir_all(logs);
            let short: String = udid.chars().take(12).collect();
            let _ = std::fs::write(logs.join(format!("companion-{short}.log")), text);
        }
        Err(e) => eprintln!("companion: log: {e}"),
    }
}

fn run(jobs: Receiver<Job>, events: Sender<Event>, logs: std::path::PathBuf) {
    let socket = Path::new(usbmux::SOCKET);
    let mut target: Option<Target> = None;
    let mut session: Option<Session> = None;
    // Whether the flow has this target's layout (later connections are
    // reconnects), and when to try connecting next.
    let mut announced = false;
    // A split this run started and hasn't seen end: watched again after a
    // reconnect, which may find it already done (the connection can drop
    // while the iPad works).
    let mut split_pending = false;
    let mut next_try = Instant::now();
    let mut last_error = String::new();
    let (mut last_ping, mut last_poll) = (Instant::now(), Instant::now());
    let mut last_log = Instant::now();
    loop {
        let job = match jobs.recv_timeout(Duration::from_millis(250)) {
            Ok(job) => Some(job),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match job {
            Some(Job::Attach { udid, device, token }) => {
                // The same iPad again (USB noticed it twice): keep the connection.
                let same = target.as_ref().is_some_and(|t| t.udid == udid && t.token == token);
                if !(same && session.is_some()) {
                    if !same {
                        announced = false;
                    }
                    target = Some(Target { udid, device, token });
                    session = None;
                    next_try = Instant::now();
                }
            }
            Some(Job::Split { omarchy_gb }) => {
                let result = session.as_mut().ok_or_else(|| fail("the companion is not connected")).and_then(|s| {
                    let bytes = s.layout.apfs_bytes_for(omarchy_gb)?;
                    let replacing = s.layout.following();
                    // Already split exactly so (only free space after APFS, the
                    // same size, e.g. after an accidental close): nothing to do.
                    if replacing.is_empty() && bytes == s.layout.container_bytes {
                        let _ = events.send(Event::SplitFinished { ok: true });
                        return Ok(());
                    }
                    s.client.split(bytes, &s.layout.revision, &replacing)?;
                    s.splitting = true;
                    split_pending = true;
                    last_poll = Instant::now();
                    Ok(())
                });
                if let Err(e) = result {
                    split_pending = false;
                    eprintln!("companion: split: {e}");
                    let _ = events.send(Event::SplitRefused(e.to_string()));
                    if let Some(s) = session.as_mut() {
                        if let Ok(fresh) = s.client.snapshot() {
                            s.layout = fresh.layout;
                            let _ = events.send(Event::IpadosReady { layout: s.layout.flow_layout() });
                        }
                    }
                }
            }
            Some(Job::Restart(done)) => {
                let ok = session.as_mut().is_some_and(|s| s.client.restart().map_err(|e| eprintln!("companion: restart: {e}")).is_ok());
                let _ = done.send(ok);
            }
            Some(Job::Refresh) => match session.as_mut().map(|s| s.client.snapshot().map(|f| (f, s))) {
                Some(Ok((fresh, s))) => {
                    s.layout = fresh.layout;
                    let _ = events.send(Event::IpadosReady { layout: s.layout.flow_layout() });
                }
                // Not answering: as a lost connection (Reconnecting…, then again).
                Some(Err(e)) => {
                    eprintln!("companion: refresh: {e}");
                    session = None;
                    next_try = Instant::now();
                    let _ = events.send(Event::CompanionLost);
                }
                None => {
                    eprintln!("companion: refresh: not connected");
                    let _ = events.send(Event::CompanionLost);
                }
            },
            None => {}
        }
        // Connect, or reconnect after a drop, while there is an iPad to reach.
        if session.is_none() && Instant::now() >= next_try {
            if let Some(t) = &target {
                next_try = Instant::now() + RETRY;
                match open(socket, t) {
                    Ok(Opened::Ready(mut client, snapshot)) => {
                        save_log(&mut client, &logs, &t.udid);
                        last_log = Instant::now();
                        let running = snapshot.operation.as_ref().filter(|o| o.status == "running");
                        let splitting = running.is_some() || split_pending;
                        if announced {
                            let _ = events.send(Event::CompanionBack);
                        } else if let Some(op) = running {
                            // A split this run didn't start is running (the
                            // installer was closed during it): watch it again.
                            let target = op.plan.as_ref().map_or(0, |p| p.target_bytes);
                            let omarchy_gb = ((snapshot.layout.splittable_bytes() - target).max(0) as f64 / 1e9).round() as u32;
                            let _ = events.send(Event::SplitRunning { device: t.device.clone(), omarchy_gb });
                            announced = true;
                        } else {
                            let _ = events.send(Event::IpadosReady { layout: snapshot.layout.flow_layout() });
                            announced = true;
                        }
                        session = Some(Session { client, layout: snapshot.layout.clone(), splitting });
                        // A split of ours: its state now, not in POLL.
                        last_poll = if split_pending { Instant::now() - POLL } else { Instant::now() };
                        last_ping = Instant::now();
                        last_error.clear();
                    }
                    // Not listening yet (still booting) or not reachable: try again.
                    Ok(Opened::NotYet(e)) => {
                        if e != last_error {
                            eprintln!("companion: waiting: {e}");
                            last_error = e;
                        }
                    }
                    // Answers, but can't be used: the flow says so.
                    Ok(Opened::Refused(e)) => {
                        eprintln!("companion: {e}");
                        let _ = events.send(Event::ChainFailed(e));
                        target = None;
                    }
                    Err(e) => eprintln!("companion: {e}"),
                }
            }
        }
        let Some(s) = session.as_mut() else { continue };
        let result = (|| -> Result<()> {
            if last_ping.elapsed() >= PING {
                s.client.ping()?;
                last_ping = Instant::now();
            }
            if last_log.elapsed() >= LOG_EVERY {
                last_log = Instant::now();
                if let Some(t) = &target {
                    save_log(&mut s.client, &logs, &t.udid);
                }
            }
            if s.splitting && last_poll.elapsed() >= POLL {
                last_poll = Instant::now();
                let snapshot = s.client.snapshot()?;
                match snapshot.operation.as_ref().map(|o| o.status.as_str()) {
                    Some("completed") => {
                        s.splitting = false;
                        split_pending = false;
                        s.layout = snapshot.layout;
                        let _ = events.send(Event::SplitFinished { ok: true });
                    }
                    Some("stopped" | "unknown") => {
                        s.splitting = false;
                        split_pending = false;
                        let why = snapshot.operation.and_then(|o| o.message).unwrap_or_else(|| "The split stopped.".into());
                        eprintln!("companion: split stopped: {why}");
                        let _ = events.send(Event::SplitRefused(why));
                        // The split is chosen again from the layout as it is now.
                        let fresh = s.client.snapshot()?;
                        s.layout = fresh.layout;
                        let _ = events.send(Event::IpadosReady { layout: s.layout.flow_layout() });
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            // Reconnect right away; the device may also freeze for a while
            // during the split, which USB tells the flow about.
            eprintln!("companion: connection lost: {e}");
            session = None;
            next_try = Instant::now();
            let _ = events.send(Event::CompanionLost);
        }
    }
}

enum Opened {
    Ready(Client, Box<Snapshot>),
    NotYet(String),
    Refused(String),
}

/// One connection attempt: checks it is the user's device and reads the
/// layout.
fn open(socket: &Path, t: &Target) -> Result<Opened> {
    let (mut client, hello) = match Client::connect(socket, &t.udid, &t.token) {
        Ok(c) => c,
        Err(e) => return Ok(Opened::NotYet(e.to_string())),
    };
    if hello.ecid.as_deref() != Some(t.device.id.0.as_str()) {
        return Ok(Opened::Refused("the iPad running iPadOS is not the device that was in DFU".into()));
    }
    let snapshot = match client.snapshot() {
        Ok(s) => s,
        Err(e) => return Ok(Opened::NotYet(e.to_string())),
    };
    if let Some(issue) = &snapshot.layout.issue {
        return Ok(Opened::Refused(issue.clone()));
    }
    Ok(Opened::Ready(client, Box::new(snapshot)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Value {
        // A 32 GB iPad: a small system partition, then APFS to the end.
        json!({
            "layout": {
                "revision": "6F9619FF-8B86-D011-B42D-00C04FC964FF", "diskID": "x", "diskName": "disk0",
                "deviceName": "iPad7,5", "sectorSize": 4096, "totalBlocks": 7_812_500,
                "firstUsableLBA": 6, "lastUsableLBA": 7_812_494,
                "partitions": [
                    {"id": "A", "name": "Other", "bsdName": "disk0s1", "kind": "other", "firstLBA": 6, "lastLBA": 97_655, "typeGUID": "x", "contents": "x"},
                    {"id": "B", "name": "Container", "bsdName": "disk0s2", "kind": "apfs", "firstLBA": 97_656, "lastLBA": 7_812_494, "typeGUID": "x", "contents": "APFS"}
                ],
                "containerID": "c", "backingPartitionID": "B", "containerBytes": 31_600_000_000u64,
                "allocatedBytes": 18_400_000_000u64, "minimumContainerBytes": 20_000_000_000u64,
                "growthSupported": true, "issue": null
            },
            "operation": null
        })
    }

    fn parse(raw: Value) -> Snapshot {
        let mut s: Snapshot = serde_json::from_value(raw.clone()).unwrap();
        s.layout = s.layout.resolve(&raw["layout"]).unwrap();
        s
    }

    #[test]
    fn the_engine_layout_becomes_the_flow_layout() {
        let s = parse(snapshot());
        let l = s.layout.flow_layout();
        assert_eq!((l.total_gb, l.ipados_gb, l.ipados_used_gb), (32, 32, 20), "iPadOS keeps APFS's minimum");
        assert!(l.outside.is_empty());
        assert_eq!(l.max_omarchy_gb(), 10);
    }

    #[test]
    fn a_split_names_all_partitions_after_apfs() {
        let mut raw = snapshot();
        // APFS shrunk to 5,000,000 blocks, then boot and root partitions after it.
        raw["layout"]["partitions"][1]["lastLBA"] = json!(4_999_999);
        raw["layout"]["partitions"].as_array_mut().unwrap().extend([
            json!({"id": "C", "name": "boot", "bsdName": "disk0s3", "kind": "linux", "firstLBA": 5_000_000, "lastLBA": 5_249_999, "typeGUID": "x", "contents": "x"}),
            json!({"id": "D", "name": "omarchy-a", "bsdName": "disk0s4", "kind": "linux", "firstLBA": 5_250_000, "lastLBA": 7_812_494, "typeGUID": "x", "contents": "x"}),
        ]);
        let s = parse(raw);
        assert_eq!(s.layout.following(), vec!["C".to_string(), "D".to_string()]);
        assert!(parse(snapshot()).layout.following().is_empty());
    }

    #[test]
    fn a_split_leaves_the_chosen_space_and_refuses_too_much() {
        let s = parse(snapshot());
        let bytes = s.layout.apfs_bytes_for(8).unwrap();
        assert_eq!(bytes % 4096, 0);
        assert_eq!((s.layout.splittable_bytes() - bytes) / 1_000_000, 8000);
        assert!(s.layout.apfs_bytes_for(20).is_err());
    }
}

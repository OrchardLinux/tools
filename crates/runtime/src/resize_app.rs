//! Resize APFS, the app that resizes iPadOS on the device itself: the
//! newest release of OrchardLinux/resize-apfs, fetched and kept in the
//! cache, and the overlay palera1n boots iPadOS with to carry it.
//!
//! The overlay is the one resize-apfs's own `build-overlay.py` makes: the
//! app in place of palera1n's loader app, its helper with its launchd job
//! (writes allowed), and its entry program in palera1n's daemon job.
use crate::exec::Executor;
use crate::overlay::{self, Identity, Payload};
use orchard_boards::data::archive::Archive;
use orchard_boards::release::Receipt;
use orchard_cache::{http, Code, Context, Error, Result};
use plist::Value;
use std::path::PathBuf;
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/OrchardLinux/resize-apfs/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(30);
/// The app's members in the release, below its top folder.
const APP: &str = "out/device/ResizeAPFS.app/";
const HELPER: &str = "out/device/resize-helper";
const ENTRY: &str = "out/device/resize-overlay-entry";
const JOB: &str = "Device/dev.omarchy.resize-apfs.plist";
/// The most one member may unpack to.
const MEMBER: u64 = 64 << 20;

/// A release of the app, unpacked.
pub struct App {
    pub version: String,
    files: Vec<(String, Vec<u8>, u16)>,
    executable: String,
    helper: Vec<u8>,
    entry: Vec<u8>,
    job: Vec<u8>,
}

fn format(message: impl Into<String>) -> Error {
    Error::new(Code::Format, message.into()).context("the Resize APFS release")
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    let network = |e: ureq::Error| Error::new(Code::Network, e.to_string()).context(url);
    let mut response = http::agent(url, TIMEOUT)?.get(url).call().map_err(network)?;
    response.body_mut().with_config().limit(limit).read_to_vec().map_err(network)
}

/// The newest release: its version, and its ZIP with the checksum the
/// release publishes beside it.
fn newest() -> Result<(String, Receipt)> {
    let release: serde_json::Value = serde_json::from_slice(&get(LATEST, 1 << 20)?).map_err(|_| format("GitHub's answer is not JSON"))?;
    let version = release["tag_name"].as_str().ok_or_else(|| format("no version"))?.to_owned();
    let asset = |suffix: &str| {
        release["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
    };
    let zip = asset(".zip").ok_or_else(|| format("no ZIP"))?;
    let sums = asset(".zip.sha256").and_then(|a| a["browser_download_url"].as_str()).ok_or_else(|| format("no checksum"))?;
    let sums = String::from_utf8(get(sums, 4096)?).map_err(|_| format("the checksum is not text"))?;
    let sha256 = sums.split_whitespace().next().filter(|h| h.len() == 64).ok_or_else(|| format("the checksum is not a SHA-256"))?;
    let receipt = Receipt {
        url: zip["browser_download_url"].as_str().ok_or_else(|| format("no download address"))?.to_owned(),
        bytes: zip["size"].as_u64().ok_or_else(|| format("no size"))?,
        sha256: sha256.to_owned(),
        expanded_bytes: None,
    };
    Ok((version, receipt))
}

/// The newest release of the app, downloaded once and checked.
pub fn latest(executor: &Executor) -> Result<App> {
    let (version, receipt) = newest().context("looking up the newest Resize APFS")?;
    let zip = std::fs::read(executor.download(&receipt)?)?;
    let mut archive = Archive::open(zip)?;
    let names: Vec<String> = archive.entries().map(|e| e.name.clone()).collect();
    // Everything is below one folder named after the release.
    let top = names.first().and_then(|n| n.split_once('/')).map(|(top, _)| format!("{top}/")).ok_or_else(|| format("an empty ZIP"))?;
    let mut read = |name: &str| archive.read(&format!("{top}{name}"), MEMBER);
    let (helper, entry, job) = (read(HELPER)?, read(ENTRY)?, read(JOB)?);
    let mut files = Vec::new();
    for name in &names {
        if let Some(path) = name.strip_prefix(&top).and_then(|n| n.strip_prefix(APP)).filter(|p| !p.is_empty() && !p.ends_with('/')) {
            files.push((path.to_owned(), read(&format!("{APP}{path}"))?, 0o644));
        }
    }
    let info = files.iter().find(|(p, _, _)| p == "Info.plist").ok_or_else(|| format("the app has no Info.plist"))?;
    let executable = Value::from_reader(std::io::Cursor::new(&info.1))
        .ok()
        .and_then(|v| v.as_dictionary()?.get("CFBundleExecutable")?.as_string().map(str::to_owned))
        .ok_or_else(|| format("the app names no executable"))?;
    Ok(App { version, files, executable, helper, entry, job })
}

impl App {
    /// The overlay that carries the app, made from the palera1n that will
    /// boot iPadOS with it (the one this run starts), written into the cache.
    pub fn overlay(&self, executor: &Executor) -> Result<PathBuf> {
        let (pin, release) = crate::flags::palera1n_pin().ok_or_else(|| Error::new(Code::NotFound, "palera1n publishes no build for this computer"))?;
        let receipt = Receipt { url: format!("{release}/{}", pin.asset), bytes: pin.size, sha256: pin.sha256.into(), expanded_bytes: None };
        let palera1n = &crate::tools::executable(&executor.download(&receipt)?, pin)?[..];
        // The helper may change the storage: the app asks before it does.
        let mut job = Value::from_reader(std::io::Cursor::new(&self.job)).map_err(|_| format("the helper's job is not a property list"))?;
        let arguments = job
            .as_dictionary_mut()
            .and_then(|d| d.get_mut("ProgramArguments"))
            .and_then(Value::as_array_mut)
            .ok_or_else(|| format("the helper's job has no arguments"))?;
        arguments.push("--allow-writes".into());
        let mut bytes = Vec::new();
        job.to_writer_xml(&mut bytes).map_err(|e| format(e.to_string()))?;
        let payload = Payload {
            app: &self.files,
            app_executable: &self.executable,
            helper: ("/usr/libexec/resize-helper", &self.helper),
            entry: ("/usr/libexec/resize-overlay-entry", &self.entry),
            job: ("/Library/LaunchDaemons/dev.omarchy.resize-apfs.plist", bytes),
            files: Vec::new(),
            boot_screen: Some(overlay::BOOT_SCREEN),
        };
        let image = overlay::build_with(palera1n, &payload, &Identity::random())?;
        let dir = executor.cache().join("overlays");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("resize-apfs-{}.dmg", self.version));
        std::fs::write(&path, image)?;
        Ok(path)
    }
}

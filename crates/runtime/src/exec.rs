//! Carries out the asset effects: downloads pinned files into the cache and
//! builds the peripheral firmware. Each effect runs on its own thread and
//! reports back as an [`Event`]. Nothing here talks to a device.
use crate::kernel_package::{self, Package};
use crate::release::{self, Receipt, Release};
use std::collections::HashMap;
use orchard_boards::firmware_build::{self, Request};
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::{Code, Error, Result};
use orchard_flow::assets::{palera1n_pin, PALERA1N_RELEASE};
use orchard_flow::{Asset, Device, Effect, Event};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(60);
const CHUNK: usize = 8 << 20;

/// A resolved kernel package and its cached file, or None when none is
/// published for exactly the layer.
pub type Resolved = Option<(Package, PathBuf)>;

/// A local file's hash as checked, with the size and modification time it had.
type Checked = (String, u64, Option<std::time::SystemTime>);

#[derive(Clone)]
pub struct Executor {
    /// A board data directory given for development (`--data`).
    explicit_data: Option<PathBuf>,
    /// The board data in use, once resolved.
    data: Arc<Mutex<Option<PathBuf>>>,
    /// Where finished files go, named by their SHA-256.
    cache: PathBuf,
    events: Sender<Event>,
    release: Arc<Mutex<Option<Release>>>,
    /// Where the release index is: the published one, or a local build's.
    index: String,
    /// A companion built into a directory, until the installer embeds it.
    companion: Option<PathBuf>,
    /// The built overlay and the token in it.
    overlay: Arc<Mutex<Option<(PathBuf, String)>>>,
    /// Each board's kernel package once resolved: the package and its cached
    /// file, or None when none is published for exactly the board's layer.
    kernel: Arc<Mutex<HashMap<String, Resolved>>>,
    /// Local files already checked against their hash, with the size and
    /// modification time they had then: hashed once, not before every use
    /// (the root image is 747 MB, about 3 s each time).
    verified: Arc<Mutex<HashMap<PathBuf, Checked>>>,
    /// A Pongo to use instead of the release's (`--pongo`, testing a build).
    pongo: Option<PathBuf>,
    /// One download per file at a time: whoever comes second waits and takes
    /// the finished file (two writers on one partial file broke each other).
    downloads: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
    tools: Arc<Mutex<Option<crate::orchard_tools::Tools>>>,
}

impl Executor {
    /// `data`: a board data directory to use instead of the release's.
    /// Where flow events go (palera1n's progress, from chain.rs).
    pub fn events(&self) -> Sender<Event> {
        self.events.clone()
    }

    pub fn new(data: Option<&Path>, cache: &Path, events: Sender<Event>) -> Executor {
        Executor { explicit_data: data.map(Path::to_owned), data: Arc::default(), cache: cache.to_owned(), events, release: Arc::default(), kernel: Arc::default(), index: String::new(), companion: None, overlay: Arc::default(), verified: Arc::default(), pongo: None, downloads: Arc::default(), tools: Arc::default() }
    }

    /// Starts the work an effect names. Effects that are not assets are left
    /// to other executors.
    pub fn run(&self, effect: &Effect, device: Option<&Device>) {
        let chain: Option<fn(&Executor, &Device) -> Result<()>> = match effect {
            Effect::BootInstallEnv(_) => Some(crate::chain::boot_install_env),
            Effect::BootIpadosWithOverlay(_) => Some(crate::chain::boot_ipados),
            _ => None,
        };
        if let (Some(chain), Some(device)) = (chain, device) {
            let (me, device) = (self.clone(), device.clone());
            std::thread::spawn(move || {
                if let Err(e) = chain(&me, &device) {
                    let _ = me.events.send(Event::ChainFailed(e.to_string()));
                }
            });
            return;
        }
        let Effect::Fetch(asset) = effect else { return };
        let (me, asset, board) = (self.clone(), *asset, device.map(|d| d.board.clone()));
        std::thread::spawn(move || {
            // Network trouble is normal: tried again quietly, waiting a
            // little longer each time. Only what retrying can't fix (a file
            // that doesn't match, a board the release lacks) or a network
            // gone for minutes reaches the user.
            const WAITS: [u64; 9] = [2, 3, 5, 10, 15, 30, 30, 60, 60];
            let mut waits = WAITS.iter();
            let event = loop {
                match me.fetch(asset, board.as_deref()) {
                    Ok(()) => break Event::AssetReady(asset),
                    Err(e) if matches!(e.code, Code::Network | Code::Io | Code::Timeout) => match waits.next() {
                        Some(&secs) => {
                            eprintln!("fetch {asset:?}: {e}; again in {secs} s");
                            std::thread::sleep(std::time::Duration::from_secs(secs));
                        }
                        None => break Event::AssetFailed(asset, format!("{e} (still failing after several minutes)")),
                    },
                    Err(e) => break Event::AssetFailed(asset, e.to_string()),
                }
            };
            let _ = me.events.send(event);
        });
    }

    fn fetch(&self, asset: Asset, board: Option<&str>) -> Result<()> {
        let need_board = || board.ok_or_else(|| Error::new(Code::NotFound, "no board known yet"));
        match asset {
            Asset::RootImage => {
                let rel = self.release()?;
                self.download(&rel.root_map)?;
                self.download(&rel.root).map(drop)
            }
            Asset::PlatformLayer => {
                let rel = self.release()?;
                let board = need_board()?;
                let platform = rel.platform_for(board).ok_or_else(|| {
                    Error::new(Code::NotFound, format!("{} {} does not cover {board} yet", rel.name, rel.version))
                })?;
                self.download(&platform.layer).map(drop)
            }
            Asset::Chain => {
                let tools = self.orchard_tools()?;
                self.download(&tools.pongo)?;
                self.download(&tools.m1n1)?;
                if self.index.is_empty() {
                    return Ok(());
                }
                self.download(&self.release()?.install_tools).map(drop)
            }
            Asset::Firmware => {
                let board = need_board()?;
                let out = self.firmware_dir(board);
                let request = Request::Peripheral { skip_unsupported: true };
                let store = self.cache.join("store");
                firmware_build::build(&self.data()?, Some(&store), board, &out, &request).map(drop)
            }
            Asset::Palera1n => {
                let pin = palera1n_pin(std::env::consts::OS, std::env::consts::ARCH)
                    .ok_or_else(|| Error::new(Code::NotFound, "palera1n publishes no build for this computer"))?;
                self.palera1n(pin).map(drop)
            }
            Asset::KernelPackage => {
                let rel = self.release()?;
                let board = need_board()?;
                let layer = &rel
                    .platform_for(board)
                    .ok_or_else(|| Error::new(Code::NotFound, format!("{} does not cover {board} yet", rel.name)))?
                    .layer;
                let layer_bytes = std::fs::read(self.download(layer)?)?;
                let found = match kernel_package::resolve(&rel.base, &layer_bytes, &layer.sha256)? {
                    Some(pkg) => {
                        let receipt = Receipt {
                            url: pkg.url.clone(),
                            bytes: pkg.bytes,
                            sha256: pkg.sha256.clone(),
                            expanded_bytes: None,
                        };
                        let path = self.download(&receipt)?;
                        Some((pkg, path))
                    }
                    None => None,
                };
                self.kernel.lock().expect("kernel lock").insert(board.to_owned(), found);
                Ok(())
            }
            Asset::Overlay => {
                // A companion directory given for development, else the built-in one.
                let companion = match &self.companion {
                    Some(dir) => crate::overlay::Companion::from_dir(dir)?,
                    None => crate::overlay::Companion::embedded()?,
                };
                let pin = palera1n_pin(std::env::consts::OS, std::env::consts::ARCH)
                    .ok_or_else(|| Error::new(Code::NotFound, "palera1n publishes no build for this computer"))?;
                let receipt = Receipt { url: format!("{PALERA1N_RELEASE}/{}", pin.asset), bytes: pin.size, sha256: pin.sha256.into(), expanded_bytes: None };
                let executable = crate::tools::executable(&self.download(&receipt)?, pin)?;
                let identity = crate::overlay::Identity::random();
                let image = crate::overlay::build(&executable, &companion, &identity)?;
                let dir = self.cache.join("overlays");
                std::fs::create_dir_all(&dir)?;
                let token = identity.token_hex();
                let path = dir.join(format!("overlay-{}.dmg", &token[..16]));
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
                    file.write_all(&image)?;
                }
                // Kept with the overlay so a relaunch can reach the companion again.
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    let current = dir.join("current.json");
                    let _ = std::fs::remove_file(&current);
                    let record = serde_json::json!({"path": path, "token": token}).to_string();
                    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&current)?.write_all(record.as_bytes())?;
                }
                *self.overlay.lock().expect("overlay lock") = Some((path, token));
                Ok(())
            }
        }
    }

    /// The palera1n executable, unpacked from its verified package.
    pub fn palera1n(&self, pin: &orchard_flow::assets::Pin) -> Result<PathBuf> {
        let receipt = Receipt {
            url: format!("{PALERA1N_RELEASE}/{}", pin.asset),
            bytes: pin.size,
            sha256: pin.sha256.into(),
            expanded_bytes: None,
        };
        let package = self.download(&receipt)?;
        crate::tools::install(&package, pin, &self.cache.join("tools"))
    }

    /// A new log file for a tool's output, under the cache's `logs/`.
    pub fn log_file(&self, tool: &str) -> Result<(std::fs::File, PathBuf)> {
        let dir = self.cache.join("logs");
        std::fs::create_dir_all(&dir)?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = dir.join(format!("{tool}-{stamp}.log"));
        Ok((std::fs::File::create(&path)?, path))
    }

    /// Uses this Pongo for every boot instead of the release's.
    pub fn with_pongo(mut self, path: &Path) -> Executor {
        self.pongo = Some(path.to_owned());
        self
    }

    /// orchard2's cache directory.
    pub fn cache(&self) -> &Path {
        &self.cache
    }

    /// The Pongo every boot sends: `--pongo`'s, else the release's.
    pub fn pongo_image(&self) -> Result<PathBuf> {
        match &self.pongo {
            Some(path) => Ok(path.clone()),
            None => {
                let tools = self.orchard_tools()?;
                self.download(&tools.pongo)
            }
        }
    }

    /// Orchard's tools (Pongo, m1n1, board data): orchard's newest release,
    /// looked up once per run.
    pub fn orchard_tools(&self) -> Result<crate::orchard_tools::Tools> {
        let mut slot = self.tools.lock().expect("tools lock");
        if let Some(t) = slot.as_ref() {
            return Ok(t.clone());
        }
        // The installer takes the newest now; the launcher starts with what
        // it has and refreshes in the background.
        let t = if self.index.is_empty() {
            let me = self.clone();
            crate::orchard_tools::latest(&self.cache, move |r| {
                let _ = me.download(r);
            })?
        } else {
            crate::orchard_tools::newest(&self.cache)?
        };
        *slot = Some(t.clone());
        Ok(t)
    }

    /// Uses a companion built into a directory (see `overlay::Companion::from_dir`).
    pub fn with_companion(mut self, dir: &Path) -> Executor {
        self.companion = Some(dir.to_owned());
        self
    }

    /// The overlay the Overlay asset built.
    pub fn overlay(&self) -> Result<PathBuf> {
        self.overlay
            .lock()
            .expect("overlay lock")
            .as_ref()
            .map(|(p, _)| p.clone())
            .ok_or_else(|| Error::new(Code::NotFound, "the overlay is not built yet"))
    }

    /// The token in the built overlay, which the host proves itself with to
    /// the companion.
    pub fn companion_token(&self) -> Option<String> {
        let mut slot = self.overlay.lock().expect("overlay lock");
        if slot.is_none() {
            // After a relaunch: the overlay built last, if it is still there.
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(self.cache.join("overlays/current.json")).ok()?).ok()?;
            let path = PathBuf::from(record["path"].as_str()?);
            let token = record["token"].as_str()?.to_owned();
            if path.is_file() {
                *slot = Some((path, token));
            }
        }
        slot.as_ref().map(|(_, t)| t.clone())
    }

    /// The board's kernel package as the KernelPackage asset resolved it.
    pub fn kernel_package(&self, board: &str) -> Result<Resolved> {
        self.kernel
            .lock()
            .expect("kernel lock")
            .get(board)
            .cloned()
            .ok_or_else(|| Error::new(Code::NotFound, "the kernel package is not resolved yet"))
    }

    /// The board data (orchard's `devices/`, `chips/`, `platforms/` …): the
    /// directory given with `--data`, else the release's own, unpacked once
    /// into the cache under its hash; releases that don't ship it yet fall
    /// back to the cached copy of orchard's main branch.
    pub fn data(&self) -> Result<PathBuf> {
        let mut slot = self.data.lock().expect("data lock");
        if let Some(dir) = slot.as_ref() {
            return Ok(dir.clone());
        }
        let dir = match &self.explicit_data {
            Some(dir) => dir.clone(),
            None => match Some(self.orchard_tools()?.board_data) {
                Some(receipt) => self.board_data(&receipt)?,
                None => {
                    let store = orchard_cache::store::Store::open(Some(&self.cache.join("store")))?;
                    orchard_boards::data::board_data::resolve(None, &store)?.dir
                }
            },
        };
        *slot = Some(dir.clone());
        Ok(dir)
    }

    /// The release's board data, unpacked under its hash (once).
    fn board_data(&self, receipt: &Receipt) -> Result<PathBuf> {
        let dir = self.cache.join("board-data").join(&receipt.sha256);
        if dir.join(".complete").is_file() {
            return Ok(dir);
        }
        let archive = std::fs::read(self.download(receipt)?)?;
        let partial = dir.with_extension("partial");
        let _ = std::fs::remove_dir_all(&partial);
        unpack_board_data(&archive, &partial)?;
        std::fs::write(partial.join(".complete"), &receipt.sha256)?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&partial, &dir)?;
        Ok(dir)
    }

    /// The board's device tree name (without `.dtb`), from the catalog.
    pub fn devicetree(&self, board: &str) -> Result<String> {
        let b = orchard_boards::catalog::Catalog::open(&self.data()?)?.board(board)?;
        b.dtb.ok_or_else(|| Error::new(Code::NotFound, format!("{board} names no device tree")))
    }

    /// Where the board's firmware outputs are built.
    pub fn firmware_dir(&self, board: &str) -> PathBuf {
        self.cache.join("firmware").join(board)
    }

    /// The release index to install from (the stamp's).
    pub fn with_index(mut self, url: &str) -> Executor {
        self.index = url.to_owned();
        self
    }

    /// Installs a local release build instead of the published one: its
    /// directory (with `index.toml`), or its `index.toml`. Files are read
    /// where they are and checked like downloads.
    pub fn with_local_release(mut self, path: &Path) -> Result<Executor> {
        let path = std::fs::canonicalize(path).map_err(|e| Error::from(e).context(path.display()))?;
        let index = if path.is_dir() { path.join("index.toml") } else { path };
        self.index = format!("file://{}", index.display());
        Ok(self)
    }

    /// The current release, read once from the index.
    pub fn release(&self) -> Result<Release> {
        let mut slot = self.release.lock().expect("release lock");
        if let Some(rel) = slot.as_ref() {
            return Ok(rel.clone());
        }
        if self.index.is_empty() {
            return Err(Error::new(Code::InvalidInput, "this installer has no release to install (it isn't stamped)"));
        }
        let index = match self.index.strip_prefix("file://") {
            Some(path) => std::fs::read(path).map_err(|e| Error::from(e).context(path))?,
            None => {
                let mut http = Http::probe(&self.index, TIMEOUT)?;
                let size = http.size() as usize;
                http.fetch(0, size)?
            }
        };
        let text = String::from_utf8(index).map_err(|_| Error::new(Code::Format, "index is not UTF-8"))?;
        let manifest = release::parse_index(&text, release::base_of(&self.index))?;
        let bytes = get(&manifest)?;
        let text = String::from_utf8(bytes).map_err(|_| Error::new(Code::Format, "release is not UTF-8"))?;
        let rel = release::parse_release(&text, release::base_of(&manifest.url))?;
        *slot = Some(rel.clone());
        Ok(rel)
    }

    /// Streams a pinned file into the cache, checking its size and SHA-256
    /// before it gets its final name. A file already there is reused.
    pub fn download(&self, receipt: &Receipt) -> Result<PathBuf> {
        if let Some(path) = receipt.url.strip_prefix("file://") {
            let path = Path::new(path);
            let meta = std::fs::metadata(path).ok();
            let stamp = meta.as_ref().map(|m| (m.len(), m.modified().ok()));
            let known = self.verified.lock().expect("verified lock").get(path).cloned();
            if let (Some((sha, len, at)), Some((now_len, now_at))) = (known, stamp) {
                if sha == receipt.sha256 && len == now_len && at == now_at {
                    return Ok(path.to_owned());
                }
            }
            let checked = verify_local(path, receipt)?;
            if let Some((len, at)) = stamp {
                self.verified.lock().expect("verified lock").insert(path.to_owned(), (receipt.sha256.clone(), len, at));
            }
            return Ok(checked);
        }
        let dir = self.cache.join("files");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(&receipt.sha256);
        let gate = self.downloads.lock().expect("downloads lock").entry(receipt.sha256.clone()).or_default().clone();
        let _one_at_a_time = gate.lock().expect("download lock");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == receipt.bytes) {
            return Ok(path);
        }
        let partial = dir.join(format!("{}.partial", receipt.sha256));
        let mut file = std::fs::File::create(&partial)?;
        let mut hash = Sha256::new();
        let mut http = Http::new(&receipt.url, receipt.bytes, TIMEOUT)?;
        let mut offset = 0u64;
        while offset < receipt.bytes {
            let len = CHUNK.min((receipt.bytes - offset) as usize);
            let chunk = http.fetch(offset, len)?;
            hash.update(&chunk);
            file.write_all(&chunk)?;
            offset += chunk.len() as u64;
        }
        file.sync_all()?;
        let actual = hex(&hash.finalize());
        if actual != receipt.sha256 {
            let _ = std::fs::remove_file(&partial);
            return Err(Error::new(Code::Format, format!("SHA-256 is {actual}, the release expects {}", receipt.sha256))
                .context(&receipt.url));
        }
        std::fs::rename(&partial, &path)?;
        Ok(path)
    }
}

/// The parts of a board data archive that are board data.
const BOARD_DATA: &[&str] = &["devices/", "chips/", "platforms/", "features.toml"];

/// Unpacks a board data archive (tar.gz) into `dir`: only board data, only
/// plain relative paths.
pub fn unpack_board_data(archive: &[u8], dir: &Path) -> Result<()> {
    use std::io::Read;
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive).take(256 << 20).read_to_end(&mut tar)?;
    let files = orchard_formats::tar::read(&tar)?.files;
    let mut kept = 0;
    for file in files {
        let path = file.path.trim_start_matches("./");
        let plain = !path.is_empty() && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..");
        if !plain || !BOARD_DATA.iter().any(|k| path == k.trim_end_matches('/') || path.starts_with(k)) {
            continue;
        }
        let target = dir.join(path);
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(&target, file.data)?;
        kept += 1;
    }
    if !dir.join("devices").is_dir() || kept == 0 {
        return Err(Error::new(Code::Format, "the release's board data has no devices/"));
    }
    Ok(())
}

/// A local release file, checked in place: size, then SHA-256 while reading.
fn verify_local(path: &Path, receipt: &Receipt) -> Result<PathBuf> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| Error::from(e).context(path.display()))?;
    if file.metadata()?.len() != receipt.bytes {
        return Err(Error::new(Code::Format, "the file's size differs from the release").context(path.display()));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let actual = hex(&hash.finalize());
    if actual != receipt.sha256 {
        return Err(Error::new(Code::Format, format!("SHA-256 is {actual}, the release expects {}", receipt.sha256))
            .context(path.display()));
    }
    Ok(path.to_owned())
}

/// A small pinned file, read into memory and checked.
fn get(receipt: &Receipt) -> Result<Vec<u8>> {
    if let Some(path) = receipt.url.strip_prefix("file://") {
        return Ok(std::fs::read(verify_local(Path::new(path), receipt)?)?);
    }
    let bytes = Http::new(&receipt.url, receipt.bytes, TIMEOUT)?.fetch(0, receipt.bytes as usize)?;
    let actual = hex(&Sha256::digest(&bytes));
    if actual != receipt.sha256 {
        return Err(Error::new(Code::Format, format!("SHA-256 is {actual}, expected {}", receipt.sha256)).context(&receipt.url));
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_formats::tar::{write, Entry};

    fn gz(entries: &[Entry]) -> Vec<u8> {
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&write(entries).unwrap()).unwrap();
        z.finish().unwrap()
    }

    #[test]
    fn board_data_unpacks_only_board_data() {
        let dir = std::env::temp_dir().join(format!("board-data-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let archive = gz(&[
            Entry { path: "devices/j71b/device.toml", mode: 0o644, data: b"x" },
            Entry { path: "./chips/t8010.toml", mode: 0o644, data: b"y" },
            Entry { path: "features.toml", mode: 0o644, data: b"z" },
            Entry { path: "src/orchard/cli.py", mode: 0o644, data: b"no" },
            Entry { path: "devices/../../escape", mode: 0o644, data: b"no" },
        ]);
        unpack_board_data(&archive, &dir).unwrap();
        assert!(dir.join("devices/j71b/device.toml").is_file());
        assert!(dir.join("chips/t8010.toml").is_file());
        assert!(dir.join("features.toml").is_file());
        assert!(!dir.join("src").exists());
        assert!(!dir.parent().unwrap().join("escape").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let empty = gz(&[Entry { path: "README.md", mode: 0o644, data: b"" }]);
        assert!(unpack_board_data(&empty, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

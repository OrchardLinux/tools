//! The iPadOS overlay that carries the installer's companion: palera1n's
//! own overlay (its embedded binpack) with the companion in place of
//! palera1n's loader app. palera1n boots iPadOS with it
//! (`palera1n -l -o overlay.dmg`).
//!
//! What changes in palera1n's overlay, as resize-apfs does it:
//! - `loader.dmg` holds only the companion's app, as `palera1nLoader.app`
//!   (the path palera1n's boot code opens);
//! - the companion's helper, its launchd job and a small entry program go
//!   into the outer image; palera1n's daemon job starts the entry program,
//!   which bootstraps the helper's job and then runs palera1n's daemon;
//! - the TV loader is removed, and the SSH, telnet and debug-shell jobs are
//!   disabled;
//! - palera1n's boot screen (`/usr/share/boot.jp2`, the pastel "palera1n"
//!   picture its boot code shows while iPadOS starts) is replaced by
//!   Orchard's: the black pear on white, between the two white Apple screens
//!   ([`BOOT_SCREEN`], 2732×2048 JPEG 2000 encoded like palera1n's own, shown
//!   cropped to portrait). Without the file the screen stays black;
//! - both images get new identities.
//!
//! Nothing else of palera1n's overlay is touched, and nothing here is
//! palera1n's code or the boot ROM exploit: the overlay is data palera1n
//! itself loads.
use orchard_cache::{Code, Error, Result};
use orchard_formats::hfs::{Entry, Kind, Meta, Volume};
use orchard_formats::{hfs_write, udif};
use plist::Value;

/// Where the binpack is mounted on the device.
const BINPACK: &str = "/cores/binpack";
const LOADER_APP: &str = "/palera1nLoader.app";
const DAEMONS: &str = "/Library/LaunchDaemons";
const HELPER: &str = "/usr/libexec/orchard-companion";
const ENTRY: &str = "/usr/libexec/orchard-companion-entry";
const JOB: &str = "/Library/LaunchDaemons/dev.orchard.companion.plist";
const LABEL: &str = "dev.orchard.companion";
/// The host's secret for this boot, readable by root only.
const TOKEN: &str = "/usr/share/orchard-companion/token";
/// The system the installer installs, for the iPad app's words.
const SYSTEM: &str = "/usr/share/orchard-companion/system";
const PARTITION: &str = "whole disk (Apple_HFSX : 0)";
/// palera1n's boot screen.
pub const BOOT_IMAGE: &str = "/usr/share/boot.jp2";
/// Orchard's boot screen, in its place.
pub const BOOT_SCREEN: &[u8] = include_bytes!("../assets/boot.jp2");

/// The companion as built for iOS.
#[derive(Debug, Clone)]
pub struct Companion {
    /// The app bundle's files: path inside the bundle, contents, mode.
    pub app: Vec<(String, Vec<u8>, u16)>,
    /// The app's executable, a path inside the bundle.
    pub app_executable: String,
    /// The root helper.
    pub helper: Vec<u8>,
    /// The entry program palera1n's daemon job starts.
    pub entry: Vec<u8>,
}

/// The companion built into this installer (see build.rs), if it was.
static EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/companion.tar"));

impl Companion {
    /// The companion built into the installer.
    pub fn embedded() -> Result<Companion> {
        if EMBEDDED.is_empty() {
            return Err(Error::new(Code::NotFound, "this installer was built without the iPad companion"));
        }
        let archive = orchard_formats::tar::read(EMBEDDED)?;
        let file = |name: &str| archive.files.iter().find(|f| f.path == name).map(|f| f.data.to_vec());
        let prefix = "OrchardCompanion.app/";
        let app: Vec<(String, Vec<u8>, u16)> = archive
            .files
            .iter()
            .filter_map(|f| f.path.strip_prefix(prefix).map(|p| (p.to_owned(), f.data.to_vec(), 0o644)))
            .collect();
        let info = file("OrchardCompanion.app/Info.plist").ok_or_else(|| format("the embedded companion has no Info.plist"))?;
        let app_executable = Value::from_reader(std::io::Cursor::new(info))
            .ok()
            .and_then(|v| v.as_dictionary()?.get("CFBundleExecutable")?.as_string().map(str::to_owned))
            .ok_or_else(|| format("the embedded companion names no executable"))?;
        Ok(Companion {
            app,
            app_executable,
            helper: file("orchard-companion").ok_or_else(|| format("the embedded companion has no helper"))?,
            entry: file("orchard-companion-entry").ok_or_else(|| format("the embedded companion has no entry program"))?,
        })
    }

    /// A companion built into a directory: `OrchardCompanion.app/` (the app
    /// bundle, its executable named in Info.plist's CFBundleExecutable),
    /// `orchard-companion` (the helper) and `orchard-companion-entry`.
    pub fn from_dir(dir: &std::path::Path) -> Result<Companion> {
        use std::os::unix::fs::PermissionsExt;
        let bundle = dir.join("OrchardCompanion.app");
        let info = Value::from_file(bundle.join("Info.plist")).map_err(|e| format(format!("the companion's Info.plist: {e}")))?;
        let app_executable = info
            .as_dictionary()
            .and_then(|d| d.get("CFBundleExecutable"))
            .and_then(Value::as_string)
            .ok_or_else(|| format("the companion's Info.plist names no executable"))?
            .to_owned();
        let mut app = Vec::new();
        let mut stack = vec![bundle.clone()];
        while let Some(folder) = stack.pop() {
            for entry in std::fs::read_dir(&folder)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_symlink() {
                    return Err(format("symbolic links in the companion's app are not supported"));
                }
                if kind.is_dir() {
                    stack.push(entry.path());
                } else {
                    let path = entry.path();
                    let relative = path.strip_prefix(&bundle).unwrap().to_string_lossy().into_owned();
                    let mode = (entry.metadata()?.permissions().mode() & 0o7777) as u16;
                    app.push((relative, std::fs::read(&path)?, mode));
                }
            }
        }
        app.sort();
        Ok(Companion {
            app,
            app_executable,
            helper: std::fs::read(dir.join("orchard-companion"))?,
            entry: std::fs::read(dir.join("orchard-companion-entry"))?,
        })
    }
}

/// What makes each built overlay unique.
pub struct Identity {
    /// The secret the host proves itself with to the helper.
    pub token: [u8; 32],
    /// Volume identifiers, outer then inner.
    pub volumes: [[u8; 8]; 2],
    /// Image segment identifiers, outer then inner.
    pub segments: [[u8; 16]; 2],
}

impl Identity {
    pub fn random() -> Identity {
        let mut bytes = [0u8; 32 + 16 + 32];
        getrandom::fill(&mut bytes).expect("system randomness");
        let at = |i: usize| bytes[i];
        Identity {
            token: bytes[..32].try_into().unwrap(),
            volumes: [std::array::from_fn(|i| at(32 + i)), std::array::from_fn(|i| at(40 + i))],
            segments: [std::array::from_fn(|i| at(48 + i)), std::array::from_fn(|i| at(64 + i))],
        }
    }

    pub fn token_hex(&self) -> String {
        self.token.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn format(message: impl Into<String>) -> Error {
    Error::new(Code::Format, message.into())
}

/// The one embedded image of palera1n's executable that is a compatible
/// overlay: HFS+ with `loader.dmg` and palera1n's daemon job.
pub fn binpack(palera1n: &[u8]) -> Result<&[u8]> {
    let mut found = Vec::new();
    for image in udif::embedded(palera1n) {
        let Ok(disk) = udif::read(image) else { continue };
        let Ok(mut volume) = Volume::open(disk.as_slice(), 0) else { continue };
        let fits = volume.read("loader.dmg").is_ok()
            && volume.read("Library/LaunchDaemons/palera1nd.plist").is_ok()
            && volume.read("usr/sbin/palera1nd").is_ok_and(|t| t == b"../../../payload");
        if fits {
            found.push(image);
        }
    }
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(format("this palera1n has no overlay the installer understands")),
        _ => Err(format("this palera1n has more than one overlay")),
    }
}

struct Image {
    name: String,
    created: u32,
    modified: u32,
    finder: [u8; 32],
    size: u64,
    entries: Vec<Entry>,
}

fn open(image: &[u8]) -> Result<Image> {
    let disk = udif::read(image)?;
    let mut volume = Volume::open(disk.as_slice(), 0)?;
    let header = &disk[1024..1536];
    let be32 = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
    Ok(Image {
        name: volume.name()?,
        created: be32(16),
        modified: be32(20),
        finder: header[80..112].try_into().unwrap(),
        size: disk.len() as u64,
        entries: volume.entries()?,
    })
}

fn close(image: Image, volume_id: [u8; 8], segment: [u8; 16]) -> Result<Vec<u8>> {
    let mut finder = image.finder;
    finder[24..32].copy_from_slice(&volume_id);
    let options = hfs_write::Options {
        name: image.name,
        block_size: 4096,
        created: image.created,
        modified: image.modified,
        finder,
        min_size: image.size,
        slack: 1 << 20,
    };
    let disk = hfs_write::write(&image.entries, &options)?;
    Ok(udif::write(&disk, &udif::WriteOptions { name: PARTITION, segment_id: segment })?)
}

fn root_owned(mode: u16, modified: u32) -> Meta {
    Meta { owner: 0, group: 0, mode, created: modified, modified, attributes_modified: modified, accessed: modified, ..Meta::default() }
}

fn plist_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    value.to_writer_xml(&mut out).map_err(|e| format(format!("plist: {e}")))?;
    Ok(out)
}

fn read_plist(entries: &[Entry], path: &str) -> Result<plist::Dictionary> {
    let entry = entries.iter().find(|e| e.path == path).ok_or_else(|| format(format!("the overlay has no {path}")))?;
    let Kind::File(data) = &entry.kind else { return Err(format(format!("{path} is not a file"))) };
    Value::from_reader(std::io::Cursor::new(data))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| format(format!("{path} is not a property list")))
}

/// Puts a file in place of whatever is at `path`, adding missing folders.
fn put(entries: &mut Vec<Entry>, path: &str, data: Vec<u8>, mode: u16, time: u32) {
    let mut folder = String::new();
    for part in path.trim_start_matches('/').split('/').collect::<Vec<_>>().split_last().unwrap().1 {
        folder.push('/');
        folder.push_str(part);
        if !entries.iter().any(|e| e.path == folder) {
            entries.push(Entry { path: folder.clone(), meta: root_owned(0o755, time), kind: Kind::Folder });
        }
    }
    entries.retain(|e| e.path != path);
    entries.push(Entry { path: path.to_owned(), meta: root_owned(mode, time), kind: Kind::File(data) });
}

/// The helper's launchd job.
fn job() -> Value {
    let mut d = plist::Dictionary::new();
    d.insert("Label".into(), LABEL.into());
    d.insert("ProgramArguments".into(), Value::Array(vec![format!("{BINPACK}{HELPER}").into()]));
    d.insert("RunAtLoad".into(), true.into());
    d.insert("KeepAlive".into(), true.into());
    d.insert("ThrottleInterval".into(), 30.into());
    d.insert("UserName".into(), "root".into());
    d.insert("GroupName".into(), "wheel".into());
    d.insert("ProcessType".into(), "Interactive".into());
    d.insert("AbandonProcessGroup".into(), true.into());
    d.insert("StandardOutPath".into(), "/cores/orchard-companion.log".into());
    d.insert("StandardErrorPath".into(), "/cores/orchard-companion.err".into());
    Value::Dictionary(d)
}

/// What an overlay carries in place of palera1n's loader app: an app, its
/// root helper with the launchd job that keeps it running, and the entry
/// program palera1n's daemon job starts.
pub struct Payload<'a> {
    /// The app bundle's files: path inside the bundle, contents, mode.
    pub app: &'a [(String, Vec<u8>, u16)],
    /// The app's executable, a path inside the bundle.
    pub app_executable: &'a str,
    /// The helper and the entry program, each with its path in the overlay.
    pub helper: (&'a str, &'a [u8]),
    pub entry: (&'a str, &'a [u8]),
    /// The helper's launchd job: its path in the overlay and the plist.
    pub job: (&'a str, Vec<u8>),
    /// More files: path, contents, mode.
    pub files: Vec<(&'a str, Vec<u8>, u16)>,
    /// A picture in place of palera1n's boot screen.
    pub boot_screen: Option<&'a [u8]>,
}

/// The overlay for this palera1n and companion.
pub fn build(palera1n: &[u8], companion: &Companion, identity: &Identity) -> Result<Vec<u8>> {
    let payload = Payload {
        app: &companion.app,
        app_executable: &companion.app_executable,
        helper: (HELPER, &companion.helper),
        entry: (ENTRY, &companion.entry),
        job: (JOB, plist_bytes(&job())?),
        files: vec![
            (TOKEN, identity.token_hex().into_bytes(), 0o600),
            (SYSTEM, orchard_flow::system_name().as_bytes().to_vec(), 0o644),
        ],
        boot_screen: Some(BOOT_SCREEN),
    };
    build_with(palera1n, &payload, identity)
}

/// The overlay for this palera1n with `payload` in it.
pub fn build_with(palera1n: &[u8], companion: &Payload, identity: &Identity) -> Result<Vec<u8>> {
    let mut outer = open(binpack(palera1n)?)?;
    let time = outer.modified;
    let loader = match outer.entries.iter().find(|e| e.path == "/loader.dmg").map(|e| &e.kind) {
        Some(Kind::File(data)) => data.clone(),
        _ => return Err(format("the overlay has no loader.dmg")),
    };

    // The inner image: only the companion's app, under the loader's name.
    let mut inner = open(&loader)?;
    let root = inner.entries.iter().find(|e| e.path == "/").cloned().ok_or_else(|| format("loader.dmg has no root"))?;
    inner.entries = vec![root, Entry { path: LOADER_APP.into(), meta: root_owned(0o755, time), kind: Kind::Folder }];
    if !companion.app.iter().any(|(p, _, _)| *p == companion.app_executable) {
        return Err(format("the app has no executable"));
    }
    for (path, data, mode) in companion.app {
        if path.is_empty() || path.starts_with('/') || path.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(format(format!("not a plain path in the app: {path}")));
        }
        let mode = if *path == companion.app_executable { mode | 0o755 } else { *mode };
        put(&mut inner.entries, &format!("{LOADER_APP}/{path}"), data.clone(), mode & 0o7777, time);
    }
    let loader = close(inner, identity.volumes[1], identity.segments[1])?;

    // The outer image.
    let entries = &mut outer.entries;
    put(entries, "/loader.dmg", loader, 0o644, time);
    entries.retain(|e| e.path != "/tvloader.dmg");
    if let Some(picture) = companion.boot_screen {
        put(entries, BOOT_IMAGE, picture.to_vec(), 0o644, time);
    }
    put(entries, companion.helper.0, companion.helper.1.to_vec(), 0o755, time);
    put(entries, companion.entry.0, companion.entry.1.to_vec(), 0o755, time);
    for (path, data, mode) in &companion.files {
        put(entries, path, data.clone(), *mode, time);
    }
    put(entries, companion.job.0, companion.job.1.clone(), 0o644, time);
    let mut daemon = read_plist(entries, "/Library/LaunchDaemons/palera1nd.plist")?;
    let program = format!("{BINPACK}{}", companion.entry.0);
    daemon.insert("Program".into(), program.clone().into());
    daemon.insert("ProgramArguments".into(), Value::Array(vec![program.into()]));
    daemon.insert("RunAtLoad".into(), true.into());
    put(entries, "/Library/LaunchDaemons/palera1nd.plist", plist_bytes(&Value::Dictionary(daemon))?, 0o644, time);
    let listeners: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            let name = e.path.strip_prefix(DAEMONS)?.strip_prefix('/')?;
            let listener = ["dropbear-", "telnetd", "debug-shell"].iter().any(|p| name.starts_with(p));
            (listener && name.ends_with(".plist") && !name.contains('/')).then(|| e.path.clone())
        })
        .collect();
    for path in listeners {
        let mut job = read_plist(entries, &path)?;
        job.insert("Disabled".into(), true.into());
        job.remove("Sockets");
        put(entries, &path, plist_bytes(&Value::Dictionary(job))?, 0o644, time);
    }
    close(outer, identity.volumes[0], identity.segments[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palera1n() -> Option<Vec<u8>> {
        // The Linux arm64 build in the installer-app folder, when present.
        let tgz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../installer-app/palera1n-linux-arm64.tar.gz");
        let pin = orchard_flow::assets::palera1n_pin("linux", "aarch64")?;
        crate::tools::executable(&tgz, pin).ok()
    }

    fn companion() -> Companion {
        Companion {
            app: vec![
                ("Info.plist".into(), b"<plist/>".to_vec(), 0o644),
                ("OrchardCompanion".into(), b"\xcf\xfa\xed\xfe app".to_vec(), 0o644),
                ("Assets/icon.png".into(), vec![1, 2, 3], 0o644),
            ],
            app_executable: "OrchardCompanion".into(),
            helper: b"\xcf\xfa\xed\xfe helper".to_vec(),
            entry: b"\xcf\xfa\xed\xfe entry".to_vec(),
        }
    }

    fn file(entries: &[Entry], path: &str) -> (Vec<u8>, Meta) {
        let e = entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("{path} missing"));
        match &e.kind {
            Kind::File(d) | Kind::Symlink(d) => (d.clone(), e.meta),
            Kind::Folder => panic!("{path} is a folder"),
        }
    }

    #[test]
    fn the_overlay_carries_the_companion_and_nothing_listens() {
        let Some(palera1n) = palera1n() else { return };
        let identity = Identity::random();
        let image = build(&palera1n, &companion(), &identity).unwrap();
        let outer = open(&image).unwrap();
        let original = open(binpack(&palera1n).unwrap()).unwrap();
        let e = &outer.entries;

        assert_eq!(file(e, HELPER).0, companion().helper);
        assert_eq!(file(e, HELPER).1.mode & 0o7777, 0o755);
        let (token, meta) = file(e, TOKEN);
        assert_eq!((token, meta.mode & 0o777, meta.owner), (identity.token_hex().into_bytes(), 0o600, 0));
        let daemon = read_plist(e, "/Library/LaunchDaemons/palera1nd.plist").unwrap();
        assert_eq!(daemon["Program"].as_string(), Some("/cores/binpack/usr/libexec/orchard-companion-entry"));
        assert_eq!(file(e, "/usr/sbin/palera1nd").0, b"../../../payload", "palera1n's daemon stays");
        assert!(!e.iter().any(|x| x.path == "/tvloader.dmg"));
        assert_eq!(file(e, BOOT_IMAGE).0, BOOT_SCREEN, "Orchard's boot screen in place of palera1n's");
        for x in e.iter().filter(|x| x.path.starts_with("/Library/LaunchDaemons/dropbear-")) {
            let job = read_plist(e, &x.path).unwrap();
            assert_eq!(job.get("Disabled").and_then(Value::as_boolean), Some(true));
            assert!(!job.contains_key("Sockets"));
        }
        // Everything else is palera1n's, unchanged.
        let changed = ["/loader.dmg", "/Library/LaunchDaemons/palera1nd.plist", "/tvloader.dmg", BOOT_IMAGE];
        for o in &original.entries {
            let disabled = o.path.starts_with("/Library/LaunchDaemons/") && ["dropbear-", "telnetd", "debug-shell"].iter().any(|p| o.path[23..].starts_with(p));
            if changed.contains(&o.path.as_str()) || disabled || o.kind == Kind::Folder {
                continue;
            }
            let now = e.iter().find(|x| x.path == o.path).unwrap_or_else(|| panic!("{} lost", o.path));
            assert_eq!(now, o, "{} changed", o.path);
        }
        assert_ne!(outer.finder[24..32], original.finder[24..32], "a new volume identity");

        let inner = open(&file(e, "/loader.dmg").0).unwrap();
        let paths: Vec<&str> = inner.entries.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, ["/", "/palera1nLoader.app", "/palera1nLoader.app/Assets", "/palera1nLoader.app/Assets/icon.png", "/palera1nLoader.app/Info.plist", "/palera1nLoader.app/OrchardCompanion"]);
        assert_eq!(file(&inner.entries, "/palera1nLoader.app/OrchardCompanion").1.mode & 0o777, 0o755);
        assert!(inner.entries.iter().all(|x| x.meta.owner == 0 && x.meta.group == 0));
    }

    #[test]
    fn the_embedded_companion_matches_its_build() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../companion/out");
        let Ok(built) = Companion::from_dir(&dir) else { return };
        let embedded = Companion::embedded().unwrap();
        assert_eq!(embedded.helper, built.helper);
        assert_eq!(embedded.app_executable, built.app_executable);
        let names = |c: &Companion| c.app.iter().map(|(p, d, _)| (p.clone(), d.clone())).collect::<Vec<_>>();
        assert_eq!(names(&embedded), names(&built));
    }

    #[test]
    fn app_paths_must_be_plain() {
        let Some(palera1n) = palera1n() else { return };
        let mut bad = companion();
        bad.app.push(("../escape".into(), vec![], 0o644));
        assert!(build(&palera1n, &bad, &Identity::random()).is_err());
    }
}

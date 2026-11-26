//! The Dopamine access route, without DFU: one app, installed with the
//! person's Apple ID. The app is the pinned official Dopamine release with
//! Resize APFS in it; opened on the device it shows only Resize APFS, gets
//! storage access through Dopamine by itself and starts its helper. Nothing
//! is copied to the device afterwards.
//!
//! Nothing of Dopamine is rebuilt or removed. Its executable gets one more
//! library to load (the Resize APFS interface), its Info.plist loses the
//! scene configuration and takes Resize APFS's name, icon and fonts, and
//! the Resize APFS resources are added. This is resize-apfs's
//! `scripts/embed.py`; the signing is its `scripts/xcode_signing.py`: Xcode
//! makes the provisioning profile while it builds a tiny stub app for the
//! connected device, and the stub's profile, entitlements and certificate
//! then sign the app under `org.orchardlinux.resize-apfs.<team>`.
//!
//! Where no Xcode is signed in (Linux, and a Mac without it), xtool does
//! the Apple side, as resize-apfs's `scripts/sideload.py` has it: it signs
//! in to the Apple ID once in the terminal, registers the device, makes
//! the App ID and profile, signs and installs. It is pinned by size and
//! SHA-256 like every outside tool.
//!
//! Signing happens on the person's computer, with their account, for their
//! device.
use crate::exec::Executor;
use crate::resize_app::Access;
use crate::usbmux;
use orchard_boards::data::archive::Archive;
use orchard_boards::release::Receipt;
use orchard_cache::{Code, Error, Result};
use plist::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Dopamine 3.0.13, the official release.
const DOPAMINE: &str = "https://github.com/opa334/Dopamine/releases/download/3.0.13/Dopamine.ipa";
const DOPAMINE_BYTES: u64 = 55_422_988;
const DOPAMINE_SHA256: &str = "3664edd737cc672af5d9036d2d1513fb1d50784f0c0cfd49cc6e651b8c665e14";
const LIBRARY: &str = "@executable_path/Frameworks/ResizeAccess.dylib";
/// What the app's interface calls in Dopamine. A Dopamine release without
/// one of these names is refused here instead of failing on the device.
const INTERFACE: &[&str] = &[
    "DOJailbreaker",
    "contiguousMappingWorkaroundNeeded",
    "applyContiguousMappingWorkaround",
    "runWithError:didRemoveJailbreak:showLogs:",
    "finalize",
    "DOEnvironmentManager",
    "sharedManager",
    "isJailbroken",
    "runAsRoot:",
    "runUnsandboxed:",
    "DOPreferenceManager",
    "setPreferenceValue:forKey:",
    "bootlogoEnabled",
];
/// Taken from the Resize APFS Info.plist; a key it lacks is removed.
const INFO_KEYS: &[&str] = &[
    "CFBundleDisplayName",
    "CFBundleName",
    "CFBundleShortVersionString",
    "CFBundleVersion",
    "CFBundleIcons",
    "CFBundleIcons~ipad",
    "CFBundleIconFiles",
    "UILaunchStoryboardName",
    "UIAppFonts",
    "UIStatusBarStyle",
    "UISupportedInterfaceOrientations",
    "UISupportedInterfaceOrientations~ipad",
    "UISupportedInterfaceOrientations~iphone",
    "NSPhotoLibraryUsageDescription",
];
const STUB: &str = "DopamineSigning";
const MEMBER: u64 = 256 << 20;

fn fail(message: impl Into<String>) -> Error {
    Error::new(Code::Refused, message.into())
}

fn format(message: impl Into<String>) -> Error {
    Error::new(Code::Format, message.into())
}

fn le32(data: &[u8], at: usize) -> Result<u32> {
    data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes"))).ok_or_else(|| format("the Dopamine executable is cut short"))
}

fn is_macho(data: &[u8]) -> bool {
    matches!(data.get(..4), Some([0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe] | [0xca, 0xfe, 0xba, 0xbf]))
}

/// A 64-bit Mach-O with `path` added to the libraries it loads, in the
/// unused space between its load commands and its first section. Its size
/// and offsets stay the same.
fn add_to_slice(data: &[u8], path: &str) -> Result<Vec<u8>> {
    const HEADER: usize = 32;
    if le32(data, 0)? != 0xfeed_facf {
        return Err(format("the Dopamine executable is not a 64-bit program"));
    }
    let (count, size) = (le32(data, 16)? as usize, le32(data, 20)? as usize);
    let end = HEADER + size;
    let mut first_section = data.len();
    let mut at = HEADER;
    for _ in 0..count {
        let (command, length) = (le32(data, at)?, le32(data, at + 4)? as usize);
        if length < 8 || at + length > end {
            return Err(format("the Dopamine executable has damaged load commands"));
        }
        // LC_LOAD_DYLIB: the name starts at the offset its third word gives.
        if command == 0xc {
            let name = &data[at + le32(data, at + 8)? as usize..at + length];
            if name.split(|b| *b == 0).next() == Some(path.as_bytes()) {
                return Ok(data.to_vec());
            }
        }
        // LC_SEGMENT_64: its sections say where the file's contents begin.
        if command == 0x19 {
            for index in 0..le32(data, at + 64)? as usize {
                let offset = le32(data, at + 72 + index * 80 + 48)? as usize;
                if offset != 0 {
                    first_section = first_section.min(offset);
                }
            }
        }
        at += length;
    }
    let mut name = path.as_bytes().to_vec();
    name.push(0);
    name.resize(name.len() + (8 - (24 + name.len()) % 8) % 8, 0);
    let mut added = Vec::new();
    // name offset, timestamp, current version 1.0.0, compatibility version 1.0.0
    for word in [0xc, 24 + name.len() as u32, 24, 2, 0x10000, 0x10000] {
        added.extend(word.to_le_bytes());
    }
    added.extend(name);
    if end + added.len() > first_section || data[end..end + added.len()].iter().any(|b| *b != 0) {
        return Err(format("the Dopamine executable has no room for another library"));
    }
    let mut out = data.to_vec();
    out[16..20].copy_from_slice(&(count as u32 + 1).to_le_bytes());
    out[20..24].copy_from_slice(&((size + added.len()) as u32).to_le_bytes());
    out[end..end + added.len()].copy_from_slice(&added);
    Ok(out)
}

/// `data`, a Mach-O executable of one or several architectures, loading
/// the library too.
fn add_library(data: &[u8]) -> Result<Vec<u8>> {
    if data.get(..4) != Some(&[0xca, 0xfe, 0xba, 0xbe]) {
        return add_to_slice(data, LIBRARY);
    }
    let be32 = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().expect("4 bytes")) as usize).ok_or_else(|| format("the Dopamine executable is cut short"));
    let mut out = data.to_vec();
    for index in 0..be32(4)? {
        let (offset, size) = (be32(8 + index * 20 + 8)?, be32(8 + index * 20 + 12)?);
        let slice = data.get(offset..offset + size).ok_or_else(|| format("the Dopamine executable is cut short"))?;
        out[offset..offset + size].copy_from_slice(&add_to_slice(slice, LIBRARY)?);
    }
    Ok(out)
}

fn dictionary(bytes: &[u8], what: &str) -> Result<plist::Dictionary> {
    Value::from_reader(std::io::Cursor::new(bytes)).ok().and_then(Value::into_dictionary).ok_or_else(|| format(format!("{what} is not a property list")))
}

fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)?;
    #[cfg(unix)]
    if is_macho(data) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// The app unpacked and ready to be signed.
pub struct Prepared {
    /// The folder the IPA was unpacked into: it holds `Payload/NAME.app`.
    pub root: PathBuf,
    pub app: PathBuf,
    /// The Unix modes Dopamine's IPA records, by member name.
    modes: std::collections::BTreeMap<String, u32>,
}

/// The pinned Dopamine app with Resize APFS in it, unpacked under `work`,
/// ready to be signed.
pub fn prepare(ipa: Vec<u8>, access: &Access, work: &Path) -> Result<Prepared> {
    let mut archive = Archive::open(ipa)?;
    let names: Vec<String> = archive.entries().map(|e| e.name.clone()).collect();
    let modes = archive.entries().filter(|e| e.mode != 0).map(|e| (e.name.clone(), e.mode & 0o7777)).collect();
    for name in &names {
        if name.starts_with('/') || name.split('/').any(|part| part == "..") {
            return Err(format("an unsafe path in the Dopamine IPA"));
        }
        if !name.ends_with('/') {
            write(&work.join(name), &archive.read(name, MEMBER)?)?;
        }
    }
    let mut apps = std::fs::read_dir(work.join("Payload"))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "app"));
    let (Some(app), None) = (apps.next(), apps.next()) else {
        return Err(format("the Dopamine IPA does not hold exactly one app"));
    };
    let mut info = dictionary(&std::fs::read(app.join("Info.plist"))?, "Dopamine's Info.plist")?;
    let ours = dictionary(&access.info, "the app's Info.plist")?;
    let executable = app.join(info.get("CFBundleExecutable").and_then(Value::as_string).ok_or_else(|| format("Dopamine names no executable"))?);
    let program = std::fs::read(&executable)?;
    let has = |name: &str| program.windows(name.len() + 1).any(|w| &w[..name.len()] == name.as_bytes() && w[name.len()] == 0);
    let missing: Vec<&str> = INTERFACE.iter().copied().filter(|name| !has(name)).collect();
    if !missing.is_empty() {
        return Err(format(format!("this Dopamine release does not fit this Resize APFS: it has no {}", missing.join(", "))));
    }
    write(&executable, &add_library(&program)?)?;
    write(&app.join("Frameworks/ResizeAccess.dylib"), &access.library)?;
    for (path, data) in &access.resources {
        write(&app.join(path), data)?;
    }
    // Dopamine keeps its launch screen in a language folder, which iOS
    // would prefer to ours.
    if let Some(launch) = ours.get("UILaunchStoryboardName").and_then(Value::as_string).map(|n| format!("{n}.storyboardc")) {
        for folder in std::fs::read_dir(&app)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "lproj")) {
            let theirs = folder.join(&launch);
            if theirs.is_dir() {
                std::fs::remove_dir_all(&theirs)?;
                for (path, data) in access.resources.iter().filter(|(p, _)| p.starts_with(&format!("{launch}/"))) {
                    write(&folder.join(path), data)?;
                }
            }
        }
    }
    // Without its scene configuration Dopamine's own interface never appears.
    info.remove("UIApplicationSceneManifest");
    for key in INFO_KEYS {
        match ours.get(key) {
            Some(value) => {
                info.insert((*key).to_owned(), value.clone());
            }
            None => {
                info.remove(key);
            }
        }
    }
    // The icon comes from the loose files: by name it would be the one in
    // Dopamine's image catalog.
    for key in ["CFBundleIcons", "CFBundleIcons~ipad"] {
        if let Some(primary) = info.get_mut(key).and_then(Value::as_dictionary_mut).and_then(|d| d.get_mut("CFBundlePrimaryIcon")).and_then(Value::as_dictionary_mut) {
            primary.remove("CFBundleIconName");
        }
    }
    let mut bytes = Vec::new();
    Value::Dictionary(info).to_writer_xml(&mut bytes).map_err(|e| format(e.to_string()))?;
    std::fs::write(app.join("Info.plist"), bytes)?;
    Ok(Prepared { root: work.to_owned(), app, modes })
}

impl Prepared {
    /// The app as an IPA, for a tool that signs and installs one. Members
    /// keep the modes Dopamine's IPA gave them: executables must stay
    /// executable after signing.
    pub fn ipa(&self) -> Result<Vec<u8>> {
        let mut paths = Vec::new();
        walk(&self.root, &mut paths);
        paths.sort();
        let mut members = Vec::new();
        for path in paths {
            let Ok(relative) = path.strip_prefix(&self.root) else { continue };
            let mut name = relative.to_string_lossy().replace('\\', "/");
            let (data, usual) = match path.is_dir() {
                true => {
                    name.push('/');
                    (Vec::new(), 0o755)
                }
                false => {
                    let data = std::fs::read(&path)?;
                    let usual = if is_macho(&data) { 0o755 } else { 0o644 };
                    (data, usual)
                }
            };
            let mode = self.modes.get(&name).copied().unwrap_or(usual);
            members.push((name, data, mode));
        }
        orchard_formats::zip::write(&members).map_err(|e| format(e.to_string()))
    }
}

/// What a command printed and how it ended; None: not in time.
struct Ran {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

/// Runs a command with its output in files, so a long one cannot block it.
fn run(command: &mut Command, work: &Path, most: Duration) -> Result<Ran> {
    let (out, err) = (work.join("stdout"), work.join("stderr"));
    let mut child = command.stdin(Stdio::null()).stdout(std::fs::File::create(&out)?).stderr(std::fs::File::create(&err)?).spawn()?;
    let until = Instant::now() + most;
    let code = loop {
        match child.try_wait()? {
            Some(status) => break status.code(),
            None if Instant::now() > until => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    Ok(Ran { code, stdout: std::fs::read(&out)?, stderr: String::from_utf8_lossy(&std::fs::read(&err)?).into_owned() })
}

/// The teams Xcode is signed in to: id and name.
pub fn teams(work: &Path) -> Result<Vec<(String, String)>> {
    let ran = run(Command::new("defaults").args(["export", "com.apple.dt.Xcode", "-"]), work, Duration::from_secs(30))?;
    let Ok(settings) = dictionary(&ran.stdout, "Xcode's settings") else { return Ok(Vec::new()) };
    let mut found = std::collections::BTreeMap::new();
    for account in settings.get("IDEProvisioningTeamByIdentifier").and_then(Value::as_dictionary).into_iter().flat_map(|d| d.values()) {
        for team in account.as_array().into_iter().flatten().filter_map(Value::as_dictionary) {
            if let Some(id) = team.get("teamID").and_then(Value::as_string) {
                found.insert(id.to_owned(), team.get("teamName").and_then(Value::as_string).unwrap_or(id).to_owned());
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// The app's identifier for a team: one App ID per team, so signing again
/// every 7 days (a free Apple ID) reuses it.
pub fn identifier(team: &str) -> String {
    format!("org.orchardlinux.resize-apfs.{}", team.to_lowercase())
}

const PROJECT: &str = r#"// !$*UTF8*$!
{
	archiveVersion = 1;
	classes = {};
	objectVersion = 56;
	objects = {
		A1000001 = {isa = PBXBuildFile; fileRef = A1000002; };
		A1000002 = {isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = main.swift; sourceTree = "<group>"; };
		A1000003 = {isa = PBXFileReference; explicitFileType = wrapper.application; includeInIndex = 0; path = @STUB@.app; sourceTree = BUILT_PRODUCTS_DIR; };
		A1000004 = {isa = PBXGroup; children = (A1000002, A1000005); sourceTree = "<group>"; };
		A1000005 = {isa = PBXGroup; children = (A1000003); name = Products; sourceTree = "<group>"; };
		A1000006 = {isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (A1000001); runOnlyForDeploymentPostprocessing = 0; };
		A1000007 = {isa = PBXNativeTarget; buildConfigurationList = A1000009; buildPhases = (A1000006); buildRules = (); dependencies = (); name = @STUB@; productName = @STUB@; productReference = A1000003; productType = "com.apple.product-type.application"; };
		A1000008 = {isa = PBXProject; buildConfigurationList = A1000011; compatibilityVersion = "Xcode 14.0"; mainGroup = A1000004; productRefGroup = A1000005; projectDirPath = ""; projectRoot = ""; targets = (A1000007); };
		A1000009 = {isa = XCConfigurationList; buildConfigurations = (A1000010); defaultConfigurationName = Debug; };
		A1000010 = {isa = XCBuildConfiguration; name = Debug; buildSettings = {
			ALWAYS_SEARCH_USER_PATHS = NO;
			CODE_SIGN_STYLE = Automatic;
			CURRENT_PROJECT_VERSION = 1;
			DEVELOPMENT_TEAM = @TEAM@;
			GENERATE_INFOPLIST_FILE = YES;
			INFOPLIST_KEY_UILaunchScreen_Generation = YES;
			IPHONEOS_DEPLOYMENT_TARGET = 15.0;
			MARKETING_VERSION = 1.0;
			PRODUCT_BUNDLE_IDENTIFIER = "@BUNDLE@";
			PRODUCT_NAME = @STUB@;
			SDKROOT = iphoneos;
			SWIFT_VERSION = 5.0;
			TARGETED_DEVICE_FAMILY = "1,2";
		}; };
		A1000011 = {isa = XCConfigurationList; buildConfigurations = (A1000012); defaultConfigurationName = Debug; };
		A1000012 = {isa = XCBuildConfiguration; name = Debug; buildSettings = {SDKROOT = iphoneos; }; };
	};
	rootObject = A1000008;
}
"#;

const SCHEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion="1500" version="1.7">
   <BuildAction parallelizeBuildables="YES" buildImplicitDependencies="YES">
      <BuildActionEntries>
         <BuildActionEntry buildForRunning="YES">
            <BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="A1000007"
               BuildableName="@STUB@.app" BlueprintName="@STUB@" ReferencedContainer="container:@STUB@.xcodeproj"/>
         </BuildActionEntry>
      </BuildActionEntries>
   </BuildAction>
</Scheme>
"#;

/// The profile, entitlements and signing identity Xcode gives a stub app
/// it builds for `team` and the device: (profile, entitlements, SHA-1).
fn provision(work: &Path, team: &str, udid: &str) -> Result<(PathBuf, Vec<u8>, String)> {
    let stub = work.join("stub");
    let project = stub.join(format!("{STUB}.xcodeproj"));
    std::fs::create_dir_all(project.join("xcshareddata/xcschemes"))?;
    std::fs::write(stub.join("main.swift"), "import UIKit\nUIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, nil)\n")?;
    std::fs::write(project.join("project.pbxproj"), PROJECT.replace("@STUB@", STUB).replace("@TEAM@", team).replace("@BUNDLE@", &identifier(team)))?;
    std::fs::write(project.join(format!("xcshareddata/xcschemes/{STUB}.xcscheme")), SCHEME.replace("@STUB@", STUB))?;
    let derived = work.join("derived");
    let built = run(
        Command::new("xcodebuild")
            .arg("-project")
            .arg(&project)
            .args(["-scheme", STUB, "-configuration", "Debug", "-destination", &format!("id={udid}"), "-derivedDataPath"])
            .arg(&derived)
            .args(["-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration", "build"]),
        work,
        Duration::from_secs(900),
    )?;
    if built.code != Some(0) {
        let text = String::from_utf8_lossy(&built.stdout);
        let last: Vec<&str> = text.trim().lines().rev().take(8).collect();
        return Err(fail(format!("Xcode could not make a signing profile for the device:\n{}", last.into_iter().rev().collect::<Vec<_>>().join("\n"))));
    }
    let app = derived.join(format!("Build/Products/Debug-iphoneos/{STUB}.app"));
    let profile = app.join("embedded.mobileprovision");
    if !profile.is_file() {
        return Err(fail("Xcode built the signing app without a provisioning profile"));
    }
    let entitlements = run(Command::new("codesign").args(["-d", "--entitlements", ":-"]).arg(&app), work, Duration::from_secs(60))?.stdout;
    if !entitlements.trim_ascii_start().starts_with(b"<?xml") {
        return Err(fail("could not read the signing entitlements"));
    }
    let details = run(Command::new("codesign").arg("-dvv").arg(&app), work, Duration::from_secs(60))?.stderr;
    let authority = details.lines().find_map(|line| line.strip_prefix("Authority=")).ok_or_else(|| fail("could not find which certificate Xcode signed with"))?.to_owned();
    // The one valid identity of that name: an expired one may share it.
    let listing = run(Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]), work, Duration::from_secs(60))?.stdout;
    let listing = String::from_utf8_lossy(&listing).into_owned();
    let hashes: Vec<&str> = listing
        .lines()
        .filter_map(|line| {
            let rest = line.trim().split_once(") ")?.1;
            let (hash, name) = rest.split_once(' ')?;
            (hash.len() == 40 && name.trim_matches('"') == authority).then_some(hash)
        })
        .collect();
    match hashes.as_slice() {
        [one] => Ok((profile, entitlements, (*one).to_owned())),
        other => Err(fail(format!("expected one valid signing certificate named \"{authority}\", found {}", other.len()))),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        out.push(path.clone());
        if path.is_dir() && !path.is_symlink() {
            walk(&path, out);
        }
    }
}

/// Signs `app` under `identifier` with the profile and identity:
/// everything inside it first, innermost first, then the app itself.
fn sign(app: &Path, work: &Path, identifier: &str, profile: &Path, entitlements: &[u8], sha1: &str) -> Result<()> {
    let mut info = dictionary(&std::fs::read(app.join("Info.plist"))?, "the app's Info.plist")?;
    let executable = app.join(info.get("CFBundleExecutable").and_then(Value::as_string).unwrap_or_default());
    info.insert("CFBundleIdentifier".into(), identifier.into());
    let mut bytes = Vec::new();
    Value::Dictionary(info).to_writer_xml(&mut bytes).map_err(|e| format(e.to_string()))?;
    std::fs::write(app.join("Info.plist"), bytes)?;
    std::fs::copy(profile, app.join("embedded.mobileprovision"))?;
    let rules = work.join("entitlements.plist");
    std::fs::write(&rules, entitlements)?;
    let mut all = Vec::new();
    walk(app, &mut all);
    let bundle = |p: &Path| p.is_dir() && p.extension().is_some_and(|e| e == "framework" || e == "appex" || e == "bundle");
    let bundles: Vec<PathBuf> = all.iter().filter(|p| bundle(p)).cloned().collect();
    let loose = all.iter().filter(|p| {
        p.is_file() && !p.is_symlink() && **p != executable && !p.parent().is_some_and(|d| bundles.iter().any(|b| b == d)) && std::fs::read(p).is_ok_and(|d| is_macho(&d))
    });
    let depth = |p: &PathBuf| std::cmp::Reverse(p.components().count());
    let mut order: Vec<PathBuf> = loose.cloned().collect();
    order.sort_by_key(depth);
    let mut outer = bundles.clone();
    outer.sort_by_key(depth);
    order.extend(outer);
    let codesign = || {
        let mut command = Command::new("codesign");
        command.args(["--force", "--sign", sha1, "--timestamp=none", "--generate-entitlement-der"]);
        command
    };
    for path in &order {
        let ran = run(codesign().arg(path), work, Duration::from_secs(300))?;
        if ran.code != Some(0) {
            return Err(fail(format!("signing {} failed: {}", path.file_name().unwrap_or_default().to_string_lossy(), ran.stderr.trim())));
        }
    }
    let ran = run(codesign().arg("--entitlements").arg(&rules).arg(app), work, Duration::from_secs(300))?;
    match ran.code {
        Some(0) => Ok(()),
        _ => Err(fail(format!("signing Resize APFS failed: {}", ran.stderr.trim()))),
    }
}

/// The one device running iOS on USB, by its UDID.
pub fn device() -> Result<String> {
    let mut usb: Vec<_> = usbmux::devices(Path::new(usbmux::SOCKET))?.into_iter().filter(|d| d.usb).collect();
    match usb.len() {
        0 => Err(Error::new(Code::NoDevice, "no device running iOS is connected over USB").hint("connect the device, unlock it and trust this computer")),
        1 => Ok(usb.remove(0).udid),
        _ => Err(Error::new(Code::Ambiguous, "more than one device running iOS is connected").hint("connect only the device to resize")),
    }
}

/// xtool 2.0.0-beta.2, which signs with an Apple ID and installs: the
/// sizes and checksums resize-apfs's `scripts/tools.py` pins. The beta has
/// the fixes that signing again every 7 days runs into.
const XTOOL: &str = "https://github.com/xtool-org/xtool/releases/download/2.0.0-beta.2";

struct Xtool {
    asset: &'static str,
    bytes: u64,
    sha256: &'static str,
    /// The executable inside a ZIP that holds the whole macOS app, with its
    /// own checksum; None: the file is the executable (an AppImage).
    member: Option<(&'static str, &'static str)>,
}

fn xtool_pin() -> Option<Xtool> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Some(Xtool { asset: "xtool-aarch64.AppImage", bytes: 65_292_808, sha256: "954da46b983f87596bf8500365b2b5458b27d5394f95c6ec579423bb690ec910", member: None }),
        ("linux", "x86_64") => Some(Xtool { asset: "xtool-x86_64.AppImage", bytes: 67_795_448, sha256: "512fce299d48b0c59e7bd32787a07ecd018d61a529ed17757b9019d709a667fd", member: None }),
        ("macos", _) => Some(Xtool {
            asset: "xtool.app.zip",
            bytes: 46_999_993,
            sha256: "93e780e4b4f96ba6d48b365bdfe9ae501f111f5dbb751a0d34b9e1317f5428db",
            member: Some(("xtool.app/Contents/MacOS/xtool", "ebc400c1479accdd33da9f1905e11d0caaf400d0c087160ce916e4f243b06020")),
        }),
        _ => None,
    }
}

/// Whether xtool has a build for this computer.
pub fn xtool_here() -> bool {
    xtool_pin().is_some()
}

/// The pinned xtool, fetched and unpacked in the cache, ready to run.
fn xtool(executor: &Executor) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let pin = xtool_pin().ok_or_else(|| Error::new(Code::Unsupported, "xtool publishes no build for this computer"))?;
    let receipt = Receipt { url: format!("{XTOOL}/{}", pin.asset), bytes: pin.bytes, sha256: pin.sha256.into(), expanded_bytes: None };
    let package = executor.download(&receipt)?;
    let home = executor.cache().join("tools").join(format!("xtool-{}", &pin.sha256[..16]));
    let Some((member, checksum)) = pin.member else {
        // An AppImage: the file is the executable.
        let path = home.with_extension("AppImage");
        if !path.is_file() {
            std::fs::create_dir_all(executor.cache().join("tools"))?;
            std::fs::copy(&package, &path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        return Ok(path);
    };
    let executable = home.join(member);
    if !executable.is_file() {
        // The whole app: its executable loads what is beside it.
        let partial = home.with_extension("partial");
        let _ = std::fs::remove_dir_all(&partial);
        let mut archive = Archive::open(std::fs::read(&package)?)?;
        let entries: Vec<(String, u32)> = archive.entries().map(|e| (e.name.clone(), e.mode)).collect();
        for (name, mode) in entries {
            if name.starts_with('/') || name.split('/').any(|part| part == "..") {
                return Err(format("an unsafe path in xtool's package"));
            }
            if name.ends_with('/') {
                continue;
            }
            let path = partial.join(&name);
            write(&path, &archive.read(&name, MEMBER)?)?;
            if mode & 0o111 != 0 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        use sha2::Digest;
        let found: String = sha2::Sha256::digest(std::fs::read(partial.join(member))?).iter().map(|b| std::format!("{b:02x}")).collect();
        if found != checksum {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(format("xtool's executable is not the pinned one"));
        }
        let _ = std::fs::remove_dir_all(&home);
        std::fs::rename(&partial, &home)?;
    }
    Ok(executable)
}

/// xtool's command line: the macOS app starts its window without XTL_CLI;
/// the AppImage runs without FUSE when it unpacks itself.
fn xtool_command(xtool: &Path) -> Command {
    let mut command = Command::new(xtool);
    command.env("XTL_CLI", "1").env("APPIMAGE_EXTRACT_AND_RUN", "1");
    command
}

/// Signs and installs with xtool, in this terminal: it may ask for the
/// Apple ID, its password and the two-factor code (which go to Apple only),
/// to trust the device, or to replace a certificate.
fn with_xtool(executor: &Executor, prepared: &Prepared, work: &Path, udid: &str, say: &dyn Fn(&str)) -> Result<()> {
    let xtool = xtool(executor)?;
    let ipa = work.join("ResizeAPFS.ipa");
    std::fs::write(&ipa, prepared.ipa()?)?;
    let status = run(xtool_command(&xtool).args(["auth", "status"]), work, Duration::from_secs(120))?;
    if !(status.code == Some(0) && status.stdout.starts_with(b"Logged in")) {
        say("Sign in with your Apple ID. xtool keeps the session for later runs.");
        if !xtool_command(&xtool).args(["auth", "login", "--mode", "password"]).status()?.success() {
            return Err(fail("signing in to your Apple ID failed"));
        }
    }
    say("Signing and installing with xtool");
    let installed = xtool_command(&xtool).args(["install", "--usb", "--udid", udid]).arg(&ipa).status()?;
    match installed.success() {
        true => Ok(()),
        false => Err(fail(format!("signing or installing Resize APFS failed (xtool: {installed})"))),
    }
}

/// Who signs the app.
pub enum Signer {
    /// The Apple ID Xcode is signed in to, by its team (one of [`teams`]).
    Xcode { team: String },
    /// xtool, with an Apple ID it signs in to itself.
    Xtool,
}

/// Puts Resize APFS into the pinned Dopamine, signs it and installs it on
/// the device `udid`. `say` is told each step.
pub fn install(executor: &Executor, access: &Access, signer: &Signer, udid: &str, say: &dyn Fn(&str)) -> Result<()> {
    say("Fetching Dopamine");
    let receipt = Receipt { url: DOPAMINE.into(), bytes: DOPAMINE_BYTES, sha256: DOPAMINE_SHA256.into(), expanded_bytes: None };
    let ipa = std::fs::read(executor.download(&receipt)?)?;
    let work = executor.cache().join("work").join(format!("resize-sign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let done = (|| {
        say("Putting Resize APFS into it");
        let prepared = prepare(ipa, access, &work.join("app"))?;
        let team = match signer {
            Signer::Xtool => return with_xtool(executor, &prepared, &work, udid, say),
            Signer::Xcode { team } => team,
        };
        let app = &prepared.app;
        say("Making a signing profile with Xcode");
        let (profile, entitlements, sha1) = provision(&work, team, udid)?;
        say("Signing");
        sign(app, &work, &identifier(team), &profile, &entitlements, &sha1)?;
        say("Installing on the device");
        let ran = run(Command::new("xcrun").args(["devicectl", "device", "install", "app", "--device", udid]).arg(app), &work, Duration::from_secs(600))?;
        match ran.code {
            Some(0) => Ok(()),
            _ => {
                let text = std::format!("{}{}", ran.stderr, String::from_utf8_lossy(&ran.stdout));
                Err(fail(std::format!("installing Resize APFS failed: {}", text.trim().chars().rev().take(800).collect::<String>().chars().rev().collect::<String>())))
            }
        }
    })();
    let _ = std::fs::remove_dir_all(&work);
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64-bit Mach-O with one segment whose one section starts at
    /// `first`, and room after its load commands.
    fn program(first: u32) -> Vec<u8> {
        let mut m = vec![0u8; first as usize + 16];
        m[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
        m[16..20].copy_from_slice(&1u32.to_le_bytes());
        m[20..24].copy_from_slice(&152u32.to_le_bytes());
        m[32..36].copy_from_slice(&0x19u32.to_le_bytes());
        m[36..40].copy_from_slice(&152u32.to_le_bytes());
        m[32 + 64..32 + 68].copy_from_slice(&1u32.to_le_bytes());
        m[32 + 72 + 48..32 + 72 + 52].copy_from_slice(&first.to_le_bytes());
        m
    }

    #[test]
    fn a_library_is_added_in_the_room_after_the_load_commands() {
        let before = program(4096);
        let after = add_to_slice(&before, LIBRARY).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(le32(&after, 16).unwrap(), 2);
        let added = (le32(&after, 20).unwrap() - 152) as usize;
        assert_eq!(added % 8, 0);
        assert_eq!(le32(&after, 184).unwrap(), 0xc);
        assert_eq!(&after[184 + 24..184 + 24 + LIBRARY.len()], LIBRARY.as_bytes());
        // Added once: a second time changes nothing.
        assert_eq!(add_to_slice(&after, LIBRARY).unwrap(), after);
        // No room, and not a 64-bit program.
        assert!(add_to_slice(&program(200), LIBRARY).is_err());
        assert!(add_to_slice(&[0u8; 64], LIBRARY).is_err());
    }

    #[test]
    fn each_architecture_of_a_fat_program_gets_it() {
        let slice = program(4096);
        let mut fat = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 1];
        fat.extend([0u8; 8]);
        fat.extend(64u32.to_be_bytes());
        fat.extend((slice.len() as u32).to_be_bytes());
        fat.resize(64, 0);
        fat.extend(&slice);
        let after = add_library(&fat).unwrap();
        assert_eq!(le32(&after[64..], 16).unwrap(), 2);
        assert_eq!(&after[..64], &fat[..64]);
    }

    #[test]
    fn the_identifier_is_one_per_team() {
        assert_eq!(identifier("ABCDE12345"), "org.orchardlinux.resize-apfs.abcde12345");
    }
}

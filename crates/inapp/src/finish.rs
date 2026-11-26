//! Everything after the root's image is on its partition: firmware, fstab,
//! the account, the time zone, the kernel and the boot entry. What orchard's
//! install system does inside the new root (`install_device.py finish`, with
//! `boot-entries`), done here from outside it: as changes to the unmounted
//! ext4 filesystem and as the files of a new boot filesystem.
use crate::crypt;
use crate::ext4::{Change, Ext4, Item};
use crate::flow::Account;
use orchard_cache::{Code, Error, Result};
use orchard_formats::tar;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

const PIN_CHECK: &str = r#"#!/bin/sh
# Lock-screen PIN check for the orchard-pin PAM service (pam_exec expose_authtok).
# The PIN arrives on stdin, NUL-terminated; /etc/orchard/pin holds user:sha512-crypt.
PATH=/usr/bin:/bin
pin=$(tr -d '\000')
case $pin in [0-9][0-9][0-9][0-9][0-9][0-9]) ;; *) exit 1 ;; esac
while IFS=: read -r user hash; do
    [ "$user" = "$PAM_USER" ] || continue
    salt=${hash#\$6\$}
    salt=${salt%%\$*}
    [ "$(printf '%s' "$pin" | openssl passwd -6 -salt "$salt" -stdin)" = "$hash" ]
    exit
done < /etc/orchard/pin
exit 1
"#;

const PIN_PAM: &str = "#%PAM-1.0
# Orchard PIN: 6 digits, 5 tries, then locked for 5 minutes. It signs in at
# boot and unlocks the screen only; sudo and SSH never accept it. The lock
# runs this as the user, so the tally lives in a directory the user owns.
auth     requisite                pam_nologin.so
auth     required                 pam_faillock.so preauth dir=/var/lib/orchard/pin-tally deny=5 unlock_time=300
auth     [success=1 default=bad]  pam_exec.so quiet expose_authtok /usr/lib/orchard/pin-check
auth     [default=die]            pam_faillock.so authfail dir=/var/lib/orchard/pin-tally deny=5 unlock_time=300
auth     sufficient               pam_faillock.so authsucc dir=/var/lib/orchard/pin-tally deny=5 unlock_time=300
auth     required                 pam_deny.so
account  include                  system-local-login
session  include                  system-local-login
";

/// What the install knows about the system and its place on the disk.
pub struct System<'a> {
    /// The release's boot entry (`install_plan::entry`).
    pub entry: &'a Value,
    pub root_partuuid: &'a str,
    pub boot_partuuid: &'a str,
    /// `[install.account]` of the release.
    pub account: Option<&'a Value>,
    pub secrets: &'a Account,
    pub timezone: Option<&'a str>,
    /// Days since 1970, for the password's last change.
    pub today: u64,
    /// Seconds since 1970, for the package database.
    pub now: u64,
}

/// The kernel as it goes in: the root's package, or the platform layer's files.
pub enum Kernel<'a> {
    /// `linux-orchard-PLATFORM.pkg.tar.zst`, which wraps the layer.
    Package(&'a [u8]),
    /// `platform-*.tar.gz`.
    Layer(&'a [u8]),
}

/// A file of the kernel: its path below the root, mode and content.
struct KernelFile {
    path: String,
    mode: u32,
    /// The time the package states, which pacman checks files against.
    mtime: Option<u64>,
    data: Vec<u8>,
}

fn fail(what: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(Code::Format, format!("{what}: {detail}"))
}

fn text(data: Vec<u8>, what: &str) -> Result<String> {
    String::from_utf8(data).map_err(|_| fail(what, "not text"))
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(at) => path[..at].into(),
    }
}

/// The system's one regular user (UID 1000-59999), which must be `name`.
fn account_user(passwd: &str, name: &str) -> Result<(u32, u32, String)> {
    let users: Vec<(&str, u32, u32, &str)> = passwd
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            let (uid, gid) = (f.get(2)?.parse().ok()?, f.get(3)?.parse().ok()?);
            (f.len() == 7 && (1000..60000).contains(&uid)).then(|| (f[0], uid, gid, f[5]))
        })
        .collect();
    match users.as_slice() {
        [(user, uid, gid, home)] if *user == name => Ok((*uid, *gid, home.to_string())),
        _ => Err(fail("the account", format!("the system image needs exactly one regular user, {name}, found {:?}", users.iter().map(|u| u.0).collect::<Vec<_>>()))),
    }
}

/// `/etc/shadow` with the user's password set, as chpasswd leaves it.
fn shadow_with(shadow: &str, user: &str, hash: &str, today: u64) -> Result<String> {
    let mut found = false;
    let mut out = String::new();
    for line in shadow.lines() {
        let mut fields: Vec<String> = line.split(':').map(str::to_owned).collect();
        if fields[0] == user && fields.len() >= 3 {
            fields[1] = hash.to_owned();
            fields[2] = today.to_string();
            found = true;
        }
        out.push_str(&fields.join(":"));
        out.push('\n');
    }
    if !found {
        return Err(fail("the account", format!("/etc/shadow has no {user}")));
    }
    Ok(out)
}

/// The files of a platform layer (regular files under the paths a layer may
/// hold; scripts stay executable, nothing is setuid).
fn layer_files(layer: &[u8]) -> Result<Vec<KernelFile>> {
    let mut unpacked = Vec::new();
    flate2::read::GzDecoder::new(layer).take(512 << 20).read_to_end(&mut unpacked).map_err(|e| fail("the platform layer", e))?;
    let mut out = Vec::new();
    for file in tar::read(&unpacked)?.files {
        let path = file.path.trim_start_matches("./").to_owned();
        let allowed = ["usr/lib/", "usr/share/alsa/ucm2/", "usr/share/wireplumber/", "usr/share/licenses/"];
        if !allowed.iter().any(|prefix| path.starts_with(prefix)) {
            return Err(fail("the platform layer", "it may hold only files under /usr/lib, /usr/share/alsa/ucm2, /usr/share/wireplumber and /usr/share/licenses"));
        }
        out.push(KernelFile { path, mode: if file.mode & 0o111 != 0 { 0o755 } else { 0o644 }, mtime: None, data: file.data.to_vec() });
    }
    Ok(out)
}

/// A pacman package's files and its record in the root's package database
/// (`var/lib/pacman/local/NAME-VERSION/`), which is what `pacman -U` leaves
/// for a package without an install script.
fn package_files(package: &[u8], now: u64) -> Result<(Vec<KernelFile>, Vec<Change>)> {
    let mut unpacked = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(package)
        .map_err(|e| fail("the kernel package", e))?
        .take(512 << 20)
        .read_to_end(&mut unpacked)
        .map_err(|e| fail("the kernel package", e))?;
    let archive = tar::read(&unpacked)?;
    let meta = |name: &str| archive.files.iter().find(|f| f.path == name).map(|f| f.data);
    let info = std::str::from_utf8(meta(".PKGINFO").ok_or_else(|| fail("the kernel package", "no .PKGINFO"))?).map_err(|_| fail("the kernel package", ".PKGINFO is not text"))?;
    if meta(".INSTALL").is_some() {
        return Err(Error::new(Code::Unsupported, "the kernel package has an install script, which cannot run from here"));
    }
    let mut fields: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for line in info.lines().filter(|l| !l.starts_with('#')) {
        if let Some((key, value)) = line.split_once(" = ") {
            fields.entry(key.trim()).or_default().push(value.trim());
        }
    }
    let one = |key: &str| fields.get(key).and_then(|v| v.first().copied()).ok_or_else(|| fail("the kernel package", format!(".PKGINFO has no {key}")));
    let (name, version) = (one("pkgname")?, one("pkgver")?);
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._+-:".contains(c));
    if !plain(name) || !plain(version) {
        return Err(fail("the kernel package", "unusual name or version"));
    }
    let mut desc = String::new();
    let mut section = |title: &str, values: &[&str]| {
        if !values.is_empty() {
            desc.push_str(&format!("%{title}%\n{}\n\n", values.join("\n")));
        }
    };
    let installed = now.to_string();
    let many = |key: &str| fields.get(key).cloned().unwrap_or_default();
    section("NAME", &[name]);
    section("VERSION", &[version]);
    section("BASE", &many("pkgbase"));
    section("DESC", &many("pkgdesc"));
    section("URL", &many("url"));
    section("ARCH", &many("arch"));
    section("BUILDDATE", &many("builddate"));
    section("INSTALLDATE", &[&installed]);
    section("PACKAGER", &many("packager"));
    section("SIZE", &many("size"));
    section("LICENSE", &many("license"));
    section("VALIDATION", &["sha256"]);
    section("REPLACES", &many("replaces"));
    section("DEPENDS", &many("depend"));
    section("OPTDEPENDS", &many("optdepend"));
    section("CONFLICTS", &many("conflict"));
    section("PROVIDES", &many("provides"));
    section("XDATA", &many("xdata"));
    let mut files = Vec::new();
    let mut listed = BTreeSet::new();
    for file in &archive.files {
        if file.path.starts_with('.') {
            continue;
        }
        let mut at = 0;
        while let Some(slash) = file.path[at..].find('/') {
            at += slash + 1;
            listed.insert(file.path[..at].to_owned());
        }
        listed.insert(file.path.clone());
        files.push(KernelFile { path: file.path.clone(), mode: file.mode & 0o755, mtime: Some(file.mtime), data: file.data.to_vec() });
    }
    let mut list = String::from("%FILES%\n");
    for path in &listed {
        list.push_str(path);
        list.push('\n');
    }
    list.push('\n');
    let record = format!("/var/lib/pacman/local/{name}-{version}");
    let mut changes = vec![Change::file(format!("{record}/desc"), desc, 0o644), Change::file(format!("{record}/files"), list, 0o644)];
    if let Some(mtree) = meta(".MTREE") {
        changes.push(Change::file(format!("{record}/mtree"), mtree, 0o644));
    }
    Ok((files, changes))
}

/// The entry's token: the system's id and its root, so two installs never
/// share files.
fn token(system: &System) -> String {
    format!("{}-{}", system.entry["id"].as_str().unwrap_or("linux"), system.root_partuuid.split('-').next().unwrap_or(""))
}

fn entry_field<'a>(system: &'a System, key: &str) -> &'a str {
    system.entry[key].as_str().unwrap_or("")
}

fn options(system: &System) -> String {
    format!("root=PARTUUID={} rw {}", system.root_partuuid, entry_field(system, "options")).trim().to_owned()
}

/// What finishing leaves for the caller: the root is changed, the boot
/// filesystem's files are returned.
#[derive(Debug)]
pub struct Finished {
    /// (path on the boot filesystem, content).
    pub boot_files: Vec<(String, Vec<u8>)>,
    /// The boot entry's name, or `None` when the kernel has no device tree
    /// for this board.
    pub entry: Option<String>,
}

/// Finishes the root filesystem and says what goes on the boot filesystem.
/// `step` hears each part as it starts (the install system's step names).
pub fn finish(fs: &Ext4, system: &System, firmware: &[(String, Vec<u8>)], kernel: &Kernel, step: &dyn Fn(&str)) -> Result<Finished> {
    let mut changes: Vec<Change> = Vec::new();

    step("firmware");
    for (path, data) in firmware {
        // Arch's /lib is a link to usr/lib: firmware paths are mapped explicitly.
        let plain = path.starts_with("lib/firmware/") && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..");
        if !plain {
            return Err(fail("the firmware", format!("not a firmware path: {path}")));
        }
        changes.push(Change::file(format!("/usr/{path}"), data.clone(), 0o644));
    }
    changes.push(Change::file(
        "/etc/fstab",
        format!(
            "PARTUUID={} / ext4 rw,relatime 0 1\nPARTUUID={} /boot vfat rw,relatime,fmask=0077,dmask=0077 0 2\n",
            system.root_partuuid, system.boot_partuuid
        ),
        0o644,
    ));

    if let Some(account) = system.account {
        step("account");
        let flag = |key: &str| account[key].as_bool().unwrap_or(false);
        let name = account["user"].as_str().ok_or_else(|| fail("the account", "the release names no user"))?;
        let passwd = text(fs.read("/etc/passwd")?.ok_or_else(|| fail("the account", "the system image has no /etc/passwd"))?, "/etc/passwd")?;
        let (uid, gid, _home) = account_user(&passwd, name)?;
        if flag("password") {
            let shadow = text(fs.read("/etc/shadow")?.ok_or_else(|| fail("the account", "the system image has no /etc/shadow"))?, "/etc/shadow")?;
            let hash = crypt::sha512(system.secrets.password.as_bytes(), &crypt::salt());
            let listing = fs.list(&BTreeSet::from(["/etc".to_string()]))?;
            let kept = listing["/etc"].as_ref().and_then(|names| names.get("shadow")).cloned().unwrap_or(Item { mode: 0o100600, uid: 0, gid: 0 });
            changes.push(Change::File { path: "/etc/shadow".into(), data: shadow_with(&shadow, name, &hash, system.today)?.into_bytes(), mode: kept.mode & 0o7777, uid: kept.uid, gid: kept.gid, mtime: None });
        }
        if flag("passwordless_sudo") {
            changes.push(Change::file("/etc/sudoers.d/orchard", format!("{name} ALL=(ALL:ALL) NOPASSWD: ALL\n"), 0o440));
        }
        if flag("pin") {
            let hash = crypt::sha512(system.secrets.pin.as_bytes(), &crypt::salt());
            changes.push(Change::File { path: "/etc/orchard/pin".into(), data: format!("{name}:{hash}\n").into_bytes(), mode: 0o640, uid: 0, gid, mtime: None });
            changes.push(Change::Dir { path: "/var/lib/orchard/pin-tally".into(), mode: 0o700, uid, gid });
            changes.push(Change::File { path: format!("/var/lib/orchard/pin-tally/{name}"), data: Vec::new(), mode: 0o600, uid, gid, mtime: None });
            changes.push(Change::file("/usr/lib/orchard/pin-check", PIN_CHECK, 0o755));
            for service in ["greetd", "omarchy-lock-password"] {
                changes.push(Change::file(format!("/etc/pam.d/{service}"), PIN_PAM, 0o644));
            }
        }
    }

    if let Some(zone) = system.timezone.filter(|z| !z.is_empty() && z.split('/').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "_+-".contains(c)))) {
        let path = format!("/usr/share/zoneinfo/{zone}");
        let dir = dir_of(&path);
        let known = fs.list(&BTreeSet::from([dir.clone()]))?;
        if known[&dir].as_ref().is_some_and(|names| names.get(zone.rsplit('/').next().unwrap_or("")).is_some_and(Item::is_file)) {
            changes.push(Change::Link { path: "/etc/localtime".into(), target: format!("../usr/share/zoneinfo/{zone}") });
        }
    }

    // The configuration every later kernel update writes the entries from.
    let token = token(system);
    let devicetree = entry_field(system, "devicetree");
    changes.push(Change::file(
        "/etc/orchard/boot-entry",
        format!(
            "# This system's boot entry: read by /usr/lib/orchard/boot-entries (docs/boot-contract.md).\n\
             token={token}\ntitle={}\nsort_key={}\ndevicetree={devicetree}\noptions={}\nboot_partuuid={}\n",
            entry_field(system, "title"),
            entry_field(system, "id"),
            options(system),
            system.boot_partuuid
        ),
        0o644,
    ));

    step("kernel");
    let (files, record) = match kernel {
        Kernel::Package(package) => package_files(package, system.now)?,
        Kernel::Layer(layer) => (layer_files(layer)?, Vec::new()),
    };
    // The root image carries no kernel: a file both have means the image and
    // the layer disagree about who owns it.
    let dirs: BTreeSet<String> = files.iter().map(|f| dir_of(&format!("/{}", f.path))).collect();
    let there = fs.list(&dirs)?;
    for file in &files {
        let path = format!("/{}", file.path);
        let name = path.rsplit('/').next().unwrap_or("");
        if there[&dir_of(&path)].as_ref().is_some_and(|names| names.contains_key(name)) {
            return Err(fail("the kernel", format!("the root image already has {path}, which the platform layer provides")));
        }
    }
    let versions: BTreeSet<String> =
        files.iter().filter_map(|f| f.path.strip_prefix("usr/lib/modules/")?.strip_suffix("/vmlinuz")).filter(|v| !v.contains('/')).map(str::to_owned).collect();
    let [version] = &versions.iter().collect::<Vec<_>>()[..] else {
        return Err(fail("the kernel", format!("the system needs exactly one kernel at /usr/lib/modules/<version>/vmlinuz, found {versions:?}")));
    };
    let version = version.as_str();
    let of_kernel = |name: &str| files.iter().find(|f| f.path == format!("usr/lib/modules/{version}/{name}")).map(|f| f.data.clone());
    let linux = of_kernel("vmlinuz").expect("the version came from this file");
    let initrd = of_kernel("initrd").ok_or_else(|| fail("the kernel", "it has no initrd"))?;
    let tree = of_kernel(&format!("dtb/{devicetree}"));
    changes.extend(files.into_iter().map(|f| Change::File { path: format!("/{}", f.path), data: f.data, mode: f.mode, uid: 0, gid: 0, mtime: f.mtime }));
    changes.extend(record);
    let splash = fs.read("/usr/share/orchard/splash")?;
    fs.apply(&changes)?;

    step("entry");
    // boot-entries sync: /TOKEN/VERSION/{linux,initrd,DTB,splash} and the entry.
    let Some(tree) = tree else {
        return Ok(Finished { boot_files: Vec::new(), entry: None });
    };
    let base = format!("{token}/{version}");
    let architecture = if linux.get(0x24..0x28) == Some(&[0x18, 0x28, 0x6f, 0x01]) { "arm" } else { "aa64" };
    let name = format!("{token}-{version}");
    let mut boot_files = vec![(format!("{base}/linux"), linux), (format!("{base}/initrd"), initrd), (format!("{base}/{devicetree}"), tree)];
    if let Some(splash) = splash {
        boot_files.push((format!("{base}/splash"), splash));
    }
    boot_files.push((
        format!("loader/entries/{name}.conf"),
        format!(
            "title {}\nversion {version}\nsort-key {}\nlinux /{base}/linux\ninitrd /{base}/initrd\ndevicetree /{base}/{devicetree}\noptions {}\narchitecture {architecture}\n",
            entry_field(system, "title"),
            entry_field(system, "id"),
            options(system)
        )
        .into_bytes(),
    ));
    // The system just installed is what starts next.
    boot_files.push(("loader/loader.conf".into(), format!("default {token}-*\n").into_bytes()));
    Ok(Finished { boot_files, entry: Some(name) })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::ext4::tests::{host_tools, image};
    use orchard_boards::install_plan;
    use std::io::Write;

    pub fn layer() -> Vec<u8> {
        let v = "usr/lib/modules/7.3.0-test";
        let entries = [
            tar::Entry { path: &format!("{v}/vmlinuz"), mode: 0o644, data: b"KERNEL" },
            tar::Entry { path: &format!("{v}/initrd"), mode: 0o644, data: b"INITRD" },
            tar::Entry { path: &format!("{v}/dtb/t8010-j71b.dtb"), mode: 0o644, data: b"TREE" },
            tar::Entry { path: &format!("{v}/kernel/drivers/a.ko"), mode: 0o644, data: b"MODULE" },
            tar::Entry { path: "usr/lib/orchard/boot-entries", mode: 0o755, data: b"#!/bin/sh\n" },
        ];
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar::write(&entries).unwrap()).unwrap();
        gz.finish().unwrap()
    }

    fn system<'a>(entry: &'a Value, account: &'a Value, secrets: &'a Account) -> System<'a> {
        System {
            entry,
            root_partuuid: "1a2b3c4d-0000-4000-8000-000000000001",
            boot_partuuid: "5e6f7a8b-0000-4000-8000-000000000002",
            account: Some(account),
            secrets,
            timezone: Some("Europe/Berlin"),
            today: 20_783,
            now: 1_795_700_000,
        }
    }

    #[test]
    fn a_root_is_finished_like_the_install_system_does() {
        let dir = tempfile::tempdir().unwrap();
        let (Some(tools), Some(image)) = (host_tools(), image(dir.path(), 64)) else {
            eprintln!("skipped: needs ios-installer/out/e2fsprogs-host and mke2fs");
            return;
        };
        let fs = Ext4::new(&tools, image.to_str().unwrap(), &dir.path().join("work")).unwrap();
        fs.apply(&[
            Change::file("/etc/passwd", "root:x:0:0::/root:/bin/sh\ntablet:x:1000:1000::/home/tablet:/bin/bash\n", 0o644),
            Change::file("/etc/shadow", "root:!:20000::::::\ntablet:!:20000:0:99999:7:::\n", 0o600),
            Change::file("/usr/share/zoneinfo/Europe/Berlin", "TZif", 0o644),
            Change::file("/usr/share/orchard/splash", "SPLASH", 0o644),
            Change::Link { path: "/etc/localtime".into(), target: "../usr/share/zoneinfo/UTC".into() },
        ])
        .unwrap();
        let entry = install_plan::entry("Omarchy A", None, "t8010-j71b.dtb");
        let account = serde_json::json!({"user": "tablet", "password": true, "pin": true, "passwordless_sudo": false});
        let secrets = Account { password: "correct horse".into(), pin: "123456".into() };
        let layer = layer();
        let firmware = vec![("lib/firmware/brcm/a.bin".to_string(), b"FW".to_vec())];
        let done = finish(&fs, &system(&entry, &account, &secrets), &firmware, &Kernel::Layer(&layer), &|_| {}).unwrap();

        let shadow = String::from_utf8(fs.read("/etc/shadow").unwrap().unwrap()).unwrap();
        let line = shadow.lines().find(|l| l.starts_with("tablet:")).unwrap();
        let fields: Vec<&str> = line.split(':').collect();
        let salt = fields[1].split('$').nth(2).unwrap();
        assert_eq!(fields[1], crypt::sha512(b"correct horse", salt));
        assert_eq!(fields[2], "20783");
        assert!(shadow.starts_with("root:!:20000"), "other accounts stay");
        let pin = String::from_utf8(fs.read("/etc/orchard/pin").unwrap().unwrap()).unwrap();
        assert!(pin.starts_with("tablet:$6$"));
        assert_eq!(fs.read("/usr/lib/firmware/brcm/a.bin").unwrap().unwrap(), b"FW");
        assert_eq!(fs.read("/usr/lib/modules/7.3.0-test/kernel/drivers/a.ko").unwrap().unwrap(), b"MODULE");
        let config = String::from_utf8(fs.read("/etc/orchard/boot-entry").unwrap().unwrap()).unwrap();
        assert!(config.contains("token=omarchy-a-1a2b3c4d\n") && config.contains("devicetree=t8010-j71b.dtb\n"));
        assert!(config.contains("options=root=PARTUUID=1a2b3c4d-0000-4000-8000-000000000001 rw rootwait fbcon=map:9"));
        let listing = fs.list(&BTreeSet::from(["/etc".to_string(), "/usr/lib/orchard".to_string(), "/var/lib/orchard".to_string()])).unwrap();
        let etc = listing["/etc"].as_ref().unwrap();
        assert_eq!(etc["shadow"].mode, 0o100600);
        assert_eq!(etc["localtime"].mode & 0o170000, 0o120000);
        assert_eq!(listing["/usr/lib/orchard"].as_ref().unwrap()["boot-entries"].mode, 0o100755);
        assert_eq!(listing["/var/lib/orchard"].as_ref().unwrap()["pin-tally"], Item { mode: 0o040700, uid: 1000, gid: 1000 });
        fs.check().unwrap();

        assert_eq!(done.entry.as_deref(), Some("omarchy-a-1a2b3c4d-7.3.0-test"));
        let boot: BTreeMap<&str, &[u8]> = done.boot_files.iter().map(|(p, d)| (p.as_str(), d.as_slice())).collect();
        assert_eq!(boot["omarchy-a-1a2b3c4d/7.3.0-test/linux"], b"KERNEL");
        assert_eq!(boot["omarchy-a-1a2b3c4d/7.3.0-test/splash"], b"SPLASH");
        assert_eq!(boot["loader/loader.conf"], b"default omarchy-a-1a2b3c4d-*\n");
        let conf = String::from_utf8_lossy(boot["loader/entries/omarchy-a-1a2b3c4d-7.3.0-test.conf"]);
        assert!(conf.contains("devicetree /omarchy-a-1a2b3c4d/7.3.0-test/t8010-j71b.dtb\n") && conf.ends_with("architecture aa64\n"));

        // A second kernel on top is the image and the layer disagreeing.
        let again = finish(&fs, &system(&entry, &account, &secrets), &[], &Kernel::Layer(&layer), &|_| {}).unwrap_err();
        assert!(again.to_string().contains("already has"), "{again}");
    }

    #[test]
    fn a_package_leaves_its_record_in_the_package_database() {
        let v = "usr/lib/modules/7.3.0-test";
        let info = b"# Generated\npkgname = linux-orchard-apple-16k\npkgbase = linux-orchard-apple-16k\npkgver = 0.3.8-1\npkgdesc = orchard's kernel\nsize = 6\narch = aarch64\nlicense = GPL-2.0-only\nlicense = ISC\nprovides = linux-orchard\nconflict = linux-orchard\n";
        let tarball = tar::write(&[
            tar::Entry { path: ".MTREE", mode: 0o644, data: b"MTREE" },
            tar::Entry { path: ".PKGINFO", mode: 0o644, data: info },
            tar::Entry { path: &format!("{v}/vmlinuz"), mode: 0o644, data: b"KERNEL" },
        ])
        .unwrap();
        let mut packed = Vec::new();
        ruzstd::encoding::compress(tarball.as_slice(), &mut packed, ruzstd::encoding::CompressionLevel::Fastest);
        let (files, record) = package_files(&packed, 1_795_700_000).unwrap();
        assert_eq!(files.len(), 1);
        let Change::File { path, data, .. } = &record[0] else { panic!() };
        assert_eq!(path, "/var/lib/pacman/local/linux-orchard-apple-16k-0.3.8-1/desc");
        let desc = String::from_utf8_lossy(data);
        assert!(desc.starts_with("%NAME%\nlinux-orchard-apple-16k\n\n%VERSION%\n0.3.8-1\n\n"));
        assert!(desc.contains("%LICENSE%\nGPL-2.0-only\nISC\n\n") && desc.contains("%INSTALLDATE%\n1795700000\n\n") && desc.contains("%PROVIDES%\nlinux-orchard\n\n"));
        let Change::File { data, .. } = &record[1] else { panic!() };
        assert_eq!(String::from_utf8_lossy(data), format!("%FILES%\nusr/\nusr/lib/\nusr/lib/modules/\n{v}/\n{v}/vmlinuz\n\n"));
        assert!(matches!(&record[2], Change::File { path, .. } if path.ends_with("/mtree")));
    }

    #[test]
    fn the_account_must_be_the_images_one_user() {
        assert_eq!(account_user("root:x:0:0::/root:/bin/sh\ntablet:x:1000:1000::/home/tablet:/bin/sh\n", "tablet").unwrap(), (1000, 1000, "/home/tablet".into()));
        assert!(account_user("root:x:0:0::/root:/bin/sh\n", "tablet").is_err());
        assert!(shadow_with("root:!:1::::::\n", "tablet", "h", 2).is_err());
    }
}

//! A running system on the device: a shell on it, a command, a file to or
//! from it. A RAM payload (`orchard2 build` gives it its USB session) or an
//! installed system.
//!
//! An installed system is reached as its user with this computer's key,
//! which `orchard2 install --ssh` imported; root there is sudo, as on any
//! system. A RAM payload is reached as root with this computer's key for
//! payloads, which `orchard2 build` put into it. The payload makes a new
//! host key each time it starts; it is read over the USB serial console,
//! which is the cable and so trusted, and pinned for that start. An address
//! given by hand (`--host`) is trusted as `ssh` trusts a new one: on first
//! use. Without SSH, a file up to 16 MiB goes over the serial console.
//! Every file is checked by its SHA-256 after it is copied.
pub mod serial;
pub mod ssh;

use orchard_cache::{Code, Context, Error, Result};
use orchard_usb::{Device, Mode, System};
use serial::{quote, Console, Tty};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// What a command finds its way with on the device, whatever the shell's own.
const PATH: &str = "export PATH=/bin:/sbin:/usr/bin:/usr/sbin; ";

/// How to reach the system.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    /// An installed system's user; default: the one its install set up.
    pub user: Option<String>,
    /// An SSH address instead of the device on USB.
    pub host: Option<String>,
    /// With `host`: it is a RAM payload, reached as root.
    pub ram: bool,
}

/// The device a command works on.
pub struct Target {
    pub device: Device,
    /// Its serial console, where the computer found one.
    pub console: Option<PathBuf>,
}

impl Target {
    fn payload(&self) -> bool {
        self.device.system == Some(System::Ram)
    }

    /// Fails once the device is no longer there as it was found.
    fn check(&self) -> Result<()> {
        let listed = orchard_usb::list().map_err(|e| Error::new(Code::Usb, e.to_string()))?;
        let same = |d: &Device| d.location == self.device.location && d.mode == self.device.mode && d.system == self.device.system;
        match listed.iter().any(same) {
            true => Ok(()),
            false => Err(Error::new(Code::DeviceChanged, "the device disconnected or restarted")),
        }
    }
}

/// The one running RAM payload on USB, else the one installed system.
pub fn target() -> Result<Target> {
    let listed = orchard_usb::list().map_err(|e| Error::new(Code::Usb, e.to_string()))?;
    choose(&listed, &orchard_usb::serial_ports(&listed))
}

fn choose(listed: &[Device], consoles: &[Option<PathBuf>]) -> Result<Target> {
    let of = |system: System| -> Vec<usize> {
        (0..listed.len())
            .filter(|&i| listed[i].mode == Mode::Linux && listed[i].system == Some(system))
            .collect()
    };
    let (payloads, systems) = (of(System::Ram), of(System::Installed));
    let index = match (payloads.as_slice(), systems.as_slice()) {
        ([one], _) => *one,
        ([], [one]) => *one,
        ([], []) => {
            return Err(Error::new(Code::NoDevice, "no running RAM payload or installed system is connected")
                .hint("orchard2 devices lists what is there; a payload needs its USB session, a system its USB network"))
        }
        _ => {
            return Err(Error::new(Code::Ambiguous, "more than one running system is connected")
                .hint("connect one, or reach one by its address with --host"))
        }
    };
    Ok(Target { device: listed[index].clone(), console: consoles.get(index).cloned().flatten() })
}

/// This computer's keys and what it remembers, in the cache.
struct Keys {
    /// `private/ram-ssh`: the key for RAM payloads and their pinned host keys.
    payloads: PathBuf,
    /// `private/ssh`: the key for installed systems, their host keys, the user.
    systems: PathBuf,
}

impl Keys {
    fn new(cache: &Path) -> Keys {
        Keys { payloads: cache.join("private/ram-ssh"), systems: cache.join("private/ssh") }
    }

    fn identity(directory: &Path, made_by: &str) -> Result<PathBuf> {
        let key = directory.join("id_ed25519");
        match key.is_file() {
            true => Ok(key),
            false => Err(Error::new(Code::NotFound, "this computer has no key for it yet").context(key.display()).hint(made_by)),
        }
    }

    fn payload_identity(&self) -> Result<PathBuf> {
        Keys::identity(&self.payloads, "orchard2 build makes the key and puts it into the payload")
    }

    fn system_identity(&self) -> Result<PathBuf> {
        Keys::identity(&self.systems, "orchard2 install --ssh makes the key and lets it sign in")
    }

    /// The user an install on this computer set up, or the one named: the
    /// one remembered for this unit (`user-UNIT`), else the last install's.
    fn user(&self, named: Option<&str>, unit: Option<&str>) -> Result<String> {
        let remembered = |file: String| std::fs::read_to_string(self.systems.join(file)).ok().map(|text| text.trim().to_owned()).filter(|u| !u.is_empty());
        let user = match named {
            Some(user) => user.to_owned(),
            None => unit.and_then(|unit| remembered(format!("user-{unit}"))).or_else(|| remembered("user".into())).unwrap_or_default(),
        };
        if user.is_empty() {
            return Err(Error::new(Code::InvalidInput, "which user? This computer installed no system that allows it to sign in")
                .hint("name one with --user"));
        }
        if !ssh::valid_user(&user) {
            return Err(Error::new(Code::InvalidInput, format!("{user:?} is not a user name")));
        }
        Ok(user)
    }
}

/// The name an installed system's host key is kept under: the unit's, as
/// `orchard2 install --ssh` forgets it by.
pub fn alias(unit: &str) -> String {
    format!("orchard-{unit}")
}

/// Writes a file only this user reads.
fn private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).context(path.display())?;
    file.write_all(text.as_bytes()).context(path.display())
}

/// The start of a payload (its kernel's boot id) and its SSH host key, as
/// the payload prints them on its console.
fn boot_and_key(printed: &str) -> Result<(String, String)> {
    let first = printed.trim().lines().next().unwrap_or("").trim();
    let uuid = first.len() == 36
        && first.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    if !uuid {
        return Err(Error::new(Code::Format, "the payload gave no boot id over USB serial"));
    }
    Ok((first.to_ascii_lowercase(), ssh::host_key(printed)?))
}

/// The `ssh` command line that reaches the system, signed in once already.
pub fn connect(cache: &Path, reach: &Reach) -> Result<Vec<OsString>> {
    let keys = Keys::new(cache);
    if reach.ram && (reach.host.is_none() || reach.user.is_some()) {
        return Err(Error::new(Code::InvalidInput, "--ram goes with --host, and signs in as root: no --user"));
    }
    if let Some(host) = &reach.host {
        if !ssh::valid_host(host) {
            return Err(Error::new(Code::InvalidInput, format!("{host:?} is not an SSH address")));
        }
        if reach.ram {
            let hosts = keys.payloads.join("network_known_hosts");
            return Ok(ssh::payload(&keys.payload_identity()?, &hosts, host, false));
        }
        let user = keys.user(reach.user.as_deref(), None)?;
        return Ok(ssh::system(&keys.system_identity()?, &keys.systems.join("known_hosts"), &user, host, None));
    }
    let target = target()?;
    let check = || target.check();
    if !target.payload() {
        // The unit's own name on USB, where it gives one: its host key and
        // its user are kept under it.
        let unit = target.device.identity.unit.as_deref();
        let user = keys.user(reach.user.as_deref(), unit)?;
        let alias = unit.map(alias);
        let argv = ssh::system(&keys.system_identity()?, &keys.systems.join("known_hosts"), &user, ssh::HOST, alias.as_deref());
        ssh::wait(&argv, &check, Duration::from_secs(20))?;
        return Ok(argv);
    }
    let identity = keys.payload_identity()?;
    // Asked every time: the same device can have started anew, and the
    // console gives the start and its host key without any USB change.
    let (code, printed) = console(&target)?.run(
        "cat /proc/sys/kernel/random/boot_id && cat /run/orchard/ssh_host_ed25519_key.pub",
        Duration::from_secs(15),
    )?;
    if code != 0 {
        return Err(Error::new(Code::Unsupported, "this payload has no SSH")
            .hint("its init must run /usr/libexec/orchard/usb-session, which orchard2 build adds unless orchard.toml says transport = false; --serial copies files without it"));
    }
    let (boot, key) = boot_and_key(&printed)?;
    let pinned = format!("{} {key}\n", ssh::HOST);
    let hosts = keys.payloads.join(format!("known_hosts-{boot}"));
    match std::fs::read_to_string(&hosts) {
        Ok(known) if known != pinned => {
            return Err(Error::new(Code::Refused, "the payload's SSH host key changed although it did not start anew"))
        }
        Ok(_) => {}
        Err(_) => {
            // An earlier start's key is of no use any more.
            for old in std::fs::read_dir(&keys.payloads).into_iter().flatten().flatten() {
                if old.file_name().to_string_lossy().starts_with("known_hosts-") {
                    let _ = std::fs::remove_file(old.path());
                }
            }
            private(&hosts, &pinned)?;
        }
    }
    let argv = ssh::payload(&identity, &hosts, ssh::HOST, true);
    ssh::wait(&argv, &check, Duration::from_secs(20))?;
    Ok(argv)
}

/// The payload's serial console, opened.
fn console(target: &Target) -> Result<Console<Tty>> {
    if !target.payload() {
        return Err(Error::new(Code::Unsupported, "an installed system's serial console is a login, not a shell")
            .hint("--serial is for RAM payloads"));
    }
    let port = target.console.as_deref().ok_or_else(|| Error::new(Code::NotFound, "the RAM payload has no USB serial port here"))?;
    target.check()?;
    Ok(Console::new(Tty::open(port)?))
}

fn status(command: &mut Command) -> Result<i32> {
    let status = command.status().map_err(|e| Error::new(Code::NotFound, e.to_string()).context("ssh"))?;
    Ok(status.code().unwrap_or(255))
}

/// A shell on the system, at this terminal. Its exit status.
pub fn shell(cache: &Path, reach: &Reach) -> Result<i32> {
    let argv = connect(cache, reach)?;
    let (address, options) = argv.split_last().expect("ssh and its address");
    status(Command::new(&options[0]).args(&options[1..]).arg("-t").arg(address))
}

/// Runs `words` as one command on the system. Its exit status.
pub fn exec(cache: &Path, reach: &Reach, words: &[String]) -> Result<i32> {
    let argv = connect(cache, reach)?;
    let line = words.iter().map(|word| quote(word)).collect::<Vec<_>>().join(" ");
    status(Command::new(&argv[0]).args(&argv[1..]).arg(format!("{PATH}{line}")))
}

/// A file that was copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    pub from: String,
    pub to: String,
    pub size: u64,
    pub sha256: String,
}

fn sha256_of(path: &Path) -> Result<(String, u64)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).context(path.display())?;
    let (mut hash, mut size, mut block) = (Sha256::new(), 0u64, vec![0u8; 1 << 20]);
    loop {
        let n = file.read(&mut block).context(path.display())?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
        size += n as u64;
    }
    Ok((hash.finalize().iter().map(|b| format!("{b:02x}")).collect(), size))
}

/// The checksum in what `sha256sum` printed.
fn remote_sha256(printed: &str) -> Result<String> {
    let first = printed.trim().split_whitespace().next().unwrap_or("");
    match first.len() == 64 && first.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        true => Ok(first.to_owned()),
        false => {
            let tail: String = printed.trim().chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
            Err(Error::new(Code::Format, format!("the device gave no checksum: {tail}")))
        }
    }
}

/// One command over SSH, its output captured.
fn asked(argv: &[OsString], command: &str) -> Result<String> {
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .arg(command)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| Error::new(Code::NotFound, e.to_string()).context("ssh"))?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        false => Err(Error::new(Code::Refused, format!("{command} failed on the device ({})", output.status))),
    }
}

/// One transfer over SSH. A connection that drops (the device can stall
/// under load until SSH gives up) is made again once, and the file starts
/// over. Returns the command line that worked.
fn transfer(
    cache: &Path,
    reach: &Reach,
    mut argv: Vec<OsString>,
    command: &str,
    stream: &dyn Fn() -> Result<(Stdio, Stdio)>,
) -> Result<Vec<OsString>> {
    for attempt in 1..=2 {
        let (input, output) = stream()?;
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .arg(command)
            .stdin(input)
            .stdout(output)
            .status()
            .map_err(|e| Error::new(Code::NotFound, e.to_string()).context("ssh"))?;
        match status.code() {
            Some(0) => return Ok(argv),
            Some(255) | None if attempt == 1 => {
                eprintln!("The connection to the device dropped; connecting again");
                argv = connect(cache, reach)?;
            }
            Some(255) | None => return Err(Error::new(Code::Io, "the transfer failed: the connection to the device dropped twice")),
            Some(code) => return Err(Error::new(Code::Refused, format!("the transfer failed on the device (exit {code})"))),
        }
    }
    unreachable!("two attempts, each returns or connects again")
}

/// Copies a file of this computer to the system. It is written beside its
/// place, checked, and moved there only when its checksum is the file's.
pub fn push(cache: &Path, reach: &Reach, local: &Path, remote: &str, over_serial: bool) -> Result<Copied> {
    if over_serial && (reach.host.is_some() || reach.ram) {
        return Err(Error::new(Code::InvalidInput, "--serial is the cable: not with --host or --ram"));
    }
    let (expected, size) = sha256_of(local)?;
    let part = format!("{remote}.part");
    let (to, to_part) = (quote(remote), quote(&part));
    let actual = if over_serial {
        if size == 0 {
            return Err(Error::new(Code::InvalidInput, "the file is empty; a serial transfer sends at least one byte").hint("over SSH (without --serial) an empty file is copied too"));
        }
        if size as usize > serial::LIMIT {
            return Err(Error::new(Code::TooLarge, "a serial transfer is up to 16 MiB; larger files go over SSH"));
        }
        let mut console = console(&target()?)?;
        let data = std::fs::read(local).context(local.display())?;
        // Asked first, in a shell of its own: a place that cannot be written
        // ends here, before any byte of the file is sent.
        if console.run(&format!(": > {to_part}"), Duration::from_secs(25))?.0 != 0 {
            return Err(Error::new(Code::Refused, format!("{part}: the device cannot write there")));
        }
        console.send_file(&to_part, &data, Duration::from_secs(60 + size / 8000))?;
        let (code, printed) = console.run(&format!("sha256sum {to_part}"), Duration::from_secs(25))?;
        let actual = match code {
            0 => remote_sha256(&printed)?,
            _ => String::new(),
        };
        if actual == expected && console.run(&format!("mv {to_part} {to}"), Duration::from_secs(25))?.0 != 0 {
            return Err(Error::new(Code::Refused, format!("{remote}: copied as {part}, but it could not be moved into place")));
        }
        actual
    } else {
        let argv = connect(cache, reach)?;
        let argv = transfer(cache, reach, argv, &format!("cat > {to_part}"), &|| {
            Ok((std::fs::File::open(local).context(local.display())?.into(), Stdio::inherit()))
        })?;
        let actual = remote_sha256(&asked(&argv, &format!("sha256sum {to_part}"))?)?;
        if actual == expected {
            asked(&argv, &format!("mv {to_part} {to}"))?;
        }
        actual
    };
    if actual != expected {
        return Err(Error::new(Code::Format, format!("{remote}: the checksum differs after the transfer; left as {part}")));
    }
    Ok(Copied { from: local.display().to_string(), to: remote.to_owned(), size, sha256: expected })
}

/// Copies a file of the system to this computer. It is written beside its
/// place and moved there only when its checksum is the file's.
pub fn pull(cache: &Path, reach: &Reach, remote: &str, local: &Path, over_serial: bool) -> Result<Copied> {
    if over_serial && (reach.host.is_some() || reach.ram) {
        return Err(Error::new(Code::InvalidInput, "--serial is the cable: not with --host or --ram"));
    }
    let from = quote(remote);
    let mut name = local.file_name().ok_or_else(|| Error::new(Code::InvalidInput, "the local path names no file"))?.to_owned();
    name.push(".part");
    let part = local.with_file_name(name);
    let expected = if over_serial {
        let mut console = console(&target()?)?;
        let (code, printed) = console.run(&format!("wc -c < {from}; sha256sum {from}"), Duration::from_secs(25))?;
        let mut words = printed.split_whitespace();
        let size = words.next().filter(|_| code == 0).and_then(|n| n.parse::<usize>().ok());
        let (Some(size), Some(sum)) = (size, words.next()) else {
            return Err(Error::new(Code::NotFound, format!("{remote}: not readable on the device")));
        };
        let expected = remote_sha256(sum)?;
        let data = console.capture_exact(&format!("cat {from}"), size, Duration::from_secs(60 + size as u64 / 8000))?;
        std::fs::write(&part, data).context(part.display())?;
        expected
    } else {
        let argv = connect(cache, reach)?;
        let expected = remote_sha256(&asked(&argv, &format!("sha256sum {from}"))?)?;
        transfer(cache, reach, argv, &format!("cat {from}"), &|| {
            Ok((Stdio::null(), std::fs::File::create(&part).context(part.display())?.into()))
        })?;
        expected
    };
    let (actual, size) = sha256_of(&part)?;
    if actual != expected {
        return Err(Error::new(Code::Format, format!("{remote}: the checksum differs after the transfer; left as {}", part.display())));
    }
    std::fs::rename(&part, local).context(local.display())?;
    Ok(Copied { from: remote.to_owned(), to: local.display().to_string(), size, sha256: expected })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(system: Option<System>, location: &str) -> Device {
        Device { mode: Mode::Linux, system, location: location.into(), product: None, identity: orchard_usb::Identity::default() }
    }

    #[test]
    fn the_payload_goes_before_the_installed_system_and_two_are_too_many() {
        let ram = device(Some(System::Ram), "1");
        let installed = device(Some(System::Installed), "2");
        let other = device(None, "3");
        let console = Some(PathBuf::from("/dev/cu.usbmodem1"));
        let picked = choose(&[other.clone(), installed.clone(), ram.clone()], &[None, None, console.clone()]).unwrap();
        assert_eq!((picked.device.location.as_str(), picked.payload(), picked.console), ("1", true, console));
        let picked = choose(&[other.clone(), installed.clone()], &[None, None]).unwrap();
        assert!(!picked.payload() && picked.device.location == "2");
        assert_eq!(choose(&[other], &[None]).err().unwrap().code, Code::NoDevice);
        assert_eq!(choose(&[ram.clone(), ram], &[None, None]).err().unwrap().code, Code::Ambiguous);
        assert_eq!(choose(&[installed.clone(), installed], &[None, None]).err().unwrap().code, Code::Ambiguous);
    }

    #[test]
    fn a_payloads_start_and_key_come_off_its_console() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc";
        let (boot, got) = boot_and_key(&format!("\r\n0B1c2d3e-0000-4000-8000-0123456789ab\r\n{key}\r\n")).unwrap();
        assert_eq!((boot.as_str(), got.as_str()), ("0b1c2d3e-0000-4000-8000-0123456789ab", key));
        assert!(boot_and_key(&format!("not-a-boot-id\n{key}\n")).is_err());
        assert!(boot_and_key("0b1c2d3e-0000-4000-8000-0123456789ab\n").is_err());
    }

    #[test]
    fn checksums_and_users_are_read_strictly() {
        let sum = "a".repeat(64);
        assert_eq!(remote_sha256(&format!("{sum}  /tmp/x\n")).unwrap(), sum);
        assert!(remote_sha256("sha256sum: /tmp/x: No such file\n").is_err());
        assert!(remote_sha256(&"A".repeat(64)).is_err());

        let dir = tempfile::tempdir().unwrap();
        let keys = Keys::new(dir.path());
        assert_eq!(keys.user(None, None).unwrap_err().code, Code::InvalidInput);
        assert_eq!(keys.user(Some("tablet"), None).unwrap(), "tablet");
        assert!(keys.user(Some("-oProxyCommand=x"), None).is_err());
        // What an install with SSH allowed leaves (runtime's ssh_keys).
        std::fs::create_dir_all(dir.path().join("private/ssh")).unwrap();
        std::fs::write(dir.path().join("private/ssh/user"), "tablet\n").unwrap();
        assert_eq!(keys.user(None, None).unwrap(), "tablet");
        // A second tablet has its own user; one this computer did not
        // install falls back to the last install's.
        std::fs::write(dir.path().join("private/ssh/user-0123456789ab"), "other\n").unwrap();
        assert_eq!(keys.user(None, Some("0123456789ab")).unwrap(), "other");
        assert_eq!(keys.user(None, Some("ffffffffffff")).unwrap(), "tablet");
        assert_eq!(keys.user(Some("named"), Some("0123456789ab")).unwrap(), "named");
        assert_eq!(alias("0123456789ab"), "orchard-0123456789ab");
        // No key yet: said, with what makes one.
        assert!(keys.payload_identity().unwrap_err().hint.unwrap().contains("orchard2 build"));
        assert!(keys.system_identity().unwrap_err().hint.unwrap().contains("install --ssh"));
    }

    #[test]
    fn an_address_given_by_hand_needs_no_device() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["private/ram-ssh", "private/ssh"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
            std::fs::write(dir.path().join(sub).join("id_ed25519"), "key").unwrap();
        }
        let reach = |user: Option<&str>, host: Option<&str>, ram| Reach { user: user.map(str::to_owned), host: host.map(str::to_owned), ram };
        let argv = connect(dir.path(), &reach(None, Some("10.0.0.5"), true)).unwrap();
        assert_eq!(argv.last().unwrap(), "root@10.0.0.5");
        let argv = connect(dir.path(), &reach(Some("tablet"), Some("ipad.local"), false)).unwrap();
        assert_eq!(argv.last().unwrap(), "tablet@ipad.local");
        for bad in [reach(None, None, true), reach(Some("tablet"), Some("10.0.0.5"), true), reach(None, Some("-oX=y"), true), reach(None, Some("10.0.0.5"), false)] {
            assert_eq!(connect(dir.path(), &bad).unwrap_err().code, Code::InvalidInput, "{bad:?}");
        }
    }

    #[test]
    fn a_file_is_hashed_with_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        let (sum, size) = sha256_of(&path).unwrap();
        assert_eq!(size, 3);
        assert_eq!(sum, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}

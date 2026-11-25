//! The `ssh` command lines for a running system, and waiting until one of
//! them gets in.
use orchard_cache::{Code, Error, Result};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The device's address on the USB network every Orchard system brings up.
pub const HOST: &str = "172.16.42.1";

fn words(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn known_hosts(file: &Path) -> OsString {
    // Quoted for ssh's own parsing of the option's value: the cache's path
    // can hold a space.
    format!("UserKnownHostsFile=\"{}\"", file.display()).into()
}

/// SSH as root to a RAM payload, whose host key `hosts` pins. `strict`
/// refuses any other key; without it the first connection to a network
/// address learns the key.
pub fn payload(identity: &Path, hosts: &Path, host: &str, strict: bool) -> Vec<OsString> {
    let mut argv = words(&["ssh", "-p", "22", "-i"]);
    argv.push(identity.into());
    argv.extend(words(&[
        "-o", "IdentitiesOnly=yes",
        "-o", "Compression=no",
        "-o", "ServerAliveInterval=5",
        "-o", "ServerAliveCountMax=3",
        "-o",
    ]));
    argv.push(known_hosts(hosts));
    argv.extend(words(&[
        "-o", if strict { "StrictHostKeyChecking=yes" } else { "StrictHostKeyChecking=accept-new" },
        "-o", "BatchMode=yes",
        "-o", "ControlMaster=no",
        "-o", "ControlPath=none",
        "-o", "ControlPersist=no",
        "-o", "ConnectTimeout=8",
        "-o", "HostKeyAlgorithms=ssh-ed25519",
    ]));
    argv.push(format!("root@{host}").into());
    argv
}

/// SSH as `user` to an installed system with this computer's key. Its host
/// key is learned on the first connection and kept in `hosts`, under
/// `alias` when there is one: every device on the cable has the same
/// address, so its key is kept under the device's own name.
pub fn system(identity: &Path, hosts: &Path, user: &str, host: &str, alias: Option<&str>) -> Vec<OsString> {
    let mut argv = words(&["ssh", "-i"]);
    argv.push(identity.into());
    argv.extend(words(&["-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o"]));
    argv.push(known_hosts(hosts));
    if let Some(alias) = alias {
        argv.extend([OsString::from("-o"), format!("HostKeyAlias={alias}").into()]);
    }
    argv.extend(words(&["-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=8"]));
    argv.push(format!("{user}@{host}").into());
    argv
}

/// An address given by hand: never something `ssh` would read as an
/// option, a user or a port.
pub fn valid_host(host: &str) -> bool {
    let mut chars = host.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || ".:-".contains(c))
}

/// A user name as an account has it.
pub fn valid_user(user: &str) -> bool {
    let mut chars = user.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && user.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c))
}

/// The one ed25519 host key in what the payload printed, as `TYPE BASE64`.
pub fn host_key(text: &str) -> Result<String> {
    let base64 = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b));
    let mut keys = text.lines().map(str::trim).filter(|line| {
        let mut parts = line.split(' ');
        parts.next() == Some("ssh-ed25519") && parts.next().is_some_and(base64) && parts.next().is_none()
    });
    match (keys.next(), keys.next()) {
        (Some(key), None) => Ok(key.to_owned()),
        _ => Err(Error::new(Code::Format, "the payload did not give exactly one SSH host key over USB serial")),
    }
}

/// Runs `argv` with `command` for at most `limit`: its exit status and what
/// it wrote to stderr, or None when it was ended for not finishing.
fn attempt(argv: &[OsString], command: &str, limit: Duration) -> std::io::Result<Option<(bool, String)>> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let end = Instant::now() + limit;
    while child.try_wait()?.is_none() {
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output()?;
    Ok(Some((output.status.success(), String::from_utf8_lossy(&output.stderr).trim().to_owned())))
}

/// Waits until `argv` signs in, for at most `limit`. `check` fails when the
/// device is no longer the one that was found; a refusal of the key or the
/// host key ends the wait at once.
pub fn wait(argv: &[OsString], check: &dyn Fn() -> Result<()>, limit: Duration) -> Result<()> {
    let deadline = Instant::now() + limit;
    let mut last = "no answer".to_owned();
    loop {
        check()?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::new(Code::Timeout, format!("the device is on USB, but SSH to it did not get in: {last}")));
        }
        match attempt(argv, "true", left.min(Duration::from_secs(3))) {
            Err(e) => return Err(Error::new(Code::NotFound, e.to_string()).context("ssh")),
            Ok(None) => last = "the connection timed out".into(),
            Ok(Some((true, _))) => return check(),
            Ok(Some((false, said))) => {
                let lower = said.to_lowercase();
                let refused = ["host key verification failed", "remote host identification has changed", "permission denied"];
                if refused.iter().any(|text| lower.contains(text)) {
                    return Err(refusal(&said));
                }
                last = said;
            }
        }
        std::thread::sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// The device answered and refused: its key is not the pinned one, or it
/// does not take this computer's.
fn refusal(said: &str) -> Error {
    let last = said.lines().last().unwrap_or(said);
    let error = Error::new(Code::Refused, format!("the device refused the sign-in: {last}"));
    let said = said.to_lowercase();
    if said.contains("permission denied") {
        return error.hint(
            "it does not take this computer's key: a payload gets it from orchard2 build, an installed system from orchard2 install --ssh",
        );
    }
    // Not the key that was learned for it: another system answers there.
    if said.contains("host key verification failed") || said.contains("host identification has changed") {
        return error.hint(
            "its host key is not the one this computer knows for it; orchard2 install --ssh forgets the old one, and after an install by another tool its line in private/ssh/known_hosts in the cache has to go",
        );
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(argv: &[OsString]) -> String {
        argv.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn a_payload_is_reached_as_root_with_its_key_pinned() {
        let argv = payload(Path::new("/c/id"), Path::new("/c a/hosts"), HOST, true);
        let text = line(&argv);
        assert!(text.starts_with("ssh -p 22 -i /c/id -o IdentitiesOnly=yes"), "{text}");
        assert!(text.contains("UserKnownHostsFile=\"/c a/hosts\" -o StrictHostKeyChecking=yes"), "{text}");
        assert!(text.ends_with("-o HostKeyAlgorithms=ssh-ed25519 root@172.16.42.1"), "{text}");
        assert!(line(&payload(Path::new("i"), Path::new("h"), "10.0.0.2", false)).contains("StrictHostKeyChecking=accept-new"));
    }

    #[test]
    fn a_system_is_reached_as_its_user() {
        let text = line(&system(Path::new("/c/id"), Path::new("/c/hosts"), "tablet", HOST, None));
        assert!(text.starts_with("ssh -i /c/id -o IdentitiesOnly=yes -o BatchMode=yes"), "{text}");
        assert!(text.ends_with("-o ConnectTimeout=8 tablet@172.16.42.1"), "{text}");
        assert!(!text.contains("HostKeyAlias"), "{text}");
        // On the cable every device has that address: its key goes under its own name.
        let text = line(&system(Path::new("/c/id"), Path::new("/c/hosts"), "tablet", HOST, Some("orchard-0123456789ab")));
        assert!(text.contains("-o HostKeyAlias=orchard-0123456789ab -o StrictHostKeyChecking=accept-new"), "{text}");
    }

    #[test]
    fn an_address_or_a_user_is_never_an_option() {
        for good in ["172.16.42.1", "ipad.local", "fe80::1", "host-1"] {
            assert!(valid_host(good), "{good}");
        }
        for bad in ["", "-oProxyCommand=x", "a b", "user@host", "host/1", ".local"] {
            assert!(!valid_host(bad), "{bad}");
        }
        assert!(valid_user("tablet") && valid_user("_a-1"));
        for bad in ["", "-l", "Root", "a b", "a@b", "1a"] {
            assert!(!valid_user(bad), "{bad}");
        }
    }

    #[test]
    fn the_host_key_is_the_one_ed25519_line() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc+/=";
        assert_eq!(host_key(&format!("noise\n  {key}  \n")).unwrap(), key);
        assert!(host_key("").is_err());
        assert!(host_key(&format!("{key}\n{key}\n")).is_err());
        assert!(host_key("ssh-rsa AAAA\n").is_err());
        assert!(host_key("ssh-ed25519 AAAA comment\n").is_err());
    }

    #[test]
    fn a_sign_in_that_never_comes_ends_with_what_was_last_said() {
        // A command that is not ssh and fails at once, as a closed port does.
        let argv = words(&["sh", "-c", "echo 'connection refused' >&2; exit 255", "ssh"]);
        let e = wait(&argv, &|| Ok(()), Duration::from_millis(400)).unwrap_err();
        assert_eq!(e.code, Code::Timeout);
        assert!(e.message.contains("connection refused"), "{}", e.message);
        // A refused key ends it at once, with what to do.
        let argv = words(&["sh", "-c", "echo 'root@x: Permission denied (publickey).' >&2; exit 255", "ssh"]);
        let e = wait(&argv, &|| Ok(()), Duration::from_secs(30)).unwrap_err();
        assert_eq!(e.code, Code::Refused);
        assert!(e.hint.unwrap().contains("orchard2 build"));
        // The device going away ends it too.
        let gone = wait(&words(&["true"]), &|| Err(Error::new(Code::DeviceChanged, "gone")), Duration::from_secs(30));
        assert_eq!(gone.unwrap_err().code, Code::DeviceChanged);
        wait(&words(&["sh", "-c", "exit 0", "ssh"]), &|| Ok(()), Duration::from_secs(30)).unwrap();
    }

    #[test]
    fn a_refusal_says_what_would_help() {
        let key = refusal("root@172.16.42.1: Permission denied (publickey).");
        assert!(key.hint.unwrap().contains("orchard2 build"));
        let host = refusal("@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\nHost key verification failed.");
        assert!(host.hint.unwrap().contains("known_hosts"));
        assert!(refusal("kex_exchange_identification: read: Connection reset by peer").hint.is_none());
    }
}

//! The keys an install imports for "Allow SSH from this computer": Orchard's
//! own key for this computer (made once, in orchard2's cache under
//! private/ssh, as orchard's computer_key.py) and the user's public keys, so
//! plain `ssh USER@172.16.42.1` works over the USB cable. Each as
//! "TYPE BASE64", the form the install tools accept (no comment).
use orchard_cache::{Code, Error, Result};
use std::path::{Path, PathBuf};

const TYPES: [&str; 5] = ["ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521", "ssh-rsa"];

/// "TYPE BASE64" from a public key line, or None when it isn't one the
/// install tools take.
pub fn key_line(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let (kind, data) = (parts.next()?, parts.next()?);
    let ok = TYPES.contains(&kind)
        && (60..=1024).contains(&data.len())
        && data.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=');
    ok.then(|| format!("{kind} {data}"))
}

/// Orchard's private key for this computer, made once.
fn identity(cache: &Path) -> Result<PathBuf> {
    let dir = cache.join("private/ssh");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(cache.join("private"), std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let key = dir.join("id_ed25519");
    if !key.is_file() {
        let status = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "orchard", "-f"])
            .arg(&key)
            .status()
            .map_err(|e| Error::new(Code::NotFound, format!("ssh-keygen is needed for SSH from this computer: {e}")))?;
        if !status.success() {
            return Err(Error::new(Code::Io, "ssh-keygen couldn't make this computer's key"));
        }
    }
    Ok(key)
}

/// An Orchard system's id for the unit with this serial number, as the
/// system itself makes it for its USB serial (orchard's
/// boot/session/usb-identity): the first 12 hex digits of
/// SHA-256("orchard-SERIAL").
pub fn unit(serial_number: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("orchard-{serial_number}").as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// After an install that lets this computer sign in: remembers the user for
/// `orchard2 shell` and its kin (`private/ssh/user`, and `user-UNIT` where
/// the unit is known), and forgets the host key kept for that unit, since
/// the new system made a new one.
pub fn installed(cache: &Path, user: &str, unit: Option<&str>) -> Result<()> {
    let directory = identity(cache)?.with_file_name("");
    let private = |path: PathBuf, text: String| -> Result<()> {
        std::fs::write(&path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    };
    private(directory.join("user"), format!("{user}\n"))?;
    let Some(unit) = unit else { return Ok(()) };
    private(directory.join(format!("user-{unit}")), format!("{user}\n"))?;
    let hosts = directory.join("known_hosts");
    if let Ok(known) = std::fs::read_to_string(&hosts) {
        // Its line starts with the name the key is kept under.
        let name = format!("orchard-{unit}");
        let kept: String = known
            .lines()
            .filter(|line| line.split([' ', ',']).next() != Some(name.as_str()))
            .map(|line| format!("{line}\n"))
            .collect();
        if kept != known {
            private(hosts, kept)?;
        }
    }
    Ok(())
}

/// The keys to import: Orchard's own, then the user's, each once.
pub fn keys(cache: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut add = |path: &Path| {
        if let Ok(text) = std::fs::read_to_string(path) {
            for key in text.lines().filter_map(key_line) {
                if !out.contains(&key) {
                    out.push(key);
                }
            }
        }
    };
    add(&identity(cache)?.with_extension("pub"));
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for name in ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"] {
            add(&home.join(".ssh").join(name));
        }
    }
    if out.is_empty() {
        return Err(Error::new(Code::NotFound, "this computer has no SSH key to import"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_install_remembers_its_user_and_forgets_the_units_old_host_key() {
        // What usb-identity prints for this serial number: sha256("orchard-F4KXY12ZAB")[..12].
        assert_eq!(unit("F4KXY12ZAB"), {
            use sha2::{Digest, Sha256};
            let d = Sha256::digest(b"orchard-F4KXY12ZAB");
            d.iter().take(6).map(|b| format!("{b:02x}")).collect::<String>()
        });
        assert_eq!(unit("F4KXY12ZAB").len(), 12);
        if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
            return;
        }
        let cache = std::env::temp_dir().join(format!("orchard-ssh-installed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let directory = cache.join("private/ssh");
        std::fs::create_dir_all(&directory).unwrap();
        let known = "orchard-0123456789ab ssh-ed25519 OLD\norchard-ffffffffffff ssh-ed25519 OTHER\n10.0.0.5 ssh-ed25519 NET\n";
        std::fs::write(directory.join("known_hosts"), known).unwrap();
        installed(&cache, "tablet", Some("0123456789ab")).unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("user")).unwrap(), "tablet\n");
        assert_eq!(std::fs::read_to_string(directory.join("user-0123456789ab")).unwrap(), "tablet\n");
        let left = std::fs::read_to_string(directory.join("known_hosts")).unwrap();
        assert_eq!(left, "orchard-ffffffffffff ssh-ed25519 OTHER\n10.0.0.5 ssh-ed25519 NET\n");
        // Without the unit only the last user is remembered, and nothing is forgotten.
        installed(&cache, "other", None).unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("user")).unwrap(), "other\n");
        assert_eq!(std::fs::read_to_string(directory.join("known_hosts")).unwrap(), left);
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn key_lines_lose_their_comment_and_odd_ones_are_left_out() {
        let data = "A".repeat(68);
        assert_eq!(key_line(&format!("ssh-ed25519 {data} user@host")), Some(format!("ssh-ed25519 {data}")));
        assert_eq!(key_line(&format!("ssh-dss {data}")), None);
        assert_eq!(key_line("ssh-ed25519 short"), None);
        assert_eq!(key_line(&format!("ssh-rsa {data}; rm -rf /")), None);
    }

    #[test]
    fn this_computer_gets_a_key_once_and_it_is_imported_first() {
        let cache = std::env::temp_dir().join(format!("orchard-ssh-keys-{}", std::process::id()));
        let first = keys(&cache).unwrap();
        assert!(first[0].starts_with("ssh-ed25519 ") && !first[0].contains("orchard"));
        assert_eq!(keys(&cache).unwrap()[0], first[0], "the key is made once");
        std::fs::remove_dir_all(&cache).unwrap();
    }
}

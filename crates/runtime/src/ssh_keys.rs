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

/// Remembers the user an install let this computer sign in as, for
/// `orchard2 shell` and its kin (`private/ssh/user`, as orchard's
/// computer_key.py).
pub fn remember_user(cache: &Path, user: &str) -> Result<()> {
    let key = identity(cache)?;
    let path = key.with_file_name("user");
    std::fs::write(&path, format!("{user}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
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

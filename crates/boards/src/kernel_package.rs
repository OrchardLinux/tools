//! The installed system's kernel package (orchard's `kernel_packages.py`):
//! the published `linux-orchard-PLATFORM` that wraps exactly the release's
//! platform layer, so the system owns its kernel through its package manager
//! and updates it with its ordinary updates. Without it the device installs
//! the layer's files, and the kernel stops updating.
//!
//! pacman (Arch Linux ARM) only for now; apk (Alpine) follows when a release
//! needs it. A layer no published package wraps (a nightly, a local build)
//! gets no package, never a different kernel.
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::{Code, Error, Result};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

const REPOSITORY: &str = "https://github.com/OrchardLinux/packages/releases/download";
const PIN: &str = "https://raw.githubusercontent.com/OrchardLinux/packages/main/packages/linux-orchard-{platform}/source.json";
const TIMEOUT: Duration = Duration::from_secs(60);

/// A package chosen for a layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub base: String,
    pub file: String,
    pub version: String,
    pub sha256: String,
    pub bytes: u64,
    pub url: String,
}

fn gunzip_or_plain(data: &[u8]) -> Result<Vec<u8>> {
    if data.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(data).take(256 << 20).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(data.to_vec())
    }
}

/// (platform, kernel architecture) from the layer's `platform.json`.
pub fn layer_platform(layer_gz: &[u8]) -> Result<(String, String)> {
    let tar = gunzip_or_plain(layer_gz)?;
    let archive = orchard_formats::tar::read(&tar)?;
    let file = archive
        .files
        .iter()
        .find(|f| f.path.trim_start_matches("./") == "usr/lib/orchard/platform.json")
        .ok_or_else(|| Error::new(Code::NotFound, "the platform layer has no usr/lib/orchard/platform.json"))?;
    let doc: Value = serde_json::from_slice(file.data).map_err(|_| Error::new(Code::Format, "platform.json is not JSON"))?;
    let platform = doc["platform"].as_str().ok_or_else(|| Error::new(Code::Format, "platform.json names no platform"))?;
    Ok((platform.to_owned(), doc["architecture"].as_str().unwrap_or("arm64").to_owned()))
}

/// {file, version, sha256, compressed size} of `name` in a pacman database.
pub fn pacman_entry(db: &[u8], name: &str) -> Result<Option<(String, String, String, u64)>> {
    let tar = gunzip_or_plain(db)?;
    for file in orchard_formats::tar::read(&tar)?.files {
        if !file.path.ends_with("/desc") {
            continue;
        }
        let text = String::from_utf8_lossy(file.data);
        let lines: Vec<&str> = text.split('\n').collect();
        let value = |key: &str| lines.iter().position(|l| *l == key).and_then(|i| lines.get(i + 1)).map(|s| s.to_string());
        if value("%NAME%").as_deref() == Some(name) {
            let size = value("%CSIZE%").and_then(|c| c.trim().parse().ok());
            if let (Some(f), Some(v), Some(s), Some(n)) = (value("%FILENAME%"), value("%VERSION%"), value("%SHA256SUM%"), size) {
                return Ok(Some((f, v, s, n)));
            }
        }
    }
    Ok(None)
}

/// Whether a published package may stand in for this layer: its pin names
/// the layer's SHA-256 and its version is the pin's orchard release.
pub fn pinned(pin: &Value, layer_sha256: &str, version: &str) -> bool {
    let orchard = pin.get("orchard").and_then(Value::as_str).unwrap_or("\u{0}");
    pin.get("sha256").and_then(Value::as_str) == Some(layer_sha256) && version.starts_with(&format!("{orchard}-"))
}

/// A plain file name the receiver accepts for `Put`.
fn stageable(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn get(url: &str) -> Result<Vec<u8>> {
    let mut http = Http::probe(url, TIMEOUT)?;
    let size = http.size() as usize;
    http.fetch(0, size)
}

/// A small file, or None when the server says it is not there (HTTP 404).
fn get_published(url: &str) -> Result<Option<Vec<u8>>> {
    match get(url) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.code == Code::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The package for a layer, or `None` when none is published for exactly it
/// (HTTP 404, or a pin that doesn't name this layer). A repository that
/// can't be reached is an error like any other asset's, never `None`.
pub fn resolve(base: &str, layer_gz: &[u8], layer_sha256: &str) -> Result<Option<Package>> {
    if base != "pacman" {
        return Ok(None);
    }
    let (platform, architecture) = layer_platform(layer_gz)?;
    let arch = match architecture.as_str() {
        "arm64" => "aarch64",
        "arm" => "armv7h",
        _ => return Ok(None),
    };
    let name = format!("linux-orchard-{platform}");
    let Some(db) = get_published(&format!("{REPOSITORY}/repo-{arch}/orchard.db"))? else { return Ok(None) };
    let Some((file, version, sha256, bytes)) = pacman_entry(&db, &name)? else { return Ok(None) };
    if !stageable(&file) {
        return Ok(None);
    }
    let Some(pin) = get_published(&PIN.replace("{platform}", &platform))? else { return Ok(None) };
    let pin: Value =
        serde_json::from_slice(&pin).map_err(|_| Error::new(Code::Format, "the kernel package's pin is not JSON"))?;
    if !pinned(&pin, layer_sha256, &version) {
        return Ok(None);
    }
    Ok(Some(Package {
        base: base.to_owned(),
        url: format!("{REPOSITORY}/repo-{arch}/{file}"),
        file,
        version,
        sha256: sha256.to_ascii_lowercase(),
        bytes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_package_stands_in_only_for_its_pinned_layer() {
        let pin = json!({"sha256": "aaa", "orchard": "0.3.2"});
        assert!(pinned(&pin, "aaa", "0.3.2-1"));
        assert!(!pinned(&pin, "bbb", "0.3.2-1"), "another layer");
        assert!(!pinned(&pin, "aaa", "0.3.1-1"), "another release");
        assert!(!pinned(&json!({}), "aaa", "0.3.2-1"));
    }

    #[test]
    fn only_plain_names_are_staged() {
        assert!(stageable("linux-orchard-apple-16k-0.3.2-1-aarch64.pkg.tar.zst"));
        assert!(!stageable("a+b.pkg"));
        assert!(!stageable("../x"));
    }

    #[test]
    fn pacman_desc_entries_are_found_by_name() {
        let desc = b"%CSIZE%\n42\n\n%FILENAME%\nlinux-orchard-apple-16k-0.3.2-1-aarch64.pkg.tar.zst\n\n%NAME%\nlinux-orchard-apple-16k\n\n%VERSION%\n0.3.2-1\n\n%SHA256SUMS%\nx\n\n%SHA256SUM%\nABC\n";
        let tar = orchard_formats::tar::write(&[orchard_formats::tar::Entry {
            path: "linux-orchard-apple-16k-0.3.2-1/desc",
            mode: 0o644,
            data: desc,
        }])
        .unwrap();
        let (file, version, sha, size) = pacman_entry(&tar, "linux-orchard-apple-16k").unwrap().unwrap();
        assert_eq!(size, 42);
        assert_eq!((file.as_str(), version.as_str(), sha.as_str()), ("linux-orchard-apple-16k-0.3.2-1-aarch64.pkg.tar.zst", "0.3.2-1", "ABC"));
        assert!(pacman_entry(&tar, "linux-orchard-apple-4k").unwrap().is_none());
    }
}

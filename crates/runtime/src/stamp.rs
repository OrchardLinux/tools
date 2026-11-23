//! What a distro's installer installs, stamped onto the generic binary by the
//! distro's release step (packaging/stamp.py): its name and wordmark, the
//! release index, and the launcher it puts on the computer after an install.
//! Nothing about a distro is compiled in.
//!
//! Linux: at the very end of the executable, the JSON, its length (u64 little
//! endian), then the 16-byte magic. macOS: the JSON in the app bundle's
//! Contents/Resources/stamp.json (the bundle is re-signed after stamping).
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const MAGIC: &[u8; 16] = b"ORCHARD-STAMP-1\0";

/// A file pinned by URL, size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
    /// The file's version (the launcher's: an installed newer one stays).
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The system's name, as the windows and the iPad say it ("Omarchy A").
    pub name: String,
    /// Its wordmark (SVG), drawn on the device while it starts.
    #[serde(default)]
    pub wordmark: Option<String>,
    /// The distro's release index (index.toml).
    pub index: String,
    /// The launcher per host (`macos`, `linux-x86_64`, `linux-aarch64`).
    #[serde(default)]
    pub launcher: BTreeMap<String, Pin>,
}

impl Stamp {
    /// The launcher for this computer, if the stamp has one.
    pub fn launcher_here(&self) -> Option<&Pin> {
        self.launcher.get(&host())
    }
}

/// This computer's key in [`Stamp::launcher`].
pub fn host() -> String {
    if cfg!(target_os = "macos") {
        "macos".into()
    } else {
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// The stamp at the end of `path`, if there is one.
pub fn read_from(path: &Path) -> Option<Stamp> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.seek(SeekFrom::End(0)).ok()?;
    if len < 24 {
        return None;
    }
    let mut tail = [0u8; 24];
    file.seek(SeekFrom::End(-24)).ok()?;
    file.read_exact(&mut tail).ok()?;
    if &tail[8..] != MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(tail[..8].try_into().ok()?);
    if size == 0 || size > 64 * 1024 || size + 24 > len {
        return None;
    }
    let mut json = vec![0u8; size as usize];
    file.seek(SeekFrom::End(-24 - size as i64)).ok()?;
    file.read_exact(&mut json).ok()?;
    serde_json::from_slice(&json).ok()
}

/// The running executable's stamp.
pub fn read() -> Option<Stamp> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        // …/X.app/Contents/MacOS/exe → …/X.app/Contents/Resources/stamp.json
        let json = exe.parent()?.parent()?.join("Resources/stamp.json");
        if let Ok(bytes) = std::fs::read(json) {
            return serde_json::from_slice(&bytes).ok();
        }
    }
    read_from(&exe)
}

/// The bytes to append to stamp a binary (packaging, tests).
pub fn trailer(stamp: &Stamp) -> Vec<u8> {
    let json = serde_json::to_vec(stamp).expect("stamp");
    let mut out = json.clone();
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    out.extend_from_slice(MAGIC);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_reads_back_and_an_unstamped_file_has_none() {
        let dir = std::env::temp_dir().join(format!("orchard-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bin");
        std::fs::write(&file, b"\x7fELF not really a binary").unwrap();
        assert_eq!(read_from(&file), None);
        let mut launcher = BTreeMap::new();
        launcher.insert("macos".into(), Pin { url: "https://x/Orchard-macOS.zip".into(), bytes: 3, sha256: "ab".repeat(32), version: Some("0.1.0".into()) });
        let stamp = Stamp { name: "Omarchy A".into(), wordmark: Some("<svg/>".into()), index: "https://x/index.toml".into(), launcher };
        let mut bytes = std::fs::read(&file).unwrap();
        bytes.extend(trailer(&stamp));
        std::fs::write(&file, bytes).unwrap();
        assert_eq!(read_from(&file), Some(stamp));
    }
}

//! Taking a tool's executable out of its release package, in-process: a
//! UDIF disk image with an HFS+ volume (macOS) or a gzipped tarball (Linux).
use orchard_boards::data::disk_image::DiskImage;
use orchard_cache::source::{FileSource, Source};
use orchard_cache::{Code, Error, Result};
use orchard_flow::assets::{Package, Pin};
use orchard_formats::hfs::{Disk, Volume};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest executable accepted out of a package.
const MAX_EXECUTABLE: u64 = 256 << 20;

struct Adapter<S>(S);

impl<S: Source> Disk for Adapter<S> {
    fn size(&self) -> u64 {
        self.0.size()
    }
    fn read(&mut self, offset: u64, len: usize) -> orchard_formats::Result<Vec<u8>> {
        self.0.read_at(offset, len).map_err(|_| orchard_formats::Error("disk image read failed"))
    }
}

/// The executable's bytes from a verified package file.
pub fn executable(package: &Path, pin: &Pin) -> Result<Vec<u8>> {
    if pin.package == Package::Bare {
        // The release file is the executable; its download was checked.
        return Ok(std::fs::read(package)?);
    }
    let member = pin.member.ok_or_else(|| Error::new(Code::NotFound, "the pin names no executable"))?;
    let bytes = match pin.package {
        Package::Bare => unreachable!(),
        Package::Dmg => {
            let image = DiskImage::open(FileSource::open(package)?)?;
            let part = image
                .partitions()
                .iter()
                .find(|p| p.name.contains("Apple_HFS"))
                .cloned()
                .ok_or_else(|| Error::new(Code::Format, "the disk image has no HFS+ volume"))?;
            let mut volume = Volume::open(Adapter(image), part.first_sector * orchard_formats::udif::SECTOR)?;
            volume.read(member)?
        }
        Package::TarGz => {
            let file = std::fs::File::open(package)?;
            let mut tar = Vec::new();
            flate2::read::GzDecoder::new(file).take(MAX_EXECUTABLE * 2).read_to_end(&mut tar)?;
            let archive = orchard_formats::tar::read(&tar)?;
            archive
                .files
                .iter()
                .find(|f| f.path.trim_start_matches("./") == member)
                .map(|f| f.data.to_vec())
                .ok_or_else(|| Error::new(Code::Format, format!("{member} is not in the package")))?
        }
    };
    if bytes.len() as u64 > MAX_EXECUTABLE {
        return Err(Error::new(Code::Format, "the executable is implausibly large"));
    }
    if let Some(expected) = pin.executable_sha256 {
        let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if actual != expected {
            return Err(Error::new(Code::Format, format!("executable SHA-256 is {actual}, expected {expected}")));
        }
    }
    Ok(bytes)
}

/// Unpacks the executable next to the cached package, marked executable,
/// and returns its path. An executable already unpacked is reused.
pub fn install(package: &Path, pin: &Pin, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    // Named after the tool: the release file's name up to its host part.
    let tool = pin.asset.split('-').next().unwrap_or("tool");
    let path = dir.join(format!("{tool}-{}", &pin.sha256[..16]));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = executable(package, pin)?;
    let partial = path.with_extension("partial");
    std::fs::write(&partial, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_flow::assets::palera1n_pin;

    /// Runs against the release files when they are next to the workspace
    /// (installer-app/), which is how they were checked by hand.
    fn local(name: &str) -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../installer-app").join(name);
        p.is_file().then_some(p)
    }

    #[test]
    fn a_bare_tool_is_installed_under_its_own_name() {
        let dir = std::env::temp_dir().join(format!("orchard-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let package = dir.join("download");
        std::fs::write(&package, b"#!/bin/sh\n").unwrap();
        for host in [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "aarch64"), ("linux", "x86_64")] {
            let pin = orchard_flow::assets::checkra1n_pin(host.0, host.1).unwrap();
            let path = install(&package, pin, &dir.join("tools")).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(name, format!("checkra1n-{}", &pin.sha256[..16]));
        }
        let pin = palera1n_pin("linux", "x86_64").unwrap();
        assert_eq!(pin.asset.split('-').next(), Some("palera1n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn macos_dmg_yields_the_pinned_executable() {
        let Some(dmg) = local("palera1n-macos-universal.dmg") else { return };
        let bytes = executable(&dmg, palera1n_pin("macos", "aarch64").unwrap()).unwrap();
        assert_eq!(&bytes[..4], &[0xca, 0xfe, 0xba, 0xbe]);
    }

    #[test]
    fn linux_tarball_yields_the_pinned_executable() {
        let Some(tgz) = local("palera1n-linux-arm64.tar.gz") else { return };
        let bytes = executable(&tgz, palera1n_pin("linux", "aarch64").unwrap()).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF");
    }

    #[test]
    fn linux_x86_64_tarball_yields_the_pinned_executable() {
        let Some(tgz) = local("palera1n-linux-x86_64.tar.gz") else { return };
        let bytes = executable(&tgz, palera1n_pin("linux", "x86_64").unwrap()).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF");
    }
}

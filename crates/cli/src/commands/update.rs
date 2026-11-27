//! `update`: replaces this program with its newest release, the one
//! `install.sh` installs: the file for this computer, checked against the
//! release's checksums.
use crate::args::Global;
use crate::output::{Output, Report};
use orchard_build::published;
use orchard_cache::store;
use orchard_cache::{Code, Context, Error, Result};
use serde::Serialize;
use std::path::Path;

/// The newest release's files: `install.sh` reads the same.
const BUILDS: &str = "https://github.com/OrchardLinux/tools/releases/latest/download";
/// The commit this program was built from, in a published build.
const BUILT_FROM: Option<&str> = option_env!("ORCHARD_BUILD");

/// The published file for this computer, as `install.sh` names it.
fn asset() -> Option<String> {
    let name = env!("CARGO_BIN_NAME");
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => Some(format!("{name}-macos")),
        ("linux", arch @ ("x86_64" | "aarch64")) => Some(format!("{name}-linux-{arch}")),
        _ => None,
    }
}

#[derive(Serialize)]
struct Updated {
    /// The program that was replaced, or would have been.
    path: String,
    /// The commit it was built from, where a published build says.
    from: Option<&'static str>,
    /// The commit the newest release was built from.
    to: String,
    /// The newest release's version.
    version: String,
    /// False: it was the newest already.
    changed: bool,
}

impl Report for Updated {
    fn human(&self) -> Vec<u8> {
        match self.changed {
            true => format!("{} is now orchard {}\n", self.path, self.version),
            false => format!("{} is the newest release already ({})\n", self.path, self.version),
        }
        .into_bytes()
    }
}

/// Whether `path` is a build in a source checkout, which is rebuilt, not
/// replaced.
fn built_here(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == "target")
}

pub fn update(_: &Global) -> Result<Output> {
    let asset = asset().ok_or_else(|| Error::new(Code::Unsupported, "no release is published for this computer"))?;
    let path = std::env::current_exe()?.canonicalize()?;
    if built_here(&path) {
        return Err(Error::new(Code::Refused, format!("{} was built in a source checkout", path.display())).hint("build it again with cargo; update replaces an installed program"));
    }
    let to = String::from_utf8_lossy(&published::fetch(&format!("{BUILDS}/BUILD"), 1 << 10)?).trim().to_owned();
    let version = String::from_utf8_lossy(&published::fetch(&format!("{BUILDS}/VERSION"), 1 << 10)?).trim().to_owned();
    if BUILT_FROM == Some(to.as_str()) {
        return Ok(Updated { path: path.display().to_string(), from: BUILT_FROM, to, version, changed: false }.into());
    }
    let sums = published::fetch(&format!("{BUILDS}/SHA256SUMS"), 64 << 10)?;
    let sums = published::sums(&String::from_utf8_lossy(&sums));
    let expected = sums.get(&asset).ok_or_else(|| Error::new(Code::NotFound, format!("the newest release has no {asset}")))?;
    let bytes = published::fetch(&format!("{BUILDS}/{asset}"), 64 << 20)?;
    if &store::sha256(&bytes) != expected {
        return Err(Error::new(Code::Format, format!("{asset} does not match the release's checksum")));
    }
    // Beside the program, so that taking its place is one rename.
    let partial = path.with_extension(format!("new-{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        std::fs::write(&partial, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&partial, &path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written.context(path.display())?;
    Ok(Updated { path: path.display().to_string(), from: BUILT_FROM, to, version, changed: true }.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_in_a_checkout_is_not_replaced() {
        assert!(built_here(Path::new("/home/a/tools/target/release/orchard")));
        assert!(!built_here(Path::new("/home/a/.local/bin/orchard")));
    }

    #[test]
    fn this_computer_has_a_published_file() {
        let asset = asset().unwrap();
        assert!(asset.starts_with(concat!(env!("CARGO_BIN_NAME"), "-")), "{asset}");
    }
}

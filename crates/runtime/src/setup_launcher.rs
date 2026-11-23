//! After an install: puts Orchard, the launcher, on this computer from the
//! asset the installer's stamp pins (stamp.rs), so the installed system can be
//! started again later. macOS: ~/Applications/Orchard.app (the asset is a zip
//! of it). Linux: ~/.local/bin/orchard with its desktop entry and icons (the
//! asset is a tar.gz with `orchard`, `applications/` and `icons/`).
//!
//! Orchard is shared by every distro's installer: an installed Orchard that is
//! as new or newer than the pinned one stays (never a downgrade).
use crate::exec::Executor;
use crate::release::Receipt;
use crate::stamp::Pin;
use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "no home folder".to_owned())
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(|e| e.to_string())?;
    status.success().then_some(()).ok_or_else(|| format!("{command:?} failed ({status})"))
}

/// "1.2.3" as numbers, for comparing versions.
fn parse_version(v: &str) -> Vec<u64> {
    v.trim().split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)).collect()
}

/// The installed Orchard's version, read without starting it: the bundle's
/// Info.plist on macOS, the version file the install leaves on Linux.
fn installed_version(home: &Path) -> Option<String> {
    if cfg!(target_os = "macos") {
        let plist = std::fs::read_to_string(home.join("Applications/Orchard.app/Contents/Info.plist")).ok()?;
        let at = plist.find("<key>CFBundleShortVersionString</key>")?;
        let rest = &plist[at..];
        let start = rest.find("<string>")? + "<string>".len();
        let end = rest.find("</string>")?;
        Some(rest[start..end].to_owned())
    } else {
        home.join(".local/bin/orchard").is_file().then_some(())?;
        std::fs::read_to_string(linux_version_file(home)).ok().map(|v| v.trim().to_owned())
    }
}

fn linux_version_file(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").filter(|_| !cfg!(test)).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("orchard/version")
}

/// Whether the installed Orchard is at least as new as the pinned one.
fn current_enough(installed: Option<&str>, pinned: Option<&str>) -> bool {
    match (installed, pinned) {
        (Some(i), Some(p)) => parse_version(i) >= parse_version(p),
        _ => false,
    }
}

/// Downloads, checks and puts the launcher in place; where it is now.
pub fn install(executor: &Executor, pin: &Pin) -> Result<String, String> {
    let home = home()?;
    if current_enough(installed_version(&home).as_deref(), pin.version.as_deref()) {
        return Ok(if cfg!(target_os = "macos") { "~/Applications/Orchard.app".into() } else { "Orchard, in your app launcher".into() });
    }
    let receipt = Receipt { url: pin.url.clone(), bytes: pin.bytes, sha256: pin.sha256.clone(), expanded_bytes: None };
    let asset = executor.download(&receipt).map_err(|e| e.to_string())?;
    if cfg!(target_os = "macos") {
        install_macos(&asset, &home)
    } else {
        let at = install_linux(&asset, &home)?;
        if let Some(v) = &pin.version {
            let file = linux_version_file(&home);
            std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(file, v).map_err(|e| e.to_string())?;
        }
        Ok(at)
    }
}

fn install_macos(asset: &Path, home: &Path) -> Result<String, String> {
    let apps = home.join("Applications");
    std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let app = apps.join("Orchard.app");
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| e.to_string())?;
    }
    run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(asset).arg(&apps))?;
    if !app.join("Contents/MacOS/orchard").is_file() {
        return Err("the launcher's archive has no Orchard.app".into());
    }
    Ok("~/Applications/Orchard.app".into())
}

fn install_linux(asset: &Path, home: &Path) -> Result<String, String> {
    let unpacked = std::env::temp_dir().join(format!("orchard-launcher-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    run(Command::new("tar").arg("-xzf").arg(asset).arg("-C").arg(&unpacked))?;
    let bin = home.join(".local/bin");
    let share = std::env::var_os("XDG_DATA_HOME").filter(|_| !cfg!(test)).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    for dir in [&bin, &share.join("applications"), &share.join("icons")] {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // A fresh file, not an overwrite in place: a running launcher keeps working.
    let target = bin.join("orchard");
    let fresh = bin.join(".orchard.new");
    std::fs::copy(unpacked.join("orchard"), &fresh).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&fresh, &target).map_err(|e| e.to_string())?;
    // The menu entry names the launcher by its full path (~/.local/bin may not be on PATH).
    let entry = std::fs::read_to_string(unpacked.join("applications/orchard.desktop")).map_err(|e| e.to_string())?;
    let entry: String = entry
        .lines()
        .map(|l| if l.starts_with("Exec=") { format!("Exec={}\n", target.display()) } else { format!("{l}\n") })
        .collect();
    std::fs::write(share.join("applications/orchard.desktop"), entry).map_err(|e| e.to_string())?;
    run(Command::new("cp").arg("-R").arg(unpacked.join("icons/hicolor")).arg(share.join("icons")))?;
    let _ = Command::new("update-desktop-database").arg(share.join("applications")).status();
    let _ = std::fs::remove_dir_all(&unpacked);
    Ok("Orchard, in your app launcher".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_newer_or_equal_orchard_stays() {
        use super::current_enough as ok;
        assert!(ok(Some("0.2.0"), Some("0.1.9")));
        assert!(ok(Some("0.1.0"), Some("0.1.0")));
        assert!(ok(Some("0.10.0"), Some("0.9.3")));
        assert!(!ok(Some("0.1.0"), Some("0.1.1")));
        assert!(!ok(None, Some("0.1.0")));
        assert!(!ok(Some("0.1.0"), None));
    }

    /// The launcher's macOS zip (packaging/build-macos.sh) unpacks to a working app.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_launcher_goes_in_place() {
        let asset = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/generic/Orchard-macOS.zip");
        if !asset.exists() {
            return;
        }
        let home = std::env::temp_dir().join(format!("orchard-launcher-mac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // Twice: a second install replaces the first.
        for _ in 0..2 {
            super::install_macos(&asset, &home).unwrap();
        }
        let app = home.join("Applications/Orchard.app");
        assert!(std::process::Command::new("codesign").arg("--verify").arg("--strict").arg(&app).status().unwrap().success());
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// The launcher's Linux archive (packaging/package-linux.sh) goes in place.
    #[test]
    fn linux_launcher_goes_in_place() {
        let asset = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/generic/Orchard-linux-aarch64.tar.gz");
        if !asset.exists() {
            return; // packaging not built
        }
        let home = std::env::temp_dir().join(format!("orchard-launcher-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let at = super::install_linux(&asset, &home).unwrap();
        assert!(at.contains("Orchard"));
        assert!(home.join(".local/bin/orchard").is_file());
        let entry = std::fs::read_to_string(home.join(".local/share/applications/orchard.desktop")).unwrap();
        assert!(entry.contains(&format!("Exec={}", home.join(".local/bin/orchard").display())));
        assert!(home.join(".local/share/icons/hicolor/256x256/apps/orchard.png").is_file());
        std::fs::remove_dir_all(&home).unwrap();
    }
}

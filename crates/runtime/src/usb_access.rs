//! Linux: lets the user's own account reach the device on USB, once per
//! computer: a udev rule for every stage orchard and the windows talk to
//! (DFU, recovery, Pongo, m1n1, and Orchard's own systems with their serial
//! consoles), plus write access to the port's `disable` control of a device
//! in DFU, for the USB port reset after checkm8 (replug.rs).
//!
//! Installed with pkexec (a graphical password prompt) when there is a
//! desktop session, else sudo. macOS needs none of this.
//!
//! UNTESTED on Linux hardware so far (written on macOS); the rule covers
//! what orchard's Python CLI's does.
use orchard_cache::{Code, Error, Result};
use std::path::Path;
use std::process::Command;

pub const RULES: &str = "/etc/udev/rules.d/70-orchard-installer.rules";
const APPLE: u16 = 0x05ac;
/// DFU, recovery, Pongo, and Orchard's systems: recovery, RAM, installed,
/// install.
const PRODUCTS: [u16; 7] = [0x1227, 0x1281, 0x4141, 0x4142, 0x4144, 0x4145, 0x4146];
/// Orchard's systems, which have a serial console.
const WITH_CONSOLE: [u16; 4] = [0x4142, 0x4144, 0x4145, 0x4146];
/// m1n1's proxy, which is a serial port too.
const M1N1: (u16, u16) = (0x1209, 0x316d);
const DFU: u16 = 0x1227;

/// The rule file for this group.
pub fn rules(group: &str) -> Result<String> {
    if group.is_empty() || !group.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err(Error::new(Code::InvalidInput, format!("unsupported group name: {group}")));
    }
    let access = format!("MODE=\"0660\", GROUP=\"{group}\", TAG+=\"uaccess\"");
    let mut lines = vec!["# Orchard: the device's USB stages and m1n1, with their serial consoles, for the logged-in user.".to_owned()];
    let devices = PRODUCTS.iter().map(|product| (APPLE, *product, WITH_CONSOLE.contains(product))).chain([(M1N1.0, M1N1.1, true)]);
    for (vendor, product, console) in devices {
        lines.push(format!("SUBSYSTEM==\"usb\", ATTR{{idVendor}}==\"{vendor:04x}\", ATTR{{idProduct}}==\"{product:04x}\", {access}"));
        if console {
            lines.push(format!("SUBSYSTEM==\"tty\", ATTRS{{idVendor}}==\"{vendor:04x}\", ATTRS{{idProduct}}==\"{product:04x}\", {access}"));
        }
    }
    lines.push("# Lets orchard reset the port of a device in DFU that doesn't come back after checkm8.".to_owned());
    lines.push(format!(
        "ACTION==\"add\", SUBSYSTEM==\"usb\", ENV{{DEVTYPE}}==\"usb_device\", ATTR{{idVendor}}==\"{APPLE:04x}\", \
         ATTR{{idProduct}}==\"{DFU:04x}\", RUN+=\"/bin/sh -c 'chgrp {group} /sys%p/port/disable && chmod g+w /sys%p/port/disable'\""
    ));
    Ok(lines.join("\n") + "\n")
}

/// The user's primary group.
fn group() -> Result<String> {
    let out = Command::new("id").arg("-gn").output()?;
    if !out.status.success() {
        return Err(Error::new(Code::NotFound, "cannot tell the user's group"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Whether the rule still has to be installed: only on Linux, and only
/// when the file isn't there exactly as this installer writes it.
pub fn needed() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    match (group().and_then(|g| rules(&g)), std::fs::read_to_string(RULES)) {
        (Ok(want), Ok(have)) => want != have,
        _ => true,
    }
}

/// The command that installs `file` as the rule and makes udev apply it to
/// devices already plugged in.
fn script(file: &Path) -> String {
    format!(
        "install -m 0644 '{}' '{RULES}' && udevadm control --reload-rules && \
         udevadm trigger --action=add --subsystem-match=usb --attr-match=idVendor={APPLE:04x}",
        file.display()
    )
}

/// Installs the rule, asking for the administrator password: pkexec in a
/// desktop session, else sudo in the terminal.
pub fn request() -> Result<()> {
    let text = rules(&group()?)?;
    let file = std::env::temp_dir().join(format!("orchard-installer-{}.rules", std::process::id()));
    std::fs::write(&file, text)?;
    let desktop = std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let has = |tool: &str| Command::new("sh").args(["-c", &format!("command -v {tool}")]).output().is_ok_and(|o| o.status.success());
    let elevate = if desktop && has("pkexec") {
        "pkexec"
    } else if has("sudo") {
        "sudo"
    } else {
        return Err(Error::new(Code::NotFound, "USB access needs pkexec or sudo"));
    };
    let status = Command::new(elevate).args(["/bin/sh", "-c", &script(&file)]).status();
    let _ = std::fs::remove_file(&file);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Error::new(Code::Usb, format!("installing the USB rule failed or was cancelled ({s})"))),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_covers_every_stage_and_the_port_reset() {
        let r = rules("staff").unwrap();
        for product in ["1227", "1281", "4141", "4142", "4144", "4145", "4146"] {
            assert!(r.contains(&format!("ATTR{{idProduct}}==\"{product}\", MODE=\"0660\", GROUP=\"staff\", TAG+=\"uaccess\"")), "{product}");
        }
        assert!(r.contains("chgrp staff /sys%p/port/disable && chmod g+w /sys%p/port/disable"));
        // m1n1's proxy and the systems' consoles are serial ports as well.
        assert!(r.contains("SUBSYSTEM==\"usb\", ATTR{idVendor}==\"1209\", ATTR{idProduct}==\"316d\""));
        assert_eq!(r.matches("SUBSYSTEM==\"tty\"").count(), 5);
        assert!(!r.contains("SUBSYSTEM==\"tty\", ATTRS{idVendor}==\"05ac\", ATTRS{idProduct}==\"1227\""));
        assert!(rules("evil\"; rm").is_err());
        assert!(!cfg!(target_os = "macos") || !needed(), "never needed on macOS");
        assert!(script(Path::new("/tmp/x.rules")).starts_with("install -m 0644 '/tmp/x.rules' '/etc/udev/rules.d/70-orchard-installer.rules'"));
    }
}

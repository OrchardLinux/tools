//! Desktop notifications. The installer keeps no state of its own across
//! launches: what it needs to pick an install up again it asks the device
//! (the helper's running split, the install system's phase).

/// A desktop notification. macOS: Notification Center through osascript.
/// Linux (untested): notify-send when it is installed. Failures are quiet.
pub fn notify(text: &str) {
    use std::process::{Command, Stdio};
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let result = if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args(["-e", &format!("display notification \"{escaped}\" with title \"Orchard Installer\"")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("notify-send").args(["Orchard Installer", text]).stdout(Stdio::null()).stderr(Stdio::null()).status()
    };
    if let Err(e) = result {
        eprintln!("notify: {e}");
    }
}

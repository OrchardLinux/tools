use crate::error::{During, Fault};
use crate::identity::classify_with_serial;
use crate::{Error, Identity, Mode, System};
use nusb::MaybeFuture;
use serde::Serialize;

/// A connected device in a known mode. Nothing has been opened yet.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub mode: Mode,
    /// Which of Orchard's Linux systems, when the device announces one.
    pub system: Option<System>,
    /// The physical port: IOKit location id on macOS, bus and port chain on
    /// Linux. Valid only while the device stays plugged in.
    pub location: String,
    /// The product name the device gives itself.
    pub product: Option<String>,
    #[serde(flatten)]
    pub identity: Identity,
}

/// Lists devices. The host implementation uses USB; tests supply their own.
pub trait Discovery {
    fn list(&mut self) -> Result<Vec<Device>, Error>;
}

/// The real USB host.
pub struct Host;

impl Discovery for Host {
    fn list(&mut self) -> Result<Vec<Device>, Error> {
        list()
    }
}

/// Every connected device in a known mode, sorted by location.
pub fn list() -> Result<Vec<Device>, Error> {
    if let Some(why) = unavailable() {
        return Err(Fault::new(crate::Kind::Unsupported, why).during("listing USB devices"));
    }
    let mut found: Vec<Device> = nusb::list_devices()
        .wait()
        .map_err(Fault::from)
        .during("listing USB devices")?
        .filter_map(|info| {
            let (mode, system) = classify_with_serial(info.vendor_id(), info.product_id(), info.serial_number())?;
            Some(Device {
                mode,
                system,
                location: location(&info),
                product: info.product_string().map(str::to_owned),
                identity: match (mode, info.serial_number()) {
                    (Mode::Normal, Some(serial)) => Identity::from_udid(serial),
                    (_, serial) => serial.map(Identity::parse).unwrap_or_default(),
                },
            })
        })
        .collect();
    found.sort_by(|a, b| a.location.cmp(&b.location));
    Ok(found)
}

/// Why this computer shows no USB devices to any program, if it doesn't.
pub fn unavailable() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let version = std::fs::read_to_string("/proc/version").unwrap_or_default();
    without_usb(std::path::Path::new("/sys/bus/usb/devices").exists(), &version)
}

/// On Linux: the kernel lists USB devices in /sys/bus/usb/devices. WSL has
/// none until Windows hands it one.
fn without_usb(listed: bool, kernel_version: &str) -> Option<String> {
    if listed {
        return None;
    }
    let version = kernel_version.to_ascii_lowercase();
    Some(if version.contains("microsoft") || version.contains("wsl") {
        "this is WSL, which sees no USB device until Windows hands it one. In a Windows terminal, \
         `usbipd list` shows the iPad or iPhone's BUSID and `usbipd attach --wsl --auto-attach \
         --busid BUSID` hands it over and keeps doing so while it changes modes (usbipd-win, from \
         https://github.com/dorssel/usbipd-win)"
            .into()
    } else {
        "this Linux system has no USB support running: /sys/bus/usb/devices is missing".into()
    })
}

#[cfg(test)]
mod tests {
    use super::without_usb;

    #[test]
    fn a_system_without_usb_says_why() {
        assert_eq!(without_usb(true, "Linux version 6.6 microsoft-standard-WSL2"), None);
        assert!(without_usb(false, "Linux version 6.6.87.2-microsoft-standard-WSL2")
            .unwrap()
            .contains("usbipd attach --wsl"));
        assert!(without_usb(false, "Linux version 6.12 (gcc)").unwrap().contains("no USB support"));
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn location(info: &nusb::DeviceInfo) -> String {
    format!("0x{:x}", info.location_id())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn location(info: &nusb::DeviceInfo) -> String {
    let chain: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", info.bus_id(), chain.join("."))
}

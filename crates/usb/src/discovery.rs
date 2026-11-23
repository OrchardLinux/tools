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

#[cfg(target_os = "macos")]
pub(crate) fn location(info: &nusb::DeviceInfo) -> String {
    format!("0x{:x}", info.location_id())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn location(info: &nusb::DeviceInfo) -> String {
    let chain: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", info.bus_id(), chain.join("."))
}

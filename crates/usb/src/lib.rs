//! Talking to a device over USB.
//!
//! Finding devices and reading their identity, and one module per protocol:
//! [`dfu`], [`recovery`], [`pongo`] and [`m1n1`]. Each opens a [`Device`]
//! from discovery, offers one method per protocol action with an explicit
//! timeout, and releases the device when the handle is dropped. Retries,
//! waiting for a device to come back and decisions belong to the layers
//! above.
mod discovery;
mod error;
mod identity;
mod m1n1_bulk;
mod others;
mod serial_port;
mod wire;

pub mod boot_manager;
pub mod dfu;
pub mod install;
pub mod m1n1;
pub mod pongo;
pub mod recovery;

pub use discovery::{list, unavailable, Device, Discovery, Host};
pub use error::{Error, Kind};
pub use identity::{classify, Identity, Mode, System};
pub use serial_port::serial_ports;
pub use others::{apple_device, ecid_key, others, Target};

use serde::Serialize;

/// How a command that may make the device leave its mode ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Sent {
    /// The device accepted the transfer.
    Acknowledged,
    /// The transfer failed. A device that starts something else may be gone
    /// before it answers, so only what enumerates next tells whether the
    /// command ran.
    Unconfirmed,
}

impl Sent {
    /// The name used in output, the same as in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Sent::Acknowledged => "acknowledged",
            Sent::Unconfirmed => "unconfirmed",
        }
    }
}

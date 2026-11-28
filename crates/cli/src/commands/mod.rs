pub mod boards;
pub mod build;
pub mod cache;
pub mod data;
pub mod devices;
pub mod dfu;
pub mod fw;
pub mod host;
pub mod install;
pub mod m1n1;
pub mod pongo;
pub mod recovery;
pub mod remote;
pub mod resize;
pub mod run;
pub mod session;
pub mod term;
pub mod update;

use crate::args::{Global, PayloadArgs};
use crate::output::Report;
use orchard_boards::pack::Linux;
use orchard_cache::{Context, Result};
use orchard_device::session::Session;
use orchard_usb::{Host, Mode, Sent};
use serde::Serialize;
use std::path::Path;

/// A session on the one connected device in `mode` that the device flags
/// choose. It holds the device's lock for the command's one action, or
/// continues the session the command was started in.
fn session(global: &Global, mode: Mode) -> Result<Session<Host>> {
    Session::start(Host, &global.device.selector(Some(mode)), None)
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).context(path.display())
}

/// The files of what m1n1 or Cascadia's loader is to start, as read.
pub struct LinuxFiles {
    kernel: Vec<u8>,
    dtbs: Vec<Vec<u8>>,
    initramfs: Option<Vec<u8>>,
    logo: Option<Vec<u8>>,
}

impl LinuxFiles {
    /// The files `payload` names; none without a kernel.
    fn read(payload: &PayloadArgs) -> Result<Option<LinuxFiles>> {
        let Some(kernel) = &payload.kernel else {
            return Ok(None);
        };
        Ok(Some(LinuxFiles {
            kernel: read_file(kernel)?,
            dtbs: payload
                .dtb
                .iter()
                .map(|p| read_file(p))
                .collect::<Result<_>>()?,
            initramfs: payload.initramfs.as_deref().map(read_file).transpose()?,
            logo: payload.logo.as_deref().map(read_file).transpose()?,
        }))
    }

    /// The files as packing takes them, with `dtbs` as slices of them.
    fn linux<'a>(&'a self, payload: &'a PayloadArgs, dtbs: &'a [&'a [u8]]) -> Linux<'a> {
        Linux {
            kernel: &self.kernel,
            dtbs,
            initramfs: self.initramfs.as_deref(),
            bootargs: payload.bootargs.as_deref(),
            logo: self.logo.as_deref(),
        }
    }

    fn dtbs(&self) -> Vec<&[u8]> {
        self.dtbs.iter().map(Vec::as_slice).collect()
    }
}

/// The report of an action that returns no data of its own.
#[derive(Serialize)]
struct Done {
    action: &'static str,
    /// Bytes sent, for actions that send a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    /// Whether the device acknowledged, for commands that may make it leave.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Sent>,
}

impl Done {
    fn action(action: &'static str) -> Done {
        Done {
            action,
            size: None,
            result: None,
        }
    }

    fn sent(action: &'static str, data: &[u8]) -> Done {
        Done {
            size: Some(data.len() as u64),
            ..Done::action(action)
        }
    }

    fn command(result: Sent) -> Done {
        Done {
            result: Some(result),
            ..Done::action("command")
        }
    }
}

impl Report for Done {
    fn human(&self) -> Vec<u8> {
        let text = match (self.size, self.result) {
            (Some(size), _) => format!("{}: {size} bytes\n", self.action),
            (None, Some(result)) => format!("{}: {}\n", self.action, result.as_str()),
            (None, None) => format!("{}: done\n", self.action),
        };
        text.into_bytes()
    }
}

/// The run log's first line: which orchard, on what.
pub(crate) fn run_title() -> String {
    format!("orchard {}, {} {}", crate::args::version(), std::env::consts::OS, std::env::consts::ARCH)
}

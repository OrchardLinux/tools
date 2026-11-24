pub mod boards;
pub mod boot;
pub mod cache;
pub mod data;
pub mod devices;
pub mod dfu;
pub mod fw;
pub mod install;
pub mod load;
pub mod m1n1;
pub mod pongo;
pub mod prep;
pub mod recovery;
pub mod run;
pub mod session;
pub mod term;

use crate::args::{FirmwareArgs, Global, PathArgs, PayloadArgs};
use crate::output::Report;
use orchard_boards::catalog::Catalog;
use orchard_boards::pack::Linux;
use orchard_cache::store::Store;
use orchard_cache::{Context, Result};
use orchard_device::prep::{BootPath, Request};
use orchard_device::select;
use orchard_device::session::{in_session, Session};
use orchard_usb::{Host, Mode, Sent};
use serde::Serialize;
use std::fmt::Write;
use std::path::Path;

/// How a command that prepares a boot chain is told to: which methods,
/// and what a chain's firmware is built with.
pub struct Way<'a> {
    pub path: &'a PathArgs,
    pub firmware: &'a FirmwareArgs,
}

impl Way<'_> {
    /// The request of the device flags and these, without a store for a
    /// plan.
    pub fn request<'a>(&'a self, catalog: &'a Catalog, store: Option<&'a Store>) -> Request<'a> {
        let path = self.path;
        Request {
            store,
            firmware: orchard_boards::firmware_build::Boot {
                chain: None,
                keys: self.firmware.keys.as_deref(),
                im4m: self.firmware.im4m.as_deref(),
            },
            from: path.from,
            entry: path.entry.as_deref(),
            chain: path.chain.as_deref(),
            board: path.board.as_deref(),
            ..Request::new(catalog)
        }
    }
}

/// The text of a plan's methods and their steps.
pub fn planned_text(planned: &BootPath) -> String {
    let from = &planned.from;
    let mut text = match &planned.board {
        Some(board) => format!("{board} in {}", from.state.as_str()),
        None => format!("a device in {}", from.state.as_str()),
    };
    if let Some(marker) = &from.marker {
        let _ = write!(text, " (marker {marker})");
    }
    text.push('\n');
    if planned.methods.is_empty() {
        text.push_str("methods: none needed\n");
    }
    let mut number = 0;
    for at in &planned.methods {
        let method = &at.method;
        let _ = writeln!(
            text,
            "{} ({}): {} to {}",
            method.name,
            method.kind.as_str(),
            method.needs.as_str(),
            method.leaves.as_str()
        );
        for step in &at.steps {
            number += 1;
            let _ = writeln!(text, "  {number}. {step}");
        }
    }
    text
}

/// The methods that ran, each with its steps, as text.
pub fn went_text(went: &BootPath) -> String {
    let mut text = String::new();
    for at in &went.methods {
        let method = &at.method;
        let _ = writeln!(
            text,
            "{} ({}): {} to {}",
            method.name,
            method.kind.as_str(),
            method.needs.as_str(),
            method.leaves.as_str()
        );
        for step in at.done.iter().flatten() {
            let _ = writeln!(text, "  {}: {}", step.name, step.outcome.as_str());
        }
    }
    text
}

/// Where the device is after a run.
pub fn device_text(went: &BootPath) -> String {
    match (&went.device, went.state) {
        (Some(device), Some(state)) => {
            format!("the device at {} is in {}", device.location, state.as_str())
        }
        _ => String::new(),
    }
}

/// A plan, or what a run did and where the device is now.
impl Report for BootPath {
    fn human(&self) -> Vec<u8> {
        if self.device.is_none() {
            return planned_text(self).into_bytes();
        }
        format!("{}{}\n", went_text(self), device_text(self)).into_bytes()
    }
}

/// The device a command that starts a system works on: without a device
/// flag, a device that already runs Linux is not the one to start.
fn session_to_start(global: &Global, timeout: std::time::Duration) -> Result<Session<Host>> {
    let selector = select::to_start(&mut Host, &in_session(&global.device.selector(None)))?;
    Session::start(Host, &selector, Some(timeout))
}

/// A session for a plan: none when a board and a state say all a plan
/// needs, else one that only looks at the device.
fn plan_session(global: &Global, path: &PathArgs) -> Result<Option<Session<Host>>> {
    match (&path.board, path.from) {
        (Some(_), Some(_)) => Ok(None),
        _ => Session::start(Host, &global.device.selector(None), None).map(Some),
    }
}

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

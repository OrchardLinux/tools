//! What a workflow needs from the USB host besides the list of devices:
//! opening a device for a protocol.
//!
//! Workflows are written against these traits, so their tests can stand in
//! for the device in the test file and check the order of what is sent.
//! The real host opens the protocol modules of the usb crate.
use crate::entry::{Executable, GASTER};
use orchard_cache::store::Store;
use orchard_usb::dfu::{Dfu, Pwned, Status};
use orchard_usb::m1n1::Proxy;
use orchard_usb::pongo::{Output, Pongo};
use orchard_usb::recovery::Recovery;
use orchard_usb::{Device, Discovery, Error, Sent};
use std::time::Duration;

#[cfg(unix)]
mod tool;

/// One argument of a tool: text passed as it is, or the path of one of the
/// tool's input files, by its place in [`Tool::files`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolArg<'a> {
    Text(&'a str),
    File(usize),
}

/// An outside tool and what it is run with. The entry that uses the tool
/// decides all of it; the host writes the files out and runs the tool.
pub struct Tool<'a> {
    /// The name errors call the tool by.
    pub name: &'a str,
    pub executable: &'a [u8],
    pub args: &'a [ToolArg<'a>],
    pub files: &'a [&'a [u8]],
    /// A directory files can be run from. The tool is staged in a private
    /// directory inside it.
    pub dir: &'a std::path::Path,
}

/// DFU's actions, as the boot ROM and an iBSS in DFU mode take them; see
/// [`Dfu`].
pub trait DfuMode {
    fn status(&mut self, timeout: Duration) -> Result<Status, Error>;
    fn clear_status(&mut self, timeout: Duration) -> Result<(), Error>;
    fn abort(&mut self, timeout: Duration) -> Result<(), Error>;
    fn reset(&mut self) -> Result<(), Error>;
    fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error>;
    /// Starts `image` on a boot ROM an exploit has run on. Such a boot ROM
    /// takes an image in its own way, which differs from [`DfuMode::send`].
    fn start_pwned(
        &mut self,
        payload: Pwned,
        image: &[u8],
        timeout: Duration,
    ) -> Result<Sent, Error>;
}

impl DfuMode for Dfu {
    fn status(&mut self, timeout: Duration) -> Result<Status, Error> {
        Dfu::status(self, timeout)
    }
    fn clear_status(&mut self, timeout: Duration) -> Result<(), Error> {
        Dfu::clear_status(self, timeout)
    }
    fn abort(&mut self, timeout: Duration) -> Result<(), Error> {
        Dfu::abort(self, timeout)
    }
    fn reset(&mut self) -> Result<(), Error> {
        Dfu::reset(self)
    }
    fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error> {
        Dfu::send(self, image, timeout)
    }
    fn start_pwned(
        &mut self,
        payload: Pwned,
        image: &[u8],
        timeout: Duration,
    ) -> Result<Sent, Error> {
        Dfu::start_pwned(self, payload, image, timeout)
    }
}

/// iBoot's actions in recovery mode; see [`Recovery`].
pub trait RecoveryMode {
    fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error>;
    fn command(&mut self, command: &str, timeout: Duration) -> Result<Sent, Error>;
    fn getenv(&mut self, name: &str, timeout: Duration) -> Result<String, Error>;
}

impl RecoveryMode for Recovery {
    fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error> {
        Recovery::send(self, image, timeout)
    }
    fn command(&mut self, command: &str, timeout: Duration) -> Result<Sent, Error> {
        Recovery::command(self, command, timeout)
    }
    fn getenv(&mut self, name: &str, timeout: Duration) -> Result<String, Error> {
        Recovery::getenv(self, name, timeout)
    }
}

/// The Pongo shell's actions; see [`Pongo`].
pub trait PongoShell {
    fn output(&mut self, timeout: Duration) -> Result<Output, Error>;
    fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error>;
    fn command(&mut self, command: &str, timeout: Duration) -> Result<Sent, Error>;
}

impl PongoShell for Pongo {
    fn output(&mut self, timeout: Duration) -> Result<Output, Error> {
        Pongo::output(self, timeout)
    }
    fn send(&mut self, image: &[u8], timeout: Duration) -> Result<(), Error> {
        Pongo::send(self, image, timeout)
    }
    fn command(&mut self, command: &str, timeout: Duration) -> Result<Sent, Error> {
        Pongo::command(self, command, timeout)
    }
}

/// The m1n1 proxy's actions; see [`Proxy`].
pub trait M1n1Proxy {
    fn ping(&mut self, timeout: Duration) -> Result<(), Error>;
    fn call(&mut self, operation: u64, args: &[u64], timeout: Duration) -> Result<u64, Error>;
    fn read(&mut self, address: u64, size: usize, timeout: Duration) -> Result<Vec<u8>, Error>;
    fn write(&mut self, address: u64, data: &[u8], timeout: Duration) -> Result<(), Error>;
    fn take_console(&mut self) -> Vec<u8>;
}

impl M1n1Proxy for Proxy {
    fn ping(&mut self, timeout: Duration) -> Result<(), Error> {
        Proxy::ping(self, timeout)
    }
    fn call(&mut self, operation: u64, args: &[u64], timeout: Duration) -> Result<u64, Error> {
        Proxy::call(self, operation, args, timeout)
    }
    fn read(&mut self, address: u64, size: usize, timeout: Duration) -> Result<Vec<u8>, Error> {
        Proxy::read(self, address, size, timeout)
    }
    fn write(&mut self, address: u64, data: &[u8], timeout: Duration) -> Result<(), Error> {
        Proxy::write(self, address, data, timeout)
    }
    fn take_console(&mut self) -> Vec<u8> {
        Proxy::take_console(self)
    }
}

/// A USB host that lists devices and opens them.
pub trait Connect: Discovery {
    /// Resolve the worker before dispatching any device operations that need it.
    fn gaster_worker(&self, _store: &Store) -> orchard_cache::Result<Executable> {
        Err(orchard_cache::Error::new(
            orchard_cache::Code::Unsupported,
            "this host has no gaster worker",
        ))
    }

    /// The pinned, isolated gaster worker: entry with no bags, otherwise
    /// unwrap-only. `inherit` hands the session lock to the child.
    fn gaster(
        &mut self,
        _device: &Device,
        _bags: Option<&[[u8; 48]]>,
        _worker: &Executable,
        _timeout: Duration,
        _inherit: &dyn Fn(&mut std::process::Command) -> orchard_cache::Result<()>,
    ) -> orchard_cache::Result<Vec<[u8; 48]>> {
        Err(orchard_cache::Error::new(
            orchard_cache::Code::Unsupported,
            "this host has no gaster worker",
        ))
    }

    /// Runs an entry's tool to its end, for `timeout` at most. A host
    /// that runs no tools keeps this answer.
    fn entry_tool(&mut self, tool: &Tool<'_>, _timeout: Duration) -> orchard_cache::Result<()> {
        Err(orchard_cache::Error::new(
            orchard_cache::Code::Unsupported,
            format!("{}: this host runs no outside tools", tool.name),
        ))
    }
    fn dfu(&mut self, device: &Device) -> Result<Box<dyn DfuMode>, Error>;
    fn recovery(&mut self, device: &Device) -> Result<Box<dyn RecoveryMode>, Error>;
    fn pongo(&mut self, device: &Device) -> Result<Box<dyn PongoShell>, Error>;
    fn m1n1(&mut self, device: &Device) -> Result<Box<dyn M1n1Proxy>, Error>;
}

impl Connect for orchard_usb::Host {
    fn gaster_worker(&self, store: &Store) -> orchard_cache::Result<Executable> {
        GASTER.tool(store)
    }

    #[cfg(unix)]
    fn gaster(
        &mut self,
        device: &Device,
        bags: Option<&[[u8; 48]]>,
        worker: &Executable,
        timeout: Duration,
        inherit: &dyn Fn(&mut std::process::Command) -> orchard_cache::Result<()>,
    ) -> orchard_cache::Result<Vec<[u8; 48]>> {
        let identity = &device.identity;
        let (Some(ecid), Some(chip), Some(board)) =
            (&identity.ecid, &identity.chip, &identity.board_id)
        else {
            return Err(orchard_cache::Error::new(
                orchard_cache::Code::InvalidInput,
                "gaster needs ECID, chip and board identity",
            ));
        };
        if bags.is_some_and(|b| b.is_empty() || b.len() > 3) {
            return Err(orchard_cache::Error::new(
                orchard_cache::Code::InvalidInput,
                "expected one to three keybags",
            ));
        }
        let data: Vec<u8> = bags.unwrap_or_default().iter().flatten().copied().collect();
        let seconds = format!("{:x}", timeout.as_secs().saturating_add(1).min(3600));
        let args = [
            ToolArg::Text(if bags.is_some() { "unwrap" } else { "pwn" }),
            ToolArg::Text(ecid),
            ToolArg::Text(chip),
            ToolArg::Text(board),
            ToolArg::Text(&seconds),
            if bags.is_some() {
                ToolArg::File(0)
            } else {
                ToolArg::Text("-")
            },
        ];
        let tool = Tool {
            name: "gaster",
            executable: &worker.bytes,
            args: &args,
            files: &[&data],
            dir: &worker.dir,
        };
        let reply = tool::reply(&tool, timeout, data.len(), inherit)?;
        Ok(reply.as_chunks::<48>().0.to_vec())
    }
    #[cfg(unix)]
    fn entry_tool(
        &mut self,
        invocation: &Tool<'_>,
        timeout: Duration,
    ) -> orchard_cache::Result<()> {
        tool::execute(invocation, timeout)
    }
    fn dfu(&mut self, device: &Device) -> Result<Box<dyn DfuMode>, Error> {
        Ok(Box::new(Dfu::open(device)?))
    }
    fn recovery(&mut self, device: &Device) -> Result<Box<dyn RecoveryMode>, Error> {
        Ok(Box::new(Recovery::open(device)?))
    }
    fn pongo(&mut self, device: &Device) -> Result<Box<dyn PongoShell>, Error> {
        Ok(Box::new(Pongo::open(device)?))
    }
    fn m1n1(&mut self, device: &Device) -> Result<Box<dyn M1n1Proxy>, Error> {
        Ok(Box::new(Proxy::open(device)?))
    }
}

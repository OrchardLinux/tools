use crate::args::Global;
use orchard_cache::Result;
use orchard_device::session::Session;
use orchard_usb::Host;
use std::process::ExitCode;
use std::time::Duration;

/// Locks the one device the flags choose, runs `command` in a session on
/// it, and ends with the command's own exit status.
pub fn run(global: &Global, timeout: Option<Duration>, command: &[String]) -> Result<ExitCode> {
    let session = Session::start(Host, &global.device.selector(None), timeout)?;
    let status = session.run(command)?;
    // A command ended by a signal has no status of its own.
    Ok(ExitCode::from(status.code().map_or(1, |code| code as u8)))
}

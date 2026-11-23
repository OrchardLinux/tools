use super::{read_file, session, Done};
use crate::args::{DfuCommand, Global};
use crate::output::{Output, Report};
use orchard_cache::Result;
use orchard_device::usb;
use orchard_usb::dfu::Status;
use orchard_usb::Mode;
use std::time::Duration;

impl Report for Status {
    fn human(&self) -> Vec<u8> {
        format!(
            "state {} ({}), status {}\n",
            self.state, self.state_name, self.status
        )
        .into_bytes()
    }
}

pub fn run(global: &Global, timeout: Duration, command: &DfuCommand) -> Result<Output> {
    // Read the file first, so a wrong path never opens the device.
    let image = match command {
        DfuCommand::Send { file } => read_file(file)?,
        _ => Vec::new(),
    };
    let mut session = session(global, Mode::Dfu)?;
    let mut dfu = session.dfu()?;
    let timeout = session.timeout(timeout)?;
    Ok(match command {
        DfuCommand::Status => dfu.status(timeout).map_err(usb::error)?.into(),
        DfuCommand::ClearStatus => {
            dfu.clear_status(timeout).map_err(usb::error)?;
            Done::action("clear-status").into()
        }
        DfuCommand::Abort => {
            dfu.abort(timeout).map_err(usb::error)?;
            Done::action("abort").into()
        }
        DfuCommand::Send { .. } => {
            dfu.send(&image, timeout).map_err(usb::error)?;
            Done::sent("send", &image).into()
        }
        DfuCommand::Reset => {
            dfu.reset().map_err(usb::error)?;
            Done::action("reset").into()
        }
    })
}

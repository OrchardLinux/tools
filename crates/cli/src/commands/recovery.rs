use super::{read_file, session, Done};
use crate::args::{Global, RecoveryCommand};
use crate::output::{Output, Report};
use orchard_cache::Result;
use orchard_device::usb;
use orchard_usb::Mode;
use serde::Serialize;
use std::time::Duration;

#[derive(Serialize)]
struct Variable {
    name: String,
    value: String,
}

impl Report for Variable {
    fn human(&self) -> Vec<u8> {
        format!("{}\n", self.value).into_bytes()
    }
}

pub fn run(global: &Global, timeout: Duration, command: &RecoveryCommand) -> Result<Output> {
    // Read the file first, so a wrong path never opens the device.
    let image = match command {
        RecoveryCommand::Send { file } => read_file(file)?,
        _ => Vec::new(),
    };
    let mut session = session(global, Mode::Recovery)?;
    let mut recovery = session.recovery()?;
    let timeout = session.timeout(timeout)?;
    Ok(match command {
        RecoveryCommand::Send { .. } => {
            recovery.send(&image, timeout).map_err(usb::error)?;
            Done::sent("send", &image).into()
        }
        RecoveryCommand::Command { command } => {
            Done::command(recovery.command(command, timeout).map_err(usb::error)?).into()
        }
        RecoveryCommand::Getenv { name } => Variable {
            name: name.clone(),
            value: recovery.getenv(name, timeout).map_err(usb::error)?,
        }
        .into(),
    })
}

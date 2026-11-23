use super::{read_file, session, Done};
use crate::args::{Global, PongoCommand};
use crate::output::{Output, Report};
use orchard_cache::Result;
use orchard_device::usb;
use orchard_usb::pongo;
use orchard_usb::Mode;
use std::time::Duration;

impl Report for pongo::Output {
    fn human(&self) -> Vec<u8> {
        self.text.clone().into_bytes()
    }
}

pub fn run(global: &Global, timeout: Duration, command: &PongoCommand) -> Result<Output> {
    // Read the file first, so a wrong path never opens the device.
    let image = match command {
        PongoCommand::Send { file } => read_file(file)?,
        _ => Vec::new(),
    };
    let mut session = session(global, Mode::Pongo)?;
    let mut pongo = session.pongo()?;
    let timeout = session.timeout(timeout)?;
    Ok(match command {
        PongoCommand::Output => pongo.output(timeout).map_err(usb::error)?.into(),
        PongoCommand::Send { .. } => {
            pongo.send(&image, timeout).map_err(usb::error)?;
            Done::sent("send", &image).into()
        }
        PongoCommand::Command { command } => {
            Done::command(pongo.command(command, timeout).map_err(usb::error)?).into()
        }
    })
}

use super::{read_file, session, Done, LinuxFiles};
use crate::args::{Global, M1n1Command};
use crate::output::{Output, Report};
use orchard_boards::pack;
use orchard_cache::store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_device::usb;
use orchard_usb::Mode;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

/// The result of a proxy operation. Results are mostly addresses, so hex.
#[derive(Serialize)]
struct Value {
    value: String,
}

impl Report for Value {
    fn human(&self) -> Vec<u8> {
        format!("{}\n", self.value).into_bytes()
    }
}

/// Memory read from the device: the bytes for people, a description in JSON.
#[derive(Serialize)]
struct Memory {
    address: String,
    size: u64,
    sha256: String,
    #[serde(skip)]
    bytes: Vec<u8>,
}

impl Report for Memory {
    fn human(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// An image written to a file.
#[derive(Serialize)]
struct Packed {
    path: String,
    size: u64,
    sha256: String,
}

impl Report for Packed {
    fn human(&self) -> Vec<u8> {
        format!("wrote {} bytes to {}\n", self.size, self.path).into_bytes()
    }
}

pub fn run(global: &Global, timeout: Duration, command: &M1n1Command) -> Result<Output> {
    if let M1n1Command::Pack { file, payload, out } = command {
        if payload.kernel.is_none() {
            return Err(Error::new(
                Code::InvalidInput,
                "nothing to pack: give at least --kernel and --dtb",
            ));
        }
        let files = LinuxFiles::read(payload)?.expect("checked above");
        let dtbs = files.dtbs();
        let image = pack::m1n1(&read_file(file)?, Some(&files.linux(payload, &dtbs)))?;
        std::fs::write(out, &image).context(out.display())?;
        return Ok(Packed {
            path: out.display().to_string(),
            size: image.len() as u64,
            sha256: store::sha256(&image),
        }
        .into());
    }
    // Read the file first, so a wrong path never opens the device.
    let data = match command {
        M1n1Command::Write { file, .. } => read_file(file)?,
        _ => Vec::new(),
    };
    let mut session = session(global, Mode::M1n1)?;
    let mut proxy = session.m1n1()?;
    let timeout = session.timeout(timeout)?;
    Ok(match command {
        M1n1Command::Pack { .. } => unreachable!("handled above"),
        M1n1Command::Ping => {
            proxy.ping(timeout).map_err(usb::error)?;
            Done::action("ping").into()
        }
        M1n1Command::Call { operation, args } => Value {
            value: format!(
                "0x{:x}",
                proxy.call(*operation, args, timeout).map_err(usb::error)?
            ),
        }
        .into(),
        M1n1Command::Read { address, size } => {
            let size = usize::try_from(*size)
                .map_err(|_| Error::new(Code::InvalidInput, "size: too large for this host"))?;
            let bytes = proxy.read(*address, size, timeout).map_err(usb::error)?;
            Memory {
                address: format!("0x{address:x}"),
                size: bytes.len() as u64,
                sha256: Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
                bytes,
            }
            .into()
        }
        M1n1Command::Write { address, .. } => {
            proxy.write(*address, &data, timeout).map_err(usb::error)?;
            Done::sent("write", &data).into()
        }
    })
}

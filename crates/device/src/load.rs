//! Loading a file onto a device: the receiver the device waits in, or a
//! prep to one whose loader takes the file, then the file packed for that
//! loader and delivered.
//!
//! What the file is decides the loader: a plain image goes to Pongo,
//! m1n1's Mach-O to a patched iBoot as its restore kernel, Cascadia's
//! loader to an autogo iBEC with the Linux it starts.
use crate::host::Connect;
use crate::prep::{self, BootPath, Load, Request};
use crate::session::Session;
use orchard_boards::pack::Form;
use orchard_cache::{Code, Error, Result};
use orchard_usb::{Discovery, Mode};
use serde::Serialize;

/// What a finished load did.
#[derive(Debug, Clone, Serialize)]
pub struct Loaded {
    #[serde(flatten)]
    pub went: BootPath,
    /// The file that was given.
    pub size: u64,
    pub sha256: String,
}

/// The file the request loads, once what can be checked without a device
/// fits.
fn file<'a>(request: &Request<'a>) -> Result<&'a [u8]> {
    let Some(Load::File(file, linux)) = request.load else {
        unreachable!("a load names its file")
    };
    let form = Form::of(file);
    let starts = match form {
        Form::Image => None,
        Form::MachO => Some(Mode::M1n1),
        Form::Cascadia => Some(Mode::Linux),
    };
    if let (Some(starts), Some(wait)) = (starts, request.wait) {
        if wait != starts {
            return Err(Error::new(
                Code::InvalidInput,
                format!(
                    "{} starts {}, and the load waits for it: no other --wait",
                    form.as_str(),
                    starts.as_str()
                ),
            ));
        }
    }
    if form == Form::Cascadia && linux.is_none() {
        return Err(Error::new(
            Code::InvalidInput,
            "Cascadia's loader starts the Linux it is given: give --kernel, --dtb and --initramfs",
        ));
    }
    Ok(file)
}

/// The modes a load does not start from, with what to do instead.
fn refused(mode: Mode) -> Result<()> {
    let (why, hint) = match mode {
        Mode::M1n1 => ("m1n1 is already running", "`orchard2 m1n1` talks to it"),
        Mode::Linux => (
            "the device is running Linux",
            "put the device into DFU mode first",
        ),
        _ => return Ok(()),
    };
    Err(Error::new(
        Code::Unsupported,
        format!("load from {} mode: {why}", mode.as_str()),
    )
    .hint(hint))
}

/// Loads the file onto the session's device and starts it.
pub fn run<D: Connect>(
    session: &mut Session<D>,
    request: &Request,
    progress: &mut dyn FnMut(&str),
) -> Result<Loaded> {
    let file = file(request)?;
    refused(session.device().mode)?;
    Ok(Loaded {
        went: prep::run(session, request, progress)?,
        size: file.len() as u64,
        sha256: orchard_cache::store::sha256(file),
    })
}

/// What a load would do, from where the device is or, without one, from
/// the stated state of the named board.
pub fn plan<D: Discovery>(request: &Request, session: Option<&Session<D>>) -> Result<BootPath> {
    file(request)?;
    if let Some(session) = session {
        refused(session.device().mode)?;
    }
    prep::plan(request, session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use orchard_boards::pack::Linux;

    #[test]
    fn what_cannot_fit_is_refused_before_a_device_is_looked_at() {
        let (_dir, catalog) = fixture::catalog();
        let loader = fixture::loader();
        let loading = |load, wait| Request {
            load: Some(load),
            wait,
            ..Request::new(&catalog)
        };
        assert!(file(&loading(Load::File(b"plain", None), Some(Mode::M1n1))).is_ok());
        let e = file(&loading(Load::File(&loader, None), None)).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
        assert!(e.message.contains("--kernel"));
        let linux = Linux {
            kernel: b"zImage",
            ..Linux::default()
        };
        let waiting = loading(Load::File(&loader, Some(linux)), Some(Mode::M1n1));
        assert_eq!(
            file(&waiting).unwrap_err().message,
            "Cascadia's loader starts linux, and the load waits for it: no other --wait"
        );
        let e = refused(Mode::M1n1).unwrap_err();
        assert_eq!(e.hint.as_deref(), Some("`orchard2 m1n1` talks to it"));
        assert!(refused(Mode::Linux).is_err());
        assert!(refused(Mode::Pongo).is_ok());
    }
}

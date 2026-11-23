//! USB errors as ours. The error type lives below USB, so this is a function
//! for `map_err` rather than a `From` impl.
use orchard_cache::{Code, Error};
use orchard_usb::Kind;

pub fn error(e: orchard_usb::Error) -> Error {
    let code = match e.kind {
        Kind::Invalid => Code::InvalidInput,
        Kind::Timeout => Code::Timeout,
        Kind::Unsupported => Code::Unsupported,
        _ => Code::Usb,
    };
    let error = Error::new(code, e.to_string());
    match e.kind {
        Kind::Busy => error.hint("another program is using the device; close it and try again"),
        Kind::Denied => error.hint("this user may not open the device; check its permissions"),
        Kind::WrongMode => error.hint("`orchard2 devices` shows the mode each device is in"),
        _ => error,
    }
}

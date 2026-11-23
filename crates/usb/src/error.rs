use std::fmt;

/// How an action failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The device did not finish the transfer in the time given.
    Timeout,
    /// The device rejected the request.
    Stall,
    /// The device is gone.
    Disconnected,
    /// The device answered, but not with what the protocol expects.
    Unexpected,
    /// The device is not in the mode this protocol talks to.
    WrongMode,
    /// Another program or driver holds the device.
    Busy,
    /// The host does not let this user open the device.
    Denied,
    /// The caller's argument cannot be sent with this protocol.
    Invalid,
    /// This version does not have the action.
    Unsupported,
    /// Any other failure the host reports.
    Other,
}

/// A failed action: which one, and how.
#[derive(Debug, Clone)]
pub struct Error {
    pub action: &'static str,
    pub kind: Kind,
    pub detail: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.action, self.detail)
    }
}

impl std::error::Error for Error {}

/// A failure before it is tied to an action.
#[derive(Debug, Clone)]
pub(crate) struct Fault {
    pub kind: Kind,
    pub detail: String,
}

impl Fault {
    pub fn new(kind: Kind, detail: impl Into<String>) -> Fault {
        Fault {
            kind,
            detail: detail.into(),
        }
    }

    pub fn unexpected(detail: impl Into<String>) -> Fault {
        Fault::new(Kind::Unexpected, detail)
    }

    pub fn invalid(detail: impl Into<String>) -> Fault {
        Fault::new(Kind::Invalid, detail)
    }

    pub fn during(self, action: &'static str) -> Error {
        Error {
            action,
            kind: self.kind,
            detail: self.detail,
        }
    }
}

/// Names the action a failure happened in.
pub(crate) trait During<T> {
    fn during(self, action: &'static str) -> Result<T, Error>;
}

impl<T> During<T> for Result<T, Fault> {
    fn during(self, action: &'static str) -> Result<T, Error> {
        self.map_err(|fault| fault.during(action))
    }
}

impl From<nusb::Error> for Fault {
    fn from(e: nusb::Error) -> Fault {
        let kind = match e.kind() {
            nusb::ErrorKind::Disconnected | nusb::ErrorKind::NotFound => Kind::Disconnected,
            nusb::ErrorKind::Busy => Kind::Busy,
            nusb::ErrorKind::PermissionDenied => Kind::Denied,
            _ => Kind::Other,
        };
        Fault::new(kind, e.to_string())
    }
}

impl From<nusb::transfer::TransferError> for Fault {
    fn from(e: nusb::transfer::TransferError) -> Fault {
        use nusb::transfer::TransferError as T;
        match e {
            T::Cancelled => Fault::new(Kind::Timeout, "timed out"),
            T::Stall => Fault::new(Kind::Stall, "the device rejected the request"),
            T::Disconnected => Fault::new(Kind::Disconnected, "the device disconnected"),
            other => Fault::new(Kind::Other, other.to_string()),
        }
    }
}

use serde::Serialize;
use std::fmt;

/// Stable error codes. Scripts match on these, so they never change meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Code {
    InvalidInput,
    NoDevice,
    Ambiguous,
    Usb,
    /// Input data is not in the expected format.
    Format,
    Network,
    Io,
    /// A file, member or object that should be there is not.
    NotFound,
    /// Something is larger than the caller allows.
    TooLarge,
    /// The data asks for something this version cannot do yet.
    Unsupported,
    /// The device or the work as a whole took longer than allowed.
    Timeout,
    /// Another orchard2 process is working on the device.
    Locked,
    /// The device is not the one, or not in the mode, the work started with.
    DeviceChanged,
    /// The device answered and said it cannot do what was asked.
    Refused,
    /// A producer or a project's build entry failed.
    Build,
}

impl Code {
    pub fn exit_status(self) -> u8 {
        match self {
            Code::InvalidInput => 2,
            Code::NoDevice | Code::Ambiguous => 3,
            Code::Usb => 4,
            Code::Format => 5,
            Code::Network => 6,
            Code::Io => 7,
            Code::NotFound => 8,
            Code::TooLarge => 9,
            Code::Unsupported => 10,
            Code::Timeout => 11,
            Code::Locked => 12,
            Code::DeviceChanged => 13,
            Code::Refused => 14,
            Code::Build => 15,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: Code,
    pub message: String,
    /// An optional second line telling the user what to do next.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Error {
        Error {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Error {
        self.hint = Some(hint.into());
        self
    }

    /// Says what the error is about: `context("BuildManifest.plist")` turns
    /// "not in the archive" into "BuildManifest.plist: not in the archive".
    pub fn context(mut self, what: impl fmt::Display) -> Error {
        self.message = format!("{what}: {}", self.message);
        self
    }
}

/// Adds [`Error::context`] to any result whose error converts into ours.
pub trait Context<T> {
    fn context(self, what: impl fmt::Display) -> Result<T>;
}

impl<T, E: Into<Error>> Context<T> for std::result::Result<T, E> {
    fn context(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|e| e.into().context(what))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\n{hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<orchard_formats::Error> for Error {
    fn from(e: orchard_formats::Error) -> Error {
        Error::new(Code::Format, e.0)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::new(Code::Io, e.to_string())
    }
}

//! What every layer above USB shares: the one error type, the on-disk
//! store, byte-range sources and HTTPS downloads.
pub mod error;
pub mod http;
pub mod source;
pub mod store;

pub use error::{Code, Context, Error, Result};

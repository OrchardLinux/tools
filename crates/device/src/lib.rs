//! Device work that needs more than one USB action: choosing a device,
//! locking it, deadlines, and the session that follows a device through
//! its modes and gives the handles to talk to it.
pub mod deadline;
pub mod gaster;
pub mod host;
use orchard_cache::inherit;
pub mod keys;
pub use orchard_cache::lock;
pub mod select;
pub mod session;
mod state;
pub mod usb;

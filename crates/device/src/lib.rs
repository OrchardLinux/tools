//! Device work that needs more than one USB action: choosing a device,
//! locking it, deadlines, the methods that prepare a boot chain, the
//! loaders' transfers, and the load and boot workflows.
pub mod boot;
pub mod deadline;
pub mod deliver;
pub mod entry;
#[cfg(test)]
mod fixture;
pub mod host;
mod inherit;
pub mod keys;
pub mod load;
pub mod lock;
pub mod methods;
pub mod prep;
pub mod select;
pub mod session;
mod state;
pub mod steps;
pub mod usb;

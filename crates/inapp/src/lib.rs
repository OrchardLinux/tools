//! The installer that runs on the device itself, inside iOS: the helper of
//! the installer app links this as a library. `flow` is its state, `engine`
//! runs it, and the rest is the work: downloads, the partition table, the
//! root image, the finished root and the boot filesystem.
pub mod crypt;
pub mod disk;
pub mod download;
pub mod engine;
pub mod ext4;
pub mod ffi;
pub mod finish;
#[cfg(test)]
mod fixture;
pub mod flow;
pub mod image;
pub mod install;

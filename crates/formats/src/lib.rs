//! File formats Apple devices and Linux boots use: parse and build from bytes.
//!
//! This crate has no dependencies on USB, network, files or the other
//! orchard crates. Every function takes bytes and returns bytes or a parsed
//! value that borrows from its input.
pub mod aop;
pub mod apfs;
pub mod bluetool;
pub mod build_manifest;
pub mod cascadia;
pub mod cpio;
pub mod crypto;
pub mod decmpfs;
pub mod der;
pub mod fdt;
pub mod hfs;
pub mod hfs_write;
pub mod iboot;
pub mod im4p;
pub mod img3;
pub mod img4;
pub mod kernelcache;
pub mod lzfse;
pub mod lzss;
pub mod m1n1;
pub mod macho;
pub mod osserialize;
pub mod sgx;
pub mod tar;
pub mod trx;
pub mod udif;
pub mod z2fw;
pub mod zip;
pub mod zlib;

use std::fmt;

/// Input that is not the format it claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub &'static str);

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

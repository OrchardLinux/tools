//! 64-bit Mach-O headers: segments and sections, and the ARM64 slice of a
//! universal binary.
use crate::{Error, Result};

pub const MH_EXECUTE: u32 = 2;
/// A Mach-O made of several Mach-Os, one per fileset entry: the kernelcache
/// kind of iOS 16 and later.
pub const MH_FILESET: u32 = 0xc;
const MAGIC_64: u32 = 0xfeed_facf;
const CPU_ARM64: u32 = 0x0100_000c;
const LC_SEGMENT_64: u32 = 0x19;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;

/// One Mach-O of a fileset: its id (for a kext, the bundle id) and where its
/// own header lies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub vmaddr: u64,
    pub fileoff: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
    pub fileoff: u64,
    pub filesize: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub segment: String,
    pub name: String,
    pub addr: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub filetype: u32,
    pub segments: Vec<Segment>,
    pub sections: Vec<Section>,
    pub entries: Vec<Entry>,
}

/// Reads the Mach-O header that starts at `start` in `data`.
pub fn parse(data: &[u8], start: usize) -> Result<Header> {
    let word = |at: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            data.get(at..at + 4)
                .ok_or(Error("truncated Mach-O"))?
                .try_into()
                .unwrap(),
        ))
    };
    let quad = |at: usize| -> Result<u64> {
        Ok(u64::from_le_bytes(
            data.get(at..at + 8)
                .ok_or(Error("truncated Mach-O"))?
                .try_into()
                .unwrap(),
        ))
    };
    if word(start)? != MAGIC_64 {
        return Err(Error("not a 64-bit Mach-O"));
    }
    let (filetype, count, total) = (
        word(start + 12)?,
        word(start + 16)? as usize,
        word(start + 20)? as usize,
    );
    let end = start + 32 + total;
    if end > data.len() {
        return Err(Error("Mach-O load commands extend past the file"));
    }
    let mut header = Header {
        filetype,
        segments: Vec::new(),
        sections: Vec::new(),
        entries: Vec::new(),
    };
    let mut at = start + 32;
    for _ in 0..count {
        let (cmd, len) = (word(at)?, word(at + 4)? as usize);
        if len < 8 || at + len > end {
            return Err(Error("bad Mach-O load command"));
        }
        if cmd == LC_SEGMENT_64 {
            let nsects = word(at + 64)? as usize;
            if len < 72 + 80 * nsects {
                return Err(Error("truncated Mach-O segment"));
            }
            header.segments.push(Segment {
                name: name(&data[at + 8..at + 24]),
                vmaddr: quad(at + 24)?,
                vmsize: quad(at + 32)?,
                fileoff: quad(at + 40)?,
                filesize: quad(at + 48)?,
            });
            for i in 0..nsects {
                let s = at + 72 + 80 * i;
                header.sections.push(Section {
                    name: name(&data[s..s + 16]),
                    segment: name(&data[s + 16..s + 32]),
                    addr: quad(s + 32)?,
                    size: quad(s + 40)?,
                });
            }
        } else if cmd == LC_FILESET_ENTRY {
            if len < 32 {
                return Err(Error("truncated Mach-O fileset entry"));
            }
            // The id is a C string inside the command, at the offset it gives.
            let id = word(at + 24)? as usize;
            if !(32..len).contains(&id) {
                return Err(Error("bad Mach-O fileset entry id"));
            }
            header.entries.push(Entry {
                id: name(&data[at + id..at + len]),
                vmaddr: quad(at + 8)?,
                fileoff: quad(at + 16)?,
            });
        }
        at += len;
    }
    Ok(header)
}

/// The ARM64 Mach-O in `data`: `data` itself, or the one ARM64 slice of a
/// universal binary.
pub fn arm64(data: &[u8]) -> Result<&[u8]> {
    let slice = if data.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        let be = |at: usize| -> Result<usize> {
            Ok(u32::from_be_bytes(
                data.get(at..at + 4)
                    .ok_or(Error("truncated universal binary"))?
                    .try_into()
                    .unwrap(),
            ) as usize)
        };
        let count = be(4)?;
        let mut found = Vec::new();
        if !(1..=16).contains(&count) {
            return Err(Error("bad universal binary"));
        }
        for i in 0..count {
            let a = 8 + 20 * i;
            let (offset, size) = (be(a + 8)?, be(a + 12)?);
            if be(a)? as u32 == CPU_ARM64 {
                found.push(
                    data.get(offset..offset + size)
                        .ok_or(Error("universal binary slice out of range"))?,
                );
            }
        }
        match found.as_slice() {
            [one] => *one,
            _ => return Err(Error("universal binary has no single ARM64 slice")),
        }
    } else {
        data
    };
    if slice.len() < 32
        || slice[..4] != MAGIC_64.to_le_bytes()
        || slice[4..8] != CPU_ARM64.to_le_bytes()
    {
        return Err(Error("not an ARM64 Mach-O"));
    }
    Ok(slice)
}

fn name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw.split(|&b| b == 0).next().unwrap_or(&[])).into_owned()
}

//! The SGX543 GPU's firmware container, after rzxvx's Project Cascadia
//! extract-sgx-firmware.py: the `__DATA.__data` and `__TEXT.__const`
//! sections of the IMGSGX543 kext in a 32-bit prelinked kernel, behind a
//! header with their sizes, their addresses and the kernel's build.
//!
//! The driver's offsets into these sections hold for one kernel build only,
//! so the recipe pins the whole container's hash, addresses and build
//! included.
use crate::{Error, Result};
use std::collections::BTreeMap;

const MAGIC_32: &[u8] = b"\xce\xfa\xed\xfe";
const LC_SEGMENT: u32 = 1;

/// A segment of a 32-bit Mach-O: where it is in memory and in the file, and
/// its sections' addresses and sizes by name.
struct Segment {
    vmaddr: u32,
    fileoff: u32,
    filesize: u32,
    sections: BTreeMap<String, (u32, u32)>,
}

fn word(data: &[u8], at: usize) -> Result<u32> {
    let bytes = data.get(at..at + 4).ok_or(Error("truncated 32-bit Mach-O"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn name(data: &[u8], at: usize) -> Result<String> {
    let field = data.get(at..at + 16).ok_or(Error("truncated 32-bit Mach-O"))?;
    let text = field.split(|b| *b == 0).next().unwrap_or_default();
    // A name ends at its padding: nothing but NULs may follow.
    if field[text.len()..].iter().any(|b| *b != 0) || !text.is_ascii() {
        return Err(Error("Mach-O name is not plain text"));
    }
    Ok(String::from_utf8_lossy(text).into_owned())
}

/// The segments of the 32-bit Mach-O at `base`, by name.
fn segments(data: &[u8], base: usize) -> Result<BTreeMap<String, Segment>> {
    if data.get(base..base + 4) != Some(MAGIC_32) || base + 28 > data.len() {
        return Err(Error("not a 32-bit Mach-O"));
    }
    let (count, size) = (word(data, base + 16)? as usize, word(data, base + 20)? as usize);
    let (mut at, end) = (base + 28, base + 28 + size);
    if end > data.len() || count > size / 8 {
        return Err(Error("bad Mach-O load commands"));
    }
    let mut found = BTreeMap::new();
    for _ in 0..count {
        if at + 8 > end {
            return Err(Error("truncated load command"));
        }
        let (command, length) = (word(data, at)?, word(data, at + 4)? as usize);
        if length < 8 || at + length > end {
            return Err(Error("bad load command size"));
        }
        if command == LC_SEGMENT {
            if length < 56 {
                return Err(Error("truncated segment"));
            }
            let segment = name(data, at + 8)?;
            let (vmaddr, vmsize) = (word(data, at + 24)?, word(data, at + 28)?);
            let (fileoff, filesize) = (word(data, at + 32)?, word(data, at + 36)?);
            let sections = word(data, at + 48)? as usize;
            if 56 + 68 * sections > length || found.contains_key(&segment) {
                return Err(Error("bad segment sections"));
            }
            let mut by_name = BTreeMap::new();
            for index in 0..sections {
                let section_at = at + 56 + index * 68;
                let section = name(data, section_at)?;
                let (address, length) = (word(data, section_at + 32)?, word(data, section_at + 36)?);
                let inside = vmaddr <= address
                    && u64::from(address) + u64::from(length) <= u64::from(vmaddr) + u64::from(vmsize);
                if !inside || by_name.insert(section, (address, length)).is_some() {
                    return Err(Error("bad section range"));
                }
            }
            found.insert(segment, Segment { vmaddr, fileoff, filesize, sections: by_name });
        }
        at += length;
    }
    Ok(found)
}

/// The firmware container from the decoded 32-bit kernel, for the kernel
/// build `build` (such as `12H321`), which goes into its header.
pub fn extract(kernel: &[u8], build: &str) -> Result<Vec<u8>> {
    if build.is_empty() || build.len() > 16 || !build.is_ascii() {
        return Err(Error("bad SGX build id"));
    }
    let top = segments(kernel, 0)?;
    let prelink = top.get("__PRELINK_TEXT").ok_or(Error("kernel has no prelinked kexts"))?;
    let (start, size) = (prelink.fileoff as usize, prelink.filesize as usize);
    if start + size > kernel.len() {
        return Err(Error("truncated prelink text"));
    }
    // A section of a kext, through the prelink text's place in the file.
    let section = |(address, length): (u32, u32)| -> Result<&[u8]> {
        let offset = u64::from(address)
            .checked_sub(u64::from(prelink.vmaddr))
            .map(|delta| delta as usize + start);
        match offset {
            Some(offset) if offset + length as usize <= start + size => {
                Ok(&kernel[offset..offset + length as usize])
            }
            _ => Err(Error("section outside the prelink text")),
        }
    };
    let mut matches = Vec::new();
    // Kexts start on page boundaries.
    for base in (start..(start + size).saturating_sub(27)).step_by(0x1000) {
        if kernel.get(base..base + 4) != Some(MAGIC_32) {
            continue;
        }
        let kext = segments(kernel, base)?;
        let strings = kext.get("__TEXT").and_then(|text| text.sections.get("__cstring").copied());
        if let Some(strings) = strings {
            if contains(section(strings)?, b"SGXDriver543\0") {
                matches.push(kext);
            }
        }
    }
    let [kext] = matches.as_slice() else {
        return Err(Error("kernel does not hold exactly one IMGSGX543 kext"));
    };
    let missing = Error("IMGSGX543 kext lacks its firmware sections");
    let data_at = *kext.get("__DATA").and_then(|s| s.sections.get("__data")).ok_or(missing)?;
    let const_at = *kext.get("__TEXT").and_then(|s| s.sections.get("__const")).ok_or(missing)?;
    let (data, constants) = (section(data_at)?, section(const_at)?);
    let mut out = b"SGX543FW".to_vec();
    for field in [1, 64, data.len() as u32, 64 + data.len() as u32, constants.len() as u32, data_at.0, const_at.0] {
        out.extend(field.to_le_bytes());
    }
    let mut id = [0u8; 16];
    id[..build.len()].copy_from_slice(build.as_bytes());
    out.extend(id);
    out.resize(64, 0);
    out.extend(data);
    out.extend(constants);
    Ok(out)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

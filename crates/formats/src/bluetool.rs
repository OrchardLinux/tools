//! Bluetooth firmware embedded in Apple's BlueTool binary: HCD command
//! streams of Write RAM records ending with Launch RAM.

use crate::{macho, Error, Result};

const WRITE_RAM: u16 = 0xfc4c;
const LAUNCH_RAM: u16 = 0xfc4e;

/// Every HCD stream in `data`, in order. Callers pick the one they need,
/// usually by its hash.
pub fn hcd_streams(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(found) = find(&data[offset..], &WRITE_RAM.to_le_bytes()) {
        let start = offset + found;
        let end = stream_end(data, start);
        if end == start {
            offset = start + 1;
        } else {
            out.push(&data[start..end]);
            offset = end;
        }
    }
    out
}

/// A regulatory table BlueTool sends as one command: `size` bytes at the
/// virtual address `address`, which a board's recipe records from its native
/// registration. Only bytes the file holds, in the one segment that maps
/// the whole range, are taken; the recipe pins their hash.
pub fn regulatory(data: &[u8], address: u64, size: u64) -> Result<&[u8]> {
    if !(1..=255).contains(&size) {
        return Err(Error("a regulatory table is one command: 1 to 255 bytes"));
    }
    let header = macho::parse(data, 0)?;
    let mut mapping = header
        .segments
        .iter()
        .filter(|s| s.vmaddr <= address && address + size <= s.vmaddr.saturating_add(s.filesize));
    let (Some(segment), None) = (mapping.next(), mapping.next()) else {
        return Err(Error("regulatory range does not map to one file-backed segment"));
    };
    let offset = segment.fileoff + (address - segment.vmaddr);
    data.get(offset as usize..(offset + size) as usize)
        .ok_or(Error("regulatory table extends past the file"))
}

fn stream_end(data: &[u8], start: usize) -> usize {
    let mut end = start;
    while end + 3 <= data.len() {
        let opcode = u16::from_le_bytes([data[end], data[end + 1]]);
        let len = usize::from(data[end + 2]);
        if !matches!(opcode, WRITE_RAM | LAUNCH_RAM) || end + 3 + len > data.len() {
            break;
        }
        // Newer streams open with a BRCMcfgS record and can follow each other
        // directly, so such a record starts the next stream.
        if end != start && data.get(end + 7..end + 15) == Some(b"BRCMcfgS") {
            break;
        }
        end += 3 + len;
        if opcode == LAUNCH_RAM {
            break;
        }
    }
    end
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(opcode: u16, payload: &[u8]) -> Vec<u8> {
        let mut r = opcode.to_le_bytes().to_vec();
        r.push(payload.len() as u8);
        r.extend(payload);
        r
    }

    #[test]
    fn finds_streams_between_other_bytes() {
        let mut first = record(WRITE_RAM, b"\0\0\0\0BRCMcfgS..");
        first.extend(record(WRITE_RAM, &[1, 2, 3]));
        first.extend(record(LAUNCH_RAM, &[0; 4]));
        let mut second = record(WRITE_RAM, b"\0\0\0\0BRCMcfgS!!");
        second.extend(record(LAUNCH_RAM, &[]));
        let mut data = b"junk\x4c".to_vec();
        data.extend(&first);
        data.extend(&second);
        data.extend(b"tail");
        assert_eq!(hcd_streams(&data), [&first[..], &second[..]]);
    }
}

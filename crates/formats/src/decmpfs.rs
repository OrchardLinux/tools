//! Files compressed by the filesystem (decmpfs), as APFS stores them: a
//! header in an extended attribute, and the data either after the header or
//! in a resource fork in 64 KiB chunks.
use crate::{lzfse, zlib, Error, Result};

const CHUNK: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Zlib,
    Lzvn,
    Lzfse,
    /// Stored as is.
    None,
}

/// Where the compressed data is and how it is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub codec: Codec,
    /// The data is in the `com.apple.ResourceFork` attribute.
    pub in_fork: bool,
    pub size: u64,
}

/// The attribute that holds the header.
pub const ATTRIBUTE: &str = "com.apple.decmpfs";
/// The attribute that holds the data when it is too big for the header.
pub const RESOURCE_FORK: &str = "com.apple.ResourceFork";

impl Header {
    /// Reads the 16-byte header. The rest of the attribute is inline data.
    pub fn parse(attribute: &[u8]) -> Result<Header> {
        if attribute.get(..4) != Some(b"fpmc") || attribute.len() < 16 {
            return Err(Error("bad decmpfs header"));
        }
        let kind = u32::from_le_bytes(attribute[4..8].try_into().unwrap());
        let size = u64::from_le_bytes(attribute[8..16].try_into().unwrap());
        let (codec, in_fork) = match kind {
            1 | 9 => (Codec::None, false),
            3 => (Codec::Zlib, false),
            4 => (Codec::Zlib, true),
            7 => (Codec::Lzvn, false),
            8 => (Codec::Lzvn, true),
            11 => (Codec::Lzfse, false),
            12 => (Codec::Lzfse, true),
            _ => return Err(Error("unsupported decmpfs compression type")),
        };
        Ok(Header {
            codec,
            in_fork,
            size,
        })
    }
}

/// Decodes data stored after the header.
pub fn inline(header: &Header, data: &[u8]) -> Result<Vec<u8>> {
    let size = usize::try_from(header.size).map_err(|_| Error("decmpfs file too large"))?;
    match (header.codec, data.first()) {
        (Codec::None, Some(0xcc)) => stored(&data[1..], size),
        (Codec::None, _) => stored(data, size),
        // A first byte marks data that was stored because it didn't compress.
        (Codec::Zlib, Some(b)) if b & 0x0f == 0x0f => stored(&data[1..], size),
        (Codec::Lzvn, Some(6)) => stored(&data[1..], size),
        (codec, _) => decode(codec, data, size),
    }
}

/// Decodes data stored in the resource fork.
pub fn resource(header: &Header, fork: &[u8]) -> Result<Vec<u8>> {
    let size = usize::try_from(header.size).map_err(|_| Error("decmpfs file too large"))?;
    let chunks = size.div_ceil(CHUNK);
    let ranges = match header.codec {
        Codec::Zlib => classic_table(fork, chunks)?,
        Codec::Lzvn | Codec::Lzfse => offset_table(fork, chunks)?,
        Codec::None => return Err(Error("decmpfs resource fork without compression")),
    };
    let mut out = Vec::with_capacity(size);
    for range in ranges {
        let data = &fork[range];
        let want = (size - out.len()).min(CHUNK);
        let piece = match (header.codec, data.first()) {
            (Codec::Zlib, Some(0xff)) => stored(&data[1..], want)?,
            (Codec::Lzvn, Some(6)) if data.len() == want + 1 => stored(&data[1..], want)?,
            (Codec::Lzvn | Codec::Lzfse, _) if data.len() == want => data.to_vec(),
            (codec, _) => decode(codec, data, want)?,
        };
        out.extend(piece);
    }
    Ok(out)
}

/// The zlib layout: a resource fork header, then a count and (offset, length)
/// pairs relative to the start of the table.
fn classic_table(fork: &[u8], chunks: usize) -> Result<Vec<std::ops::Range<usize>>> {
    let base = be32(fork, 0)? as usize + 4;
    if le32(fork, base)? as usize != chunks {
        return Err(Error("decmpfs chunk count does not match the file size"));
    }
    let table_end = base + 4 + 8 * chunks;
    (0..chunks)
        .map(|i| {
            let at = base + 4 + 8 * i;
            let start = base + le32(fork, at)? as usize;
            let end = start + le32(fork, at + 4)? as usize;
            range(fork, start, end, table_end)
        })
        .collect()
}

/// The LZVN and LZFSE layout: chunks + 1 offsets from the start of the fork.
fn offset_table(fork: &[u8], chunks: usize) -> Result<Vec<std::ops::Range<usize>>> {
    let table_end = 4 * (chunks + 1);
    (0..chunks)
        .map(|i| {
            range(
                fork,
                le32(fork, 4 * i)? as usize,
                le32(fork, 4 * (i + 1))? as usize,
                table_end,
            )
        })
        .collect()
}

fn range(
    fork: &[u8],
    start: usize,
    end: usize,
    table_end: usize,
) -> Result<std::ops::Range<usize>> {
    if start < table_end || end <= start || end > fork.len() {
        return Err(Error("decmpfs chunk is out of range"));
    }
    Ok(start..end)
}

fn decode(codec: Codec, data: &[u8], size: usize) -> Result<Vec<u8>> {
    match codec {
        // Resource fork chunks can be padded after the stream.
        Codec::Zlib => zlib::decode_padded(data, size),
        Codec::Lzvn => lzfse::decode_lzvn(data, size),
        Codec::Lzfse => lzfse::decode(data, size),
        Codec::None => stored(data, size),
    }
}

fn stored(data: &[u8], size: usize) -> Result<Vec<u8>> {
    if data.len() != size {
        return Err(Error("decmpfs data has the wrong size"));
    }
    Ok(data.to_vec())
}

fn le32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4)
            .ok_or(Error("truncated decmpfs table"))?
            .try_into()
            .unwrap(),
    ))
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        data.get(at..at + 4)
            .ok_or(Error("truncated decmpfs table"))?
            .try_into()
            .unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: u32, size: u64) -> Header {
        let mut h = b"fpmc".to_vec();
        h.extend(kind.to_le_bytes());
        h.extend(size.to_le_bytes());
        Header::parse(&h).unwrap()
    }

    #[test]
    fn inline_stored_forms() {
        assert_eq!(inline(&header(1, 3), b"abc").unwrap(), b"abc");
        assert_eq!(inline(&header(3, 3), b"\x0fabc").unwrap(), b"abc");
        assert_eq!(inline(&header(7, 3), b"\x06abc").unwrap(), b"abc");
        assert!(inline(&header(1, 4), b"abc").is_err());
    }

    #[test]
    fn lzvn_fork_with_offset_table() {
        // Two chunks: one stored at full size, one LZVN-encoded.
        let size = CHUNK + 3;
        let first = vec![5u8; CHUNK];
        let second = [0xe3, b'x', b'y', b'z', 6, 0, 0, 0, 0, 0, 0, 0];
        let mut fork = Vec::new();
        let start = 12u32;
        fork.extend(start.to_le_bytes());
        fork.extend((start + CHUNK as u32).to_le_bytes());
        fork.extend((start + CHUNK as u32 + second.len() as u32).to_le_bytes());
        fork.extend(&first);
        fork.extend(second);
        let out = resource(&header(8, size as u64), &fork).unwrap();
        assert_eq!(&out[..CHUNK], &first[..]);
        assert_eq!(&out[CHUNK..], b"xyz");
    }

    #[test]
    fn unknown_type_is_refused() {
        assert!(Header::parse(b"fpmc\x02\0\0\0\0\0\0\0\0\0\0\0").is_err());
    }
}

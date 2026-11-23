//! ZIP layout: the end record, the central directory and local headers.
//! The caller fetches the bytes; these functions only read them.
use crate::{Error, Result};

/// The most bytes from the end of the archive that can hold the end record:
/// its 22 bytes plus the longest comment.
pub const MAX_TAIL: usize = 22 + 0xffff;
/// Size of the ZIP64 end locator, which sits just before the end record.
pub const ZIP64_LOCATOR: usize = 20;
/// Size of the ZIP64 end record without its extensible data.
pub const ZIP64_END: usize = 56;
/// Size of a local header without its name and extra field.
pub const LOCAL_HEADER: usize = 30;

/// Where the central directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directory {
    pub offset: u64,
    pub size: u64,
    pub count: u64,
    /// Where the end records start. The directory must end before this.
    pub end: u64,
}

/// What the end record says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Directory(Directory),
    /// The directory location is in the ZIP64 records. Read
    /// [`ZIP64_LOCATOR`] bytes at `locator` and pass them to [`zip64_locator`].
    Zip64 {
        locator: u64,
    },
}

/// Finds the end record in the last bytes of the archive. `tail_offset` is
/// where `tail` starts in the archive.
pub fn end(tail: &[u8], tail_offset: u64) -> Result<End> {
    // Search backwards and require the comment length to reach exactly to the
    // end, so a signature inside a comment is not mistaken for the record.
    let at = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..].starts_with(b"PK\x05\x06")
                && le16(tail, i + 20).is_ok_and(|n| i + 22 + usize::from(n) == tail.len())
        })
        .ok_or(Error("ZIP end record not found"))?;
    if le16(tail, at + 4)? != 0
        || le16(tail, at + 6)? != 0
        || le16(tail, at + 8)? != le16(tail, at + 10)?
    {
        return Err(Error("multi-disk ZIP archives are not supported"));
    }
    let count = le16(tail, at + 10)?;
    let size = le32(tail, at + 12)?;
    let offset = le32(tail, at + 16)?;
    let end = tail_offset + at as u64;
    if count == u16::MAX || size == u32::MAX || offset == u32::MAX {
        let locator = end
            .checked_sub(ZIP64_LOCATOR as u64)
            .ok_or(Error("ZIP64 locator missing"))?;
        return Ok(End::Zip64 { locator });
    }
    Ok(End::Directory(Directory {
        offset: offset.into(),
        size: size.into(),
        count: count.into(),
        end,
    }))
}

/// Reads the ZIP64 locator and returns the offset of the ZIP64 end record.
pub fn zip64_locator(bytes: &[u8]) -> Result<u64> {
    if !bytes.starts_with(b"PK\x06\x07") || le32(bytes, 4)? != 0 || le32(bytes, 16)? != 1 {
        return Err(Error("bad ZIP64 locator"));
    }
    le64(bytes, 8)
}

/// Reads the ZIP64 end record found at `offset`.
pub fn zip64_end(bytes: &[u8], offset: u64) -> Result<Directory> {
    if !bytes.starts_with(b"PK\x06\x06")
        || le64(bytes, 4)? < 44
        || le32(bytes, 16)? != 0
        || le32(bytes, 20)? != 0
        || le64(bytes, 24)? != le64(bytes, 32)?
    {
        return Err(Error("bad ZIP64 end record"));
    }
    Ok(Directory {
        count: le64(bytes, 32)?,
        size: le64(bytes, 40)?,
        offset: le64(bytes, 48)?,
        end: offset,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflate,
    Other(u16),
}

/// One member as the central directory describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub method: Method,
    pub encrypted: bool,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    /// Offset of the member's local header.
    pub header: u64,
    flags: u16,
}

/// Reads the central directory.
pub fn entries(directory: &[u8], count: u64) -> Result<Vec<Entry>> {
    // Each record takes at least 46 bytes, which bounds the count.
    if count > directory.len() as u64 / 46 {
        return Err(Error("ZIP directory count exceeds its size"));
    }
    let mut out = Vec::with_capacity(count as usize);
    let mut p = 0;
    for _ in 0..count {
        if !directory[p..].starts_with(b"PK\x01\x02") {
            return Err(Error("bad ZIP directory record"));
        }
        if le16(directory, p + 34)? != 0 {
            return Err(Error("multi-disk ZIP archives are not supported"));
        }
        let name_len = usize::from(le16(directory, p + 28)?);
        let extra_len = usize::from(le16(directory, p + 30)?);
        let comment_len = usize::from(le16(directory, p + 32)?);
        let name = std::str::from_utf8(field(directory, p + 46, name_len)?)
            .map_err(|_| Error("ZIP member name is not UTF-8"))?;
        let flags = le16(directory, p + 8)?;
        let mut entry = Entry {
            name: name.to_owned(),
            method: match le16(directory, p + 10)? {
                0 => Method::Stored,
                8 => Method::Deflate,
                other => Method::Other(other),
            },
            // Bit 0: encrypted. Bit 6: strong encryption.
            encrypted: flags & (1 | 1 << 6) != 0,
            crc32: le32(directory, p + 16)?,
            compressed_size: le32(directory, p + 20)?.into(),
            size: le32(directory, p + 24)?.into(),
            header: le32(directory, p + 42)?.into(),
            flags,
        };
        zip64_sizes(&mut entry, field(directory, p + 46 + name_len, extra_len)?)?;
        out.push(entry);
        p += 46 + name_len + extra_len + comment_len;
        if p > directory.len() {
            return Err(Error("truncated ZIP directory"));
        }
    }
    if p != directory.len() {
        return Err(Error("unexpected data after ZIP directory"));
    }
    Ok(out)
}

/// Replaces 32-bit fields that are all ones with their ZIP64 values.
fn zip64_sizes(entry: &mut Entry, mut extra: &[u8]) -> Result<()> {
    while !extra.is_empty() {
        let (id, len) = (le16(extra, 0)?, usize::from(le16(extra, 2)?));
        let value = field(extra, 4, len)?;
        if id == 1 {
            let mut p = 0;
            for n in [
                &mut entry.size,
                &mut entry.compressed_size,
                &mut entry.header,
            ] {
                if *n == u64::from(u32::MAX) {
                    *n = le64(value, p)?;
                    p += 8;
                }
            }
        }
        extra = &extra[4 + len..];
    }
    if [entry.size, entry.compressed_size, entry.header].contains(&u64::from(u32::MAX)) {
        return Err(Error("ZIP64 sizes missing"));
    }
    Ok(())
}

/// The variable-length part after a local header's fixed 30 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub name_len: usize,
    pub extra_len: usize,
}

/// Checks a member's local header against its directory entry.
pub fn local_header(bytes: &[u8], entry: &Entry) -> Result<Local> {
    let method = match entry.method {
        Method::Stored => 0,
        Method::Deflate => 8,
        Method::Other(m) => m,
    };
    if !bytes.starts_with(b"PK\x03\x04")
        || le16(bytes, 6)? != entry.flags
        || le16(bytes, 8)? != method
    {
        return Err(Error("ZIP local header disagrees with the directory"));
    }
    Ok(Local {
        name_len: le16(bytes, 26)?.into(),
        extra_len: le16(bytes, 28)?.into(),
    })
}

fn field(data: &[u8], p: usize, len: usize) -> Result<&[u8]> {
    data.get(p..p.checked_add(len).ok_or(Error("ZIP offset overflow"))?)
        .ok_or(Error("truncated ZIP structure"))
}

fn le16(d: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(field(d, p, 2)?.try_into().unwrap()))
}

fn le32(d: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(field(d, p, 4)?.try_into().unwrap()))
}

fn le64(d: &[u8], p: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(field(d, p, 8)?.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end_record(count: u16, size: u32, offset: u32, comment: &[u8]) -> Vec<u8> {
        let mut r = b"PK\x05\x06".to_vec();
        r.extend([0, 0, 0, 0]);
        r.extend(count.to_le_bytes());
        r.extend(count.to_le_bytes());
        r.extend(size.to_le_bytes());
        r.extend(offset.to_le_bytes());
        r.extend((comment.len() as u16).to_le_bytes());
        r.extend(comment);
        r
    }

    #[test]
    fn end_record_behind_a_comment_with_a_signature() {
        let mut tail = vec![0; 10];
        tail.extend(end_record(3, 200, 1000, b"PK\x05\x06 not a record"));
        let found = end(&tail, 5000).unwrap();
        assert_eq!(
            found,
            End::Directory(Directory {
                offset: 1000,
                size: 200,
                count: 3,
                end: 5010
            })
        );
    }

    #[test]
    fn zip64_is_signalled() {
        let tail = end_record(u16::MAX, u32::MAX, u32::MAX, b"");
        assert_eq!(end(&tail, 100).unwrap(), End::Zip64 { locator: 80 });
    }

    #[test]
    fn no_end_record() {
        assert!(end(&[0; 64], 0).is_err());
    }
}

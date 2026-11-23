//! APFS on-disk structures: object checksums, the container and volume
//! superblocks, B-tree nodes and filesystem records. The caller reads blocks.
use crate::{Error, Result};
use std::ops::Range;

/// Object types, from the low 16 bits of an object header's type field.
pub const NX_SUPERBLOCK: u32 = 1;
pub const BTREE: u32 = 2;
pub const BTREE_NODE: u32 = 3;
pub const OMAP: u32 = 11;
pub const FS: u32 = 13;

/// Filesystem record types, from the top 4 bits of a record key.
pub const INODE: u8 = 3;
pub const XATTR: u8 = 4;
pub const FILE_EXTENT: u8 = 8;
pub const DIR_REC: u8 = 9;

/// The inode number of a volume's root directory.
pub const ROOT_DIR: u64 = 2;

const OID_MASK: u64 = (1 << 60) - 1;
const PHYSICAL: u32 = 0x4000_0000;
const EPHEMERAL: u32 = 0x8000_0000;

/// The common header of every APFS object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub oid: u64,
    pub xid: u64,
    pub kind: u32,
}

/// Checks an object's checksum and reads its header.
pub fn object(block: &[u8]) -> Result<Header> {
    if block.len() < 32 || !block.len().is_multiple_of(4) {
        return Err(Error("APFS object too small"));
    }
    if le64(block, 0)? != fletcher64(&block[8..]) {
        return Err(Error("APFS object checksum mismatch"));
    }
    Ok(Header {
        oid: le64(block, 8)?,
        xid: le64(block, 16)?,
        kind: le32(block, 24)? & 0xffff,
    })
}

fn fletcher64(data: &[u8]) -> u64 {
    let m = u64::from(u32::MAX);
    let (mut a, mut b) = (0u64, 0u64);
    for word in data.as_chunks::<4>().0 {
        a = (a + u64::from(u32::from_le_bytes(*word))) % m;
        b = (b + a) % m;
    }
    let low = m - (a + b) % m;
    let high = m - (a + low) % m;
    high << 32 | low
}

/// Reads the block size from the first 4096 bytes of a container.
pub fn block_size(head: &[u8]) -> Result<usize> {
    if head.get(32..36) != Some(b"NXSB") {
        return Err(Error("not an APFS container"));
    }
    let size = le32(head, 36)? as usize;
    if !(4096..=65536).contains(&size) || !size.is_power_of_two() {
        return Err(Error("unsupported APFS block size"));
    }
    Ok(size)
}

/// The container superblock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub xid: u64,
    pub block_size: usize,
    pub blocks: u64,
    pub uuid: [u8; 16],
    /// The checkpoint descriptor area: first block and length.
    pub checkpoints: (u64, u64),
    /// Physical address of the container object map.
    pub omap: u64,
    /// Object ids of the volumes.
    pub volumes: Vec<u64>,
}

impl Container {
    pub fn parse(block: &[u8]) -> Result<Container> {
        let header = object(block)?;
        if header.kind != NX_SUPERBLOCK || block_size(block)? != block.len() || header.xid == 0 {
            return Err(Error("bad APFS container superblock"));
        }
        // Only the version and case-sensitivity features; no Fusion drives.
        if le64(block, 64)? & !3 != 0 {
            return Err(Error("APFS container uses unsupported features"));
        }
        let descriptors = le32(block, 104)?;
        if descriptors & 0x8000_0000 != 0 {
            return Err(Error("APFS checkpoint area is not contiguous"));
        }
        let count = le32(block, 180)? as usize;
        if count > 100 {
            return Err(Error("APFS container lists too many volumes"));
        }
        let mut volumes = Vec::new();
        for i in 0..100 {
            let oid = le64(block, 184 + 8 * i)?;
            if oid != 0 {
                volumes.push(oid);
            }
        }
        Ok(Container {
            xid: header.xid,
            block_size: block.len(),
            blocks: le64(block, 40)?,
            uuid: block[72..88].try_into().unwrap(),
            checkpoints: (le64(block, 112)?, descriptors.into()),
            omap: le64(block, 160)?,
            volumes,
        })
    }
}

/// Reads an object map object and returns the address of its tree.
pub fn omap_tree(block: &[u8]) -> Result<u64> {
    if object(block)?.kind != OMAP || le32(block, 40)? & 0xc000_0000 != PHYSICAL {
        return Err(Error("bad APFS object map"));
    }
    le64(block, 48)
}

/// An object map entry: where a virtual object lives at a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub oid: u64,
    pub xid: u64,
    pub deleted: bool,
    pub block: u64,
}

pub fn mapping(key: &[u8], value: &[u8], block_size: usize) -> Result<Mapping> {
    let flags = le32(value, 0)?;
    let deleted = flags & 1 != 0;
    // Bit 1 (salted checksum) is harmless; encryption and the rest are not.
    if flags & !3 != 0 || (!deleted && le32(value, 4)? as usize != block_size) {
        return Err(Error("APFS object map entry uses unsupported features"));
    }
    Ok(Mapping {
        oid: le64(key, 0)?,
        xid: le64(key, 8)?,
        deleted,
        block: le64(value, 8)?,
    })
}

/// A volume superblock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub oid: u64,
    pub xid: u64,
    pub name: String,
    /// Physical address of the volume object map.
    pub omap: u64,
    /// The filesystem tree root, and whether it is mapped through the omap.
    pub root: u64,
    pub root_virtual: bool,
    /// The separate file extent tree of sealed volumes, or 0.
    pub extents: u64,
    /// Directory entry keys carry a name hash.
    pub hashed_names: bool,
}

impl Volume {
    pub fn parse(block: &[u8]) -> Result<Volume> {
        let header = object(block)?;
        if header.kind != FS || block.get(32..36) != Some(b"APSB") {
            return Err(Error("bad APFS volume superblock"));
        }
        let incompatible = le64(block, 56)?;
        if incompatible & !0x3f != 0 {
            return Err(Error("APFS volume uses unsupported features"));
        }
        if le64(block, 264)? & 1 == 0 {
            return Err(Error("APFS volume is encrypted"));
        }
        let tree = le32(block, 116)?;
        if tree & EPHEMERAL != 0 {
            return Err(Error("APFS volume tree is ephemeral"));
        }
        let name = bytes(block, 704, 256)?;
        let name = &name[..name
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error("APFS volume name is not terminated"))?];
        Ok(Volume {
            oid: header.oid,
            xid: header.xid,
            name: std::str::from_utf8(name)
                .map_err(|_| Error("APFS volume name is not UTF-8"))?
                .to_owned(),
            omap: le64(block, 128)?,
            root: le64(block, 136)?,
            root_virtual: tree & PHYSICAL == 0,
            extents: le64(block, 1032)?,
            // Case-insensitive (bit 0) or normalization-insensitive (bit 3).
            hashed_names: incompatible & 0b1001 != 0,
        })
    }
}

/// Which tree a node belongs to, which decides how its keys sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tree {
    /// Object map and file extent trees: keys start with a plain id.
    Plain,
    /// Filesystem trees: keys start with an id and a record type.
    Files,
}

impl Tree {
    /// The part of a key that searches compare: the id and, for filesystem
    /// trees, the record type.
    pub fn prefix(self, key: &[u8]) -> Result<(u64, u8)> {
        let word = le64(key, 0)?;
        Ok(match self {
            Tree::Plain => (word, 0),
            Tree::Files => (word & OID_MASK, (word >> 60) as u8),
        })
    }
}

/// A B-tree node.
pub struct Node<'a> {
    pub oid: u64,
    pub xid: u64,
    pub level: u16,
    pub root: bool,
    data: &'a [u8],
    rows: Vec<(Range<usize>, Option<Range<usize>>)>,
}

impl<'a> Node<'a> {
    pub fn parse(block: &'a [u8]) -> Result<Node<'a>> {
        let header = object(block)?;
        if !matches!(header.kind, BTREE | BTREE_NODE) {
            return Err(Error("APFS object is not a B-tree node"));
        }
        let flags = le16(block, 32)?;
        let (root, leaf, fixed) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
        let level = le16(block, 34)?;
        if flags & !7 != 0 || leaf != (level == 0) {
            return Err(Error("bad APFS B-tree node flags"));
        }
        // Root nodes end with 40 bytes of tree information.
        let values_end = block
            .len()
            .checked_sub(if root { 40 } else { 0 })
            .ok_or(Error("APFS root node too small"))?;
        let count = le32(block, 36)? as usize;
        let toc = 56 + usize::from(le16(block, 40)?);
        let keys = toc + usize::from(le16(block, 42)?);
        let width = if fixed { 4 } else { 8 };
        if keys > values_end || count * width > keys - toc {
            return Err(Error("APFS node table is out of range"));
        }
        let mut rows = Vec::with_capacity(count);
        for i in 0..count {
            let at = toc + i * width;
            let key_at = keys + usize::from(le16(block, at)?);
            let (key_len, value_off, value_len) = if fixed {
                (16, le16(block, at + 2)?, if leaf { 16 } else { 8 })
            } else {
                (
                    usize::from(le16(block, at + 2)?),
                    le16(block, at + 4)?,
                    usize::from(le16(block, at + 6)?),
                )
            };
            if key_len == 0 || key_at + key_len > values_end {
                return Err(Error("APFS node key is out of range"));
            }
            // 0xffff marks a ghost record without a value.
            let value = if value_off == u16::MAX {
                None
            } else {
                let start = values_end
                    .checked_sub(usize::from(value_off))
                    .filter(|&s| s >= keys);
                match start {
                    Some(s) if s + value_len <= values_end && (leaf || value_len >= 8) => {
                        Some(s..s + value_len)
                    }
                    _ => return Err(Error("APFS node value is out of range")),
                }
            };
            if value.is_none() && !leaf {
                return Err(Error("APFS index node has a ghost record"));
            }
            rows.push((key_at..key_at + key_len, value));
        }
        Ok(Node {
            oid: header.oid,
            xid: header.xid,
            level,
            root,
            data: block,
            rows,
        })
    }

    pub fn leaf(&self) -> bool {
        self.level == 0
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn key(&self, i: usize) -> &'a [u8] {
        &self.data[self.rows[i].0.clone()]
    }

    pub fn value(&self, i: usize) -> Option<&'a [u8]> {
        self.rows[i].1.clone().map(|r| &self.data[r])
    }

    /// The child id stored in an index node row.
    pub fn child(&self, i: usize) -> Result<u64> {
        le64(
            self.value(i).ok_or(Error("APFS index row has no child"))?,
            0,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    /// The inode number of the entry.
    pub inode: u64,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

pub fn dir_entry(key: &[u8], value: &[u8], hashed_names: bool) -> Result<DirEntry> {
    let name = if hashed_names {
        name(key, 12, (le32(key, 8)? & 0x3ff) as usize)?
    } else {
        name(key, 10, usize::from(le16(key, 8)?))?
    };
    let kind = match le16(value, 16)? & 0xf {
        4 => EntryKind::Directory,
        8 => EntryKind::File,
        10 => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    Ok(DirEntry {
        name,
        inode: le64(value, 0)?,
        kind,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inode {
    /// The id the file's data extents are filed under.
    pub data_id: u64,
    pub compressed: bool,
    pub mode: u16,
    pub size: u64,
}

impl Inode {
    pub fn is_dir(&self) -> bool {
        self.mode & 0xf000 == 0x4000
    }

    pub fn is_file(&self) -> bool {
        self.mode & 0xf000 == 0x8000
    }
}

pub fn inode(value: &[u8]) -> Result<Inode> {
    // Extended fields follow the 92-byte fixed part: a count, then a 4-byte
    // descriptor per field, then the field data in 8-byte steps.
    let count = usize::from(le16(value, 92)?);
    let mut data = 96 + 4 * count;
    let mut size = 0;
    for i in 0..count {
        let descriptor = 96 + 4 * i;
        let len = usize::from(le16(value, descriptor + 2)?);
        // Type 8 is the data stream; its first field is the size.
        if bytes(value, descriptor, 1)?[0] == 8 {
            size = le64(value, data)?;
        }
        data += len.div_ceil(8) * 8;
    }
    Ok(Inode {
        data_id: le64(value, 8)?,
        compressed: le32(value, 68)? & 0x20 != 0,
        mode: le16(value, 80)?,
        size,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Xattr<'a> {
    Embedded(&'a [u8]),
    /// Stored like file data, under `id`.
    Stream {
        id: u64,
        size: u64,
    },
}

pub fn xattr<'a>(key: &[u8], value: &'a [u8]) -> Result<(String, Xattr<'a>)> {
    let name = name(key, 10, usize::from(le16(key, 8)?))?;
    let flags = le16(value, 0)?;
    let data = bytes(value, 4, usize::from(le16(value, 2)?))?;
    let attr = match flags & 3 {
        2 => Xattr::Embedded(data),
        1 => Xattr::Stream {
            id: le64(data, 0)?,
            size: le64(data, 8)?,
        },
        _ => return Err(Error("bad APFS extended attribute")),
    };
    Ok((name, attr))
}

/// A run of file data. Block 0 means zeros.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub logical: u64,
    pub len: u64,
    pub block: u64,
}

/// Reads a file extent record, from a filesystem tree or a file extent tree.
pub fn extent(key: &[u8], value: &[u8]) -> Result<Extent> {
    Ok(Extent {
        logical: le64(key, 8)?,
        len: le64(value, 0)? & ((1 << 56) - 1),
        block: le64(value, 8)?,
    })
}

fn name(key: &[u8], at: usize, len: usize) -> Result<String> {
    let raw = bytes(key, at, len)?;
    match raw.split_last() {
        Some((0, text)) if !text.contains(&0) && at + len == key.len() => std::str::from_utf8(text)
            .map(str::to_owned)
            .map_err(|_| Error("APFS name is not UTF-8")),
        _ => Err(Error("bad APFS name")),
    }
}

fn bytes(data: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    data.get(at..at.checked_add(len).ok_or(Error("APFS offset overflow"))?)
        .ok_or(Error("truncated APFS record"))
}

fn le16(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(data, at, 2)?.try_into().unwrap()))
}

fn le32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, at, 4)?.try_into().unwrap()))
}

fn le64(data: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, at, 8)?.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fletcher64_reference() {
        // A block whose stored checksum covers the rest: zeros check out to
        // all ones in both halves.
        assert_eq!(fletcher64(&[0; 16]), 0xffff_ffff_ffff_ffff);
        let mut block = vec![0u8; 64];
        block[8..12].copy_from_slice(&1u32.to_le_bytes());
        let sum = fletcher64(&block[8..]);
        block[..8].copy_from_slice(&sum.to_le_bytes());
        assert!(object(&block).is_ok());
        block[40] = 1;
        assert!(object(&block).is_err());
    }

    #[test]
    fn names_need_one_terminator() {
        let mut key = vec![0; 10];
        key.extend(b"abc\0");
        assert_eq!(name(&key, 10, 4).unwrap(), "abc");
        assert!(name(&key, 10, 3).is_err());
        key[11] = 0;
        assert!(name(&key, 10, 4).is_err());
    }
}

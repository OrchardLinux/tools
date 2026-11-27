//! LwVM, the partition scheme of the 32-bit boards' flash: one 4 KiB header
//! at the start of the medium and a copy at the end of its first chunk. The
//! medium is cut into chunks; the header lists up to eight partitions and
//! says, for every physical chunk, which partition's which logical chunk it
//! holds. All offsets are bytes; identifiers are stored in network order.
//!
//! As orchard's Python (`lwvm.py`, from Shayne Legault's LwVM installer),
//! which this gives the same answers as, byte for byte.

use crate::{Error, Result};

pub const MAGIC: [u8; 16] = [0x6a, 0x90, 0x88, 0xcf, 0x8a, 0xfd, 0x63, 0x0a, 0xe3, 0x51, 0xe2, 0x48, 0x87, 0xe0, 0xb9, 0x8b];
pub const HEADER_BYTES: usize = 4096;
/// The type of a Linux partition.
pub const LINUX_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// The type of an HFS+ partition (iOS's system and data).
pub const HFS_TYPE: &str = "48465300-0000-11aa-aa11-00306543ecac";
/// A physical chunk nobody owns.
const FREE_CHUNK: u16 = 0xf111;
const MOST_PARTITIONS: usize = 8;
const DESCRIPTORS: usize = 0x200;
const DESCRIPTOR: usize = 128;
const MAP: usize = 0x800;
const MAP_WORDS: usize = 1024;

/// A run of a partition that lies in one piece on the medium.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    pub logical: u64,
    pub physical: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Counted from 1, in the header's order.
    pub number: usize,
    pub kind: String,
    pub partuuid: String,
    /// Its range in the virtual space the header describes.
    pub begin: u64,
    pub end: u64,
    pub bytes: u64,
    pub attributes: u64,
    pub name: String,
    pub extents: Vec<Extent>,
}

/// A header pair that was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub media_bytes: u64,
    pub chunk_bytes: u64,
    /// Where the header's copy lies.
    pub backup_offset: u64,
    pub partitions: Vec<Partition>,
}

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn le64(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

fn word(header: &[u8], physical: usize) -> u16 {
    u16::from_le_bytes(header[MAP + physical * 2..MAP + physical * 2 + 2].try_into().unwrap())
}

/// An identifier as text: the 16 bytes in network order.
fn uuid_text(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

fn uuid_bytes(text: &str) -> Result<[u8; 16]> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || text.len() != 36 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error("not a partition identifier"));
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| Error("not a partition identifier"))?;
    }
    Ok(bytes)
}

/// The size of a chunk on a medium of `media_bytes`: the power of two that
/// cuts it into at most 1024 chunks.
pub fn chunk_size(media_bytes: u64) -> Result<u64> {
    if media_bytes < 8192 || media_bytes % HEADER_BYTES as u64 != 0 {
        return Err(Error("LwVM medium size is unsupported"));
    }
    let shifted = (media_bytes - 1) >> 10;
    if shifted == 0 || shifted > 0xffff_ffff {
        return Err(Error("LwVM chunk geometry is unsupported"));
    }
    let chunk = 1u64 << (64 - shifted.leading_zeros());
    if chunk < 2 * HEADER_BYTES as u64 || media_bytes / chunk < 2 {
        return Err(Error("LwVM chunk geometry is unsupported"));
    }
    Ok(chunk)
}

/// The header's CRC-32, over the header with its checksum field zero.
pub fn checksum(header: &[u8]) -> u32 {
    let mut checked = header.to_vec();
    checked[0x2c..0x30].fill(0);
    crc32fast::hash(&checked)
}

/// Checks a header and its copy and reads the partitions. `media_bytes` is
/// the medium's size as its provider gives it, where that is known.
pub fn parse(primary: &[u8], backup: &[u8], media_bytes: Option<u64>) -> Result<Snapshot> {
    if primary.len() != HEADER_BYTES || primary != backup || primary[..16] != MAGIC {
        return Err(Error("LwVM header pair is incomplete or disagrees"));
    }
    if checksum(primary) != le32(primary, 0x2c) {
        return Err(Error("LwVM header CRC differs"));
    }
    let (size, count) = (le64(primary, 0x20), le32(primary, 0x28) as usize);
    let chunk = chunk_size(size)?;
    if media_bytes.is_some_and(|media| media != size) {
        return Err(Error("LwVM medium size differs from the provider"));
    }
    if primary[0x10..0x20].iter().all(|b| *b == 0) {
        return Err(Error("LwVM medium identity is zero"));
    }
    if !(1..=MOST_PARTITIONS).contains(&count) {
        return Err(Error("LwVM descriptor count is unsupported"));
    }
    let mut parts: Vec<Partition> = Vec::new();
    for index in 0..count {
        let raw = &primary[DESCRIPTORS + index * DESCRIPTOR..DESCRIPTORS + (index + 1) * DESCRIPTOR];
        let (kind, identity) = (&raw[..16], &raw[16..32]);
        let (begin, end, attributes) = (le64(raw, 32), le64(raw, 40), le64(raw, 48));
        let partuuid = uuid_text(identity);
        if kind.iter().all(|b| *b == 0) || identity.iter().all(|b| *b == 0) || parts.iter().any(|p| p.partuuid == partuuid) {
            return Err(Error("LwVM partition identity is zero or repeated"));
        }
        let header = HEADER_BYTES as u64;
        if begin < header || begin >= end || end > size || begin % header != 0 || end % header != 0 {
            return Err(Error("LwVM partition range is invalid"));
        }
        if parts.iter().any(|p| begin < p.end && end > p.begin) {
            return Err(Error("LwVM virtual partition ranges overlap"));
        }
        let units: Vec<u16> = raw[56..128].chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        let name = String::from_utf16(&units).map_err(|_| Error("LwVM partition name is not text"))?;
        parts.push(Partition {
            number: index + 1,
            kind: uuid_text(kind),
            partuuid,
            begin,
            end,
            bytes: end - begin,
            attributes,
            name: name.trim_end_matches('\0').to_owned(),
            extents: Vec::new(),
        });
    }
    // Which physical chunk holds each partition's logical chunks.
    let chunks = (size / chunk) as usize;
    let mut owned: Vec<std::collections::BTreeMap<u64, u64>> = vec![Default::default(); count];
    for physical in 0..MAP_WORDS {
        let entry = word(primary, physical);
        let (owner, logical) = ((entry >> 12) as usize, u64::from(entry & 0xfff));
        if owner == 15 {
            let known = (physical == 0 && entry == 0xf000) || (physical > 0 && physical < chunks && entry == FREE_CHUNK) || (physical >= chunks && entry == 0xf3ff);
            if !known {
                return Err(Error("LwVM has an unknown allocation sentinel"));
            }
            continue;
        }
        if owner >= count || physical == 0 || physical >= chunks {
            return Err(Error("LwVM chunk owner or physical address is invalid"));
        }
        if owned[owner].insert(logical, physical as u64).is_some() {
            return Err(Error("LwVM logical chunk is aliased"));
        }
    }
    for (part, owned) in parts.iter_mut().zip(&owned) {
        let needed = part.bytes.div_ceil(chunk);
        if owned.len() as u64 != needed || !owned.keys().copied().eq(0..needed) {
            return Err(Error("LwVM partition chunk coverage is incomplete"));
        }
        for (&logical, &physical) in owned {
            let length = chunk.min(part.bytes - logical * chunk);
            let offset = physical * chunk;
            if offset + length > size {
                return Err(Error("LwVM partition extent exceeds the medium"));
            }
            match part.extents.last_mut() {
                Some(last) if last.physical + last.bytes == offset => last.bytes += length,
                _ => part.extents.push(Extent { logical: logical * chunk, physical: offset, bytes: length }),
            }
        }
    }
    Ok(Snapshot { media_bytes: size, chunk_bytes: chunk, backup_offset: chunk - HEADER_BYTES as u64, partitions: parts })
}

/// `(first physical chunk, chunks)` of the unallocated chunks that lie back
/// to back, longest first. Only such a run can hold a Linux partition the
/// kernel's LwVM reader adds: it takes one whose chunks are in order and in
/// one piece.
pub fn free_runs(header: &[u8], snapshot: &Snapshot) -> Vec<(u64, u64)> {
    let chunks = snapshot.media_bytes / snapshot.chunk_bytes;
    let (mut runs, mut start) = (Vec::new(), None);
    for physical in 1..=chunks {
        let free = physical < chunks && word(header, physical as usize) == FREE_CHUNK;
        match (free, start) {
            (true, None) => start = Some(physical),
            (false, Some(first)) => {
                runs.push((first, physical - first));
                start = None;
            }
            _ => {}
        }
    }
    runs.sort_by_key(|&(first, count)| (std::cmp::Reverse(count), first));
    runs
}

/// `(first byte, bytes)` of the virtual space behind every partition, where
/// a new entry's range goes.
pub fn virtual_room(snapshot: &Snapshot) -> (u64, u64) {
    let header = HEADER_BYTES as u64;
    let begin = snapshot.partitions.iter().map(|p| p.end).max().unwrap_or(0).max(header).div_ceil(header) * header;
    (begin, snapshot.media_bytes - begin)
}

/// `(first chunk, chunks)` a new Linux entry could take: the free runs, cut
/// to the virtual room that is left.
pub fn free_space(header: &[u8], snapshot: &Snapshot) -> Vec<(u64, u64)> {
    if snapshot.partitions.len() >= MOST_PARTITIONS {
        return Vec::new();
    }
    let most = virtual_room(snapshot).1 / snapshot.chunk_bytes;
    free_runs(header, snapshot).into_iter().map(|(first, count)| (first, count.min(most))).filter(|&(_, count)| count > 0).collect()
}

/// A new header with a Linux entry over the free chunks `first` to
/// `first + chunks - 1`, and that entry. Nothing else changes: the entry is
/// appended, its virtual range follows every other partition's, and the
/// chunks are mapped to it in order. The result is parsed again before it
/// is returned.
pub fn allocate_free(primary: &[u8], backup: &[u8], linux_uuid: &str, first: u64, chunks: u64) -> Result<(Vec<u8>, Partition)> {
    let before = parse(primary, backup, None)?;
    let count = before.partitions.len();
    let slot = DESCRIPTORS + count * DESCRIPTOR;
    if count == MOST_PARTITIONS || primary[slot..slot + DESCRIPTOR].iter().any(|b| *b != 0) {
        return Err(Error("No empty LwVM descriptor is available"));
    }
    if !free_space(primary, &before).iter().any(|&(start, length)| start <= first && first + chunks <= start + length) || chunks == 0 {
        return Err(Error("The chosen chunks are not free LwVM space"));
    }
    let identity = uuid_bytes(linux_uuid)?;
    let text = uuid_text(&identity);
    if identity.iter().all(|b| *b == 0) || before.partitions.iter().any(|p| p.partuuid == text) {
        return Err(Error("Linux partition identity is zero or repeated"));
    }
    let begin = virtual_room(&before).0;
    let end = begin + chunks * before.chunk_bytes;
    let mut header = primary.to_vec();
    header[0x28..0x2c].copy_from_slice(&(count as u32 + 1).to_le_bytes());
    let mut descriptor = [0u8; DESCRIPTOR];
    descriptor[..16].copy_from_slice(&uuid_bytes(LINUX_TYPE)?);
    descriptor[16..32].copy_from_slice(&identity);
    descriptor[32..40].copy_from_slice(&begin.to_le_bytes());
    descriptor[40..48].copy_from_slice(&end.to_le_bytes());
    for (i, unit) in "Linux".encode_utf16().enumerate() {
        descriptor[56 + i * 2..58 + i * 2].copy_from_slice(&unit.to_le_bytes());
    }
    header[slot..slot + DESCRIPTOR].copy_from_slice(&descriptor);
    for logical in 0..chunks {
        let at = MAP + (first + logical) as usize * 2;
        header[at..at + 2].copy_from_slice(&((count as u16) << 12 | logical as u16).to_le_bytes());
    }
    let sum = checksum(&header);
    header[0x2c..0x30].copy_from_slice(&sum.to_le_bytes());
    let after = parse(&header, &header, Some(before.media_bytes))?;
    let made = after.partitions.into_iter().find(|p| p.partuuid == text).ok_or(Error("LwVM partition identity was not found"))?;
    Ok((header, made))
}

//! GUID partition tables: reading both copies and making the bytes of a
//! changed table. A table is the header sector and the entry array; the
//! backup copy differs only in where it says it is.
use crate::{Error, Result};

const SIGNATURE: &[u8; 8] = b"EFI PART";
const ENTRY: usize = 128;

/// One used entry of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Its place in the entry array, from 1: the number in a device node's name.
    pub number: u32,
    pub kind: [u8; 16],
    pub guid: [u8; 16],
    pub first: u64,
    pub last: u64,
    pub name: String,
}

impl Partition {
    pub fn blocks(&self) -> u64 {
        self.last - self.first + 1
    }
}

/// A table as one copy states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The header sector, as read.
    pub header: Vec<u8>,
    /// The whole entry array, as read.
    pub entries: Vec<u8>,
    pub my_lba: u64,
    pub other_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub entries_lba: u64,
    pub count: u32,
    pub partitions: Vec<Partition>,
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Where a header says its entry array is: (LBA, count), after checking the
/// header's own checksum. The caller reads `count * 128` bytes from there.
pub fn entries_of(header: &[u8]) -> Result<(u64, u32)> {
    if header.len() < 92 || &header[..8] != SIGNATURE {
        return Err(Error("no GPT header"));
    }
    let size = le32(header, 12) as usize;
    if !(92..=header.len()).contains(&size) || le32(header, 84) as usize != ENTRY {
        return Err(Error("unusual GPT header"));
    }
    let mut copy = header[..size].to_vec();
    copy[16..20].fill(0);
    if crc32fast::hash(&copy) != le32(header, 16) {
        return Err(Error("GPT header checksum mismatch"));
    }
    let count = le32(header, 80);
    if count == 0 || count > 1024 {
        return Err(Error("unusual GPT entry count"));
    }
    Ok((le64(header, 72), count))
}

/// A table from its header sector and entry array, with both checksums checked.
pub fn parse(header: &[u8], entries: &[u8]) -> Result<Table> {
    let (entries_lba, count) = entries_of(header)?;
    if entries.len() != count as usize * ENTRY {
        return Err(Error("GPT entry array has the wrong size"));
    }
    if crc32fast::hash(entries) != le32(header, 88) {
        return Err(Error("GPT entry array checksum mismatch"));
    }
    let (first_usable, last_usable) = (le64(header, 40), le64(header, 48));
    let mut partitions = Vec::new();
    for (index, raw) in entries.as_chunks::<ENTRY>().0.iter().enumerate() {
        if raw[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let (first, last) = (le64(raw, 32), le64(raw, 40));
        if first < first_usable || last > last_usable || last < first {
            return Err(Error("a GPT partition lies outside the usable range"));
        }
        let units: Vec<u16> = raw[56..128].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
        partitions.push(Partition {
            number: index as u32 + 1,
            kind: raw[..16].try_into().unwrap(),
            guid: raw[16..32].try_into().unwrap(),
            first,
            last,
            name: String::from_utf16_lossy(&units),
        });
    }
    let mut sorted: Vec<&Partition> = partitions.iter().collect();
    sorted.sort_by_key(|p| p.first);
    if sorted.windows(2).any(|w| w[1].first <= w[0].last) {
        return Err(Error("GPT partitions overlap"));
    }
    Ok(Table {
        header: header.to_vec(),
        entries: entries.to_vec(),
        my_lba: le64(header, 24),
        other_lba: le64(header, 32),
        first_usable,
        last_usable,
        entries_lba,
        count,
        partitions,
    })
}

impl Table {
    /// The unused ranges between the partitions, as (first LBA, blocks).
    pub fn free(&self) -> Vec<(u64, u64)> {
        let mut sorted: Vec<&Partition> = self.partitions.iter().collect();
        sorted.sort_by_key(|p| p.first);
        let (mut out, mut cursor) = (Vec::new(), self.first_usable);
        for p in sorted {
            if p.first > cursor {
                out.push((cursor, p.first - cursor));
            }
            cursor = cursor.max(p.last + 1);
        }
        if cursor <= self.last_usable {
            out.push((cursor, self.last_usable - cursor + 1));
        }
        out
    }

    /// The bytes checksummed as the table's identity: the header as far as
    /// it says it reaches, then the entry array.
    pub fn identity(&self) -> Vec<u8> {
        let size = le32(&self.header, 12) as usize;
        [&self.header[..size], &self.entries[..]].concat()
    }

    /// Whether `backup` is the other copy of this table.
    pub fn agrees_with(&self, backup: &Table) -> bool {
        self.entries == backup.entries
            && self.header[56..72] == backup.header[56..72]
            && self.my_lba == backup.other_lba
            && self.other_lba == backup.my_lba
            && (self.first_usable, self.last_usable) == (backup.first_usable, backup.last_usable)
    }
}

/// A partition to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct New {
    pub kind: [u8; 16],
    pub guid: [u8; 16],
    pub first: u64,
    pub blocks: u64,
    pub name: String,
}

/// Both copies of a changed table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub primary_header: Vec<u8>,
    pub backup_header: Vec<u8>,
    pub entries: Vec<u8>,
    /// The entry numbers the added partitions got, in the order given.
    pub numbers: Vec<u32>,
}

/// The table with the partitions numbered `remove` gone and `add` in the
/// lowest free entries. Added partitions must lie in space that is free once
/// the removed ones are gone; everything else stays byte for byte.
pub fn change(primary: &Table, backup: &Table, remove: &[u32], add: &[New]) -> Result<Changed> {
    if !primary.agrees_with(backup) {
        return Err(Error("the two GPT copies disagree"));
    }
    let mut entries = primary.entries.clone();
    for number in remove {
        if !primary.partitions.iter().any(|p| p.number == *number) {
            return Err(Error("no such GPT partition to remove"));
        }
        let at = (*number as usize - 1) * ENTRY;
        entries[at..at + ENTRY].fill(0);
    }
    let after = parse_entries(&primary.header, &entries)?;
    let mut free = after.free();
    let mut numbers = Vec::new();
    for new in add {
        let end = new.first.checked_add(new.blocks).ok_or(Error("GPT partition out of range"))?;
        let fits = new.blocks > 0 && free.iter().any(|&(start, blocks)| new.first >= start && end <= start + blocks);
        if !fits {
            return Err(Error("a new GPT partition does not lie in free space"));
        }
        let slot = (0..primary.count as usize)
            .find(|i| entries[i * ENTRY..i * ENTRY + 16].iter().all(|&b| b == 0))
            .ok_or(Error("the GPT has no free entry"))?;
        let raw = &mut entries[slot * ENTRY..(slot + 1) * ENTRY];
        raw[..16].copy_from_slice(&new.kind);
        raw[16..32].copy_from_slice(&new.guid);
        raw[32..40].copy_from_slice(&new.first.to_le_bytes());
        raw[40..48].copy_from_slice(&(end - 1).to_le_bytes());
        let units: Vec<u16> = new.name.encode_utf16().collect();
        if units.len() > 36 {
            return Err(Error("GPT partition name too long"));
        }
        for (i, unit) in units.iter().enumerate() {
            raw[56 + i * 2..58 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
        numbers.push(slot as u32 + 1);
        // The range is taken for the next one.
        free = parse_entries(&primary.header, &entries)?.free();
    }
    let crc = crc32fast::hash(&entries);
    let seal = |header: &[u8]| {
        let size = le32(header, 12) as usize;
        let mut out = header.to_vec();
        out[88..92].copy_from_slice(&crc.to_le_bytes());
        out[16..20].fill(0);
        let sum = crc32fast::hash(&out[..size]);
        out[16..20].copy_from_slice(&sum.to_le_bytes());
        out
    };
    Ok(Changed { primary_header: seal(&primary.header), backup_header: seal(&backup.header), entries, numbers })
}

/// A header with other entries: only what `free` and the overlap check need.
fn parse_entries(header: &[u8], entries: &[u8]) -> Result<Table> {
    let mut sealed = header.to_vec();
    sealed[88..92].copy_from_slice(&crc32fast::hash(entries).to_le_bytes());
    let size = le32(header, 12) as usize;
    sealed[16..20].fill(0);
    let sum = crc32fast::hash(&sealed[..size]);
    sealed[16..20].copy_from_slice(&sum.to_le_bytes());
    parse(&sealed, entries)
}

/// A GUID as text (`7c3457ef-0000-11aa-aa11-00306543ecac`) from its bytes on
/// disk, whose first three fields are little-endian.
pub fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g[3], g[2], g[1], g[0], g[5], g[4], g[7], g[6], g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    )
}

/// The bytes on disk of a GUID given as text.
pub fn guid_bytes(text: &str) -> Result<[u8; 16]> {
    let hex: Vec<u8> = text.bytes().filter(|b| *b != b'-').collect();
    if hex.len() != 32 || text.len() != 36 {
        return Err(Error("not a GUID"));
    }
    let mut raw = [0u8; 16];
    for (i, pair) in hex.as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| (b as char).to_digit(16).ok_or(Error("not a GUID"));
        raw[i] = (digit(pair[0])? * 16 + digit(pair[1])?) as u8;
    }
    Ok([raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]])
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const APFS: &str = "7c3457ef-0000-11aa-aa11-00306543ecac";
    pub const LINUX: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

    /// Both copies of a table for a disk of `total` sectors of `sector`
    /// bytes, as an iPhone has it: 128 entries, usable from LBA 6 (4 KiB
    /// sectors).
    pub fn disk(sector: u64, total: u64, parts: &[(&str, u64, u64)]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let count = 128u32;
        let array = (count as u64 * 128).div_ceil(sector);
        let mut entries = vec![0u8; count as usize * 128];
        for (i, (kind, first, blocks)) in parts.iter().enumerate() {
            let raw = &mut entries[i * 128..(i + 1) * 128];
            raw[..16].copy_from_slice(&guid_bytes(kind).unwrap());
            raw[16..32].copy_from_slice(&[i as u8 + 1; 16]);
            raw[32..40].copy_from_slice(&first.to_le_bytes());
            raw[40..48].copy_from_slice(&(first + blocks - 1).to_le_bytes());
        }
        let header = |my: u64, other: u64, entries_lba: u64| {
            let mut h = vec![0u8; sector as usize];
            h[..8].copy_from_slice(SIGNATURE);
            h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&my.to_le_bytes());
            h[32..40].copy_from_slice(&other.to_le_bytes());
            h[40..48].copy_from_slice(&(2 + array).to_le_bytes());
            h[48..56].copy_from_slice(&(total - 2 - array).to_le_bytes());
            h[56..72].copy_from_slice(&[0xd1; 16]);
            h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
            h[80..84].copy_from_slice(&count.to_le_bytes());
            h[84..88].copy_from_slice(&128u32.to_le_bytes());
            h[88..92].copy_from_slice(&crc32fast::hash(&entries).to_le_bytes());
            let sum = crc32fast::hash(&h[..92]);
            h[16..20].copy_from_slice(&sum.to_le_bytes());
            h
        };
        (header(1, total - 1, 2), header(total - 1, 1, total - 1 - array), entries)
    }

    #[test]
    fn a_table_is_read_with_its_free_space() {
        let (primary, _, entries) = disk(4096, 1000, &[(APFS, 6, 500), (LINUX, 600, 100)]);
        let table = parse(&primary, &entries).unwrap();
        assert_eq!(table.partitions.len(), 2);
        assert_eq!((table.partitions[1].number, table.partitions[1].blocks()), (2, 100));
        assert_eq!(guid_text(&table.partitions[0].kind), APFS);
        assert_eq!(table.free(), [(506, 94), (700, 295)]);
    }

    #[test]
    fn a_damaged_copy_is_refused() {
        let (mut primary, _, mut entries) = disk(4096, 1000, &[(APFS, 6, 500)]);
        entries[40] ^= 1;
        assert_eq!(parse(&primary, &entries).unwrap_err().0, "GPT entry array checksum mismatch");
        primary[40] ^= 1;
        assert_eq!(entries_of(&primary).unwrap_err().0, "GPT header checksum mismatch");
    }

    #[test]
    fn a_change_keeps_everything_else_and_both_copies_valid() {
        let (primary, backup, entries) = disk(4096, 1000, &[(APFS, 6, 500), (LINUX, 600, 100)]);
        let (p, b) = (parse(&primary, &entries).unwrap(), parse(&backup, &entries).unwrap());
        let new = New { kind: guid_bytes(LINUX).unwrap(), guid: [9; 16], first: 512, blocks: 400, name: "omarchy-a".into() };
        assert!(change(&p, &b, &[], std::slice::from_ref(&new)).is_err(), "overlaps partition 2 while it is there");
        let changed = change(&p, &b, &[2], std::slice::from_ref(&new)).unwrap();
        assert_eq!(changed.numbers, [2]);
        assert_eq!(changed.entries[..128], entries[..128], "APFS untouched");
        let (np, nb) = (parse(&changed.primary_header, &changed.entries).unwrap(), parse(&changed.backup_header, &changed.entries).unwrap());
        assert!(np.agrees_with(&nb));
        assert_eq!((np.partitions[1].first, np.partitions[1].last, np.partitions[1].name.as_str()), (512, 911, "omarchy-a"));
        assert_eq!((nb.my_lba, nb.entries_lba), (999, b.entries_lba));
    }

    #[test]
    fn guids_round_trip() {
        assert_eq!(guid_text(&guid_bytes(APFS).unwrap()), APFS);
        assert!(guid_bytes("7c3457ef").is_err());
    }
}

//! A partition table for the tests' disks, as an iPhone has it: 128
//! entries, usable from LBA 6 with 4 KiB sectors.
use orchard_formats::gpt::guid_bytes;

/// (primary header, backup header, entry array) for a disk of `total`
/// sectors with `parts` as (type, first LBA, blocks).
pub fn tables(sector: u64, total: u64, parts: &[(&str, u64, u64)]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
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
        h[..8].copy_from_slice(b"EFI PART");
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
        h[88..92].copy_from_slice(&crc32(&entries).to_le_bytes());
        let sum = crc32(&h[..92]);
        h[16..20].copy_from_slice(&sum.to_le_bytes());
        h
    };
    (header(1, total - 1, 2), header(total - 1, 1, total - 1 - array), entries)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

//! A new FAT32 filesystem with a given set of files, as the regions to write
//! to its partition. The empty filesystem is the one orchard's install system
//! makes (`install_device.fat32_regions`: one sector per cluster, 8 reserved
//! sectors, two FATs); files and directories are laid out behind each other
//! from the first cluster, with long names.
use crate::{Error, Result};
use std::collections::BTreeMap;

const RESERVED: u64 = 8;
const FATS: u64 = 2;
const END: u32 = 0x0fff_ffff;
const DIRECTORY: u8 = 0x10;
const ARCHIVE: u8 = 0x20;

/// A file to put on the filesystem. Directories come from the paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File<'a> {
    /// `/`-separated, without a leading slash.
    pub path: &'a str,
    pub data: &'a [u8],
}

/// When the files say they were written, as FAT stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub date: u16,
    pub time: u16,
}

impl Stamp {
    pub fn new(year: u16, month: u16, day: u16, hour: u16, minute: u16) -> Stamp {
        Stamp { date: (year.saturating_sub(1980) << 9) | (month << 5) | day, time: (hour << 11) | (minute << 5) }
    }
}

/// Bytes to write at an offset of the partition.
pub type Region = (u64, Vec<u8>);

#[derive(Default)]
struct Dir<'a> {
    dirs: BTreeMap<&'a str, Dir<'a>>,
    files: BTreeMap<&'a str, &'a [u8]>,
    cluster: u32,
}

/// The regions of a FAT32 filesystem of `blocks` sectors of `sector` bytes
/// holding `files`.
pub fn build(blocks: u64, sector: u32, volume_id: u32, label: &[u8; 11], files: &[File], stamp: Stamp) -> Result<Vec<Region>> {
    let size = sector as u64;
    if !(512..=4096).contains(&sector) || !sector.is_power_of_two() || blocks > u32::MAX as u64 {
        return Err(Error("unusual FAT32 geometry"));
    }
    let mut fat_sectors = 1u64;
    let clusters = loop {
        let clusters = blocks.checked_sub(RESERVED + FATS * fat_sectors).ok_or(Error("partition too small for FAT32"))?;
        let needed = ((clusters + 2) * 4).div_ceil(size);
        if needed <= fat_sectors {
            break clusters;
        }
        fat_sectors = needed;
    };
    if clusters < 65525 {
        return Err(Error("partition too small for FAT32"));
    }

    let mut root = Dir::default();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        if parts.iter().any(|p| !valid_name(p)) {
            return Err(Error("a file name FAT cannot hold"));
        }
        let mut dir = &mut root;
        for part in &parts[..parts.len() - 1] {
            if dir.files.contains_key(part) {
                return Err(Error("a FAT path is both a file and a directory"));
            }
            dir = dir.dirs.entry(part).or_default();
        }
        let leaf = parts[parts.len() - 1];
        if dir.dirs.contains_key(leaf) || dir.files.insert(leaf, file.data).is_some() {
            return Err(Error("a FAT path is given twice"));
        }
    }

    // Clusters in order: every directory, depth first, then every file.
    let mut fat = vec![0u8; (fat_sectors * size) as usize];
    let mut data: Vec<u8> = Vec::new();
    let mut next = 2u32;
    let mut take = |bytes: u64, fat: &mut Vec<u8>| -> Result<u32> {
        let count = bytes.div_ceil(size).max(1) as u32;
        if (next + count) as u64 > clusters + 2 {
            return Err(Error("the files do not fit on the FAT32 filesystem"));
        }
        let first = next;
        for c in first..first + count {
            let value = if c + 1 == first + count { END } else { c + 1 };
            fat[c as usize * 4..c as usize * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        next += count;
        Ok(first)
    };
    fat[..4].copy_from_slice(&0x0fff_fff8u32.to_le_bytes());
    fat[4..8].copy_from_slice(&END.to_le_bytes());
    assign(&mut root, true, size, &mut |bytes| take(bytes, &mut fat))?;
    let mut file_clusters = BTreeMap::new();
    place_files(&root, String::new(), &mut |bytes| take(bytes, &mut fat), &mut file_clusters)?;
    let used = next - 2;
    data.resize(used as usize * size as usize, 0);
    write_dir(&root, None, String::new(), size, stamp, &file_clusters, &mut data);
    write_files(&root, String::new(), size, &file_clusters, &mut data);

    let mut boot = vec![0u8; size as usize];
    boot[..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"ORCHARD ");
    boot[11..13].copy_from_slice(&(sector as u16).to_le_bytes());
    boot[13] = 1;
    boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    boot[16] = FATS as u8;
    boot[21] = 0xf8;
    boot[24..26].copy_from_slice(&32u16.to_le_bytes());
    boot[26..28].copy_from_slice(&64u16.to_le_bytes());
    boot[32..36].copy_from_slice(&(blocks as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&(fat_sectors as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48..50].copy_from_slice(&1u16.to_le_bytes());
    boot[50..52].copy_from_slice(&6u16.to_le_bytes());
    boot[64] = 0x80;
    boot[66] = 0x29;
    boot[67..71].copy_from_slice(&volume_id.to_le_bytes());
    boot[71..82].copy_from_slice(label);
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510..512].copy_from_slice(&[0x55, 0xaa]);
    let mut info = vec![0u8; size as usize];
    info[..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    info[488..492].copy_from_slice(&((clusters - used as u64) as u32).to_le_bytes());
    info[492..496].copy_from_slice(&next.to_le_bytes());
    info[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes());
    let mut regions = vec![(0, boot.clone()), (size, info.clone()), (6 * size, boot), (7 * size, info)];
    for index in 0..FATS {
        regions.push(((RESERVED + index * fat_sectors) * size, fat.clone()));
    }
    regions.push(((RESERVED + FATS * fat_sectors) * size, data));
    Ok(regions)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && name.chars().all(|c| c >= ' ' && !"\"*/:<>?\\|".contains(c) && (c as u32) < 0x1_0000)
}

/// How many 32-byte entries a name takes: its long-name entries and the short one.
fn slots(name: &str) -> u64 {
    (name.encode_utf16().count() as u64).div_ceil(13) + 1
}

fn dir_bytes(dir: &Dir, is_root: bool) -> u64 {
    let names = dir.dirs.keys().chain(dir.files.keys());
    (names.map(|n| slots(n)).sum::<u64>() + if is_root { 0 } else { 2 }) * 32
}

fn assign(dir: &mut Dir, is_root: bool, size: u64, take: &mut dyn FnMut(u64) -> Result<u32>) -> Result<()> {
    dir.cluster = take(dir_bytes(dir, is_root).max(size))?;
    for child in dir.dirs.values_mut() {
        assign(child, false, size, take)?;
    }
    Ok(())
}

fn place_files(dir: &Dir, prefix: String, take: &mut dyn FnMut(u64) -> Result<u32>, out: &mut BTreeMap<String, u32>) -> Result<()> {
    for (name, data) in &dir.files {
        let cluster = if data.is_empty() { 0 } else { take(data.len() as u64)? };
        out.insert(format!("{prefix}{name}"), cluster);
    }
    for (name, child) in &dir.dirs {
        place_files(child, format!("{prefix}{name}/"), take, out)?;
    }
    Ok(())
}

fn offset(cluster: u32, size: u64) -> usize {
    (cluster as usize - 2) * size as usize
}

fn write_files(dir: &Dir, prefix: String, size: u64, clusters: &BTreeMap<String, u32>, data: &mut [u8]) {
    for (name, bytes) in &dir.files {
        if !bytes.is_empty() {
            let at = offset(clusters[&format!("{prefix}{name}")], size);
            data[at..at + bytes.len()].copy_from_slice(bytes);
        }
    }
    for (name, child) in &dir.dirs {
        write_files(child, format!("{prefix}{name}/"), size, clusters, data);
    }
}

/// The short name's checksum, which ties the long-name entries to it.
fn checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &c| ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c))
}

/// An 8.3 alias for `name` that no earlier entry of the directory has.
fn alias(name: &str, taken: &mut Vec<[u8; 11]>) -> [u8; 11] {
    let clean = |text: &str| -> Vec<u8> {
        text.chars()
            .filter(|c| *c != ' ' && *c != '.')
            .map(|c| if c.is_ascii_alphanumeric() || "$%'-_@~`!(){}^#&".contains(c) { c.to_ascii_uppercase() as u8 } else { b'_' })
            .collect()
    };
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot + 1..]),
        _ => (name, ""),
    };
    let (stem, extension) = (clean(stem), clean(extension));
    for n in 1u32.. {
        let tail = format!("~{n}");
        let keep = stem.len().min(8 - tail.len());
        let mut short = [b' '; 11];
        short[..keep].copy_from_slice(&stem[..keep]);
        short[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        let ext = extension.len().min(3);
        short[8..8 + ext].copy_from_slice(&extension[..ext]);
        if !taken.contains(&short) {
            taken.push(short);
            return short;
        }
    }
    unreachable!()
}

fn entry(short: &[u8; 11], attributes: u8, cluster: u32, length: u32, stamp: Stamp) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(short);
    e[11] = attributes;
    e[14..16].copy_from_slice(&stamp.time.to_le_bytes());
    e[16..18].copy_from_slice(&stamp.date.to_le_bytes());
    e[18..20].copy_from_slice(&stamp.date.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&stamp.time.to_le_bytes());
    e[24..26].copy_from_slice(&stamp.date.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&length.to_le_bytes());
    e
}

/// A name's entries: the long name, last part first, then the short entry.
fn named(name: &str, short: &[u8; 11], attributes: u8, cluster: u32, length: u32, stamp: Stamp) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let parts = units.len().div_ceil(13);
    let sum = checksum(short);
    let mut out = Vec::new();
    for part in (0..parts).rev() {
        let mut e = [0u8; 32];
        e[0] = (part as u8 + 1) | if part + 1 == parts { 0x40 } else { 0 };
        e[11] = 0x0f;
        e[13] = sum;
        for (i, at) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].into_iter().enumerate() {
            let index = part * 13 + i;
            let unit = match index.cmp(&units.len()) {
                std::cmp::Ordering::Less => units[index],
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 0xffff,
            };
            e[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&e);
    }
    out.extend_from_slice(&entry(short, attributes, cluster, length, stamp));
    out
}

fn write_dir(dir: &Dir, parent: Option<u32>, prefix: String, size: u64, stamp: Stamp, clusters: &BTreeMap<String, u32>, data: &mut [u8]) {
    let mut bytes = Vec::new();
    if let Some(parent) = parent {
        bytes.extend_from_slice(&entry(b".          ", DIRECTORY, dir.cluster, 0, stamp));
        bytes.extend_from_slice(&entry(b"..         ", DIRECTORY, parent, 0, stamp));
    }
    let mut taken = Vec::new();
    for (name, child) in &dir.dirs {
        let short = alias(name, &mut taken);
        bytes.extend_from_slice(&named(name, &short, DIRECTORY, child.cluster, 0, stamp));
    }
    for (name, content) in &dir.files {
        let short = alias(name, &mut taken);
        bytes.extend_from_slice(&named(name, &short, ARCHIVE, clusters[&format!("{prefix}{name}")], content.len() as u32, stamp));
    }
    let at = offset(dir.cluster, size);
    data[at..at + bytes.len()].copy_from_slice(&bytes);
    // The root's `..` is cluster 0 by definition.
    let me = if parent.is_some() { dir.cluster } else { 0 };
    for (name, child) in &dir.dirs {
        write_dir(child, Some(me), format!("{prefix}{name}/"), size, stamp, clusters, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(files: &[File]) -> Vec<u8> {
        let (blocks, sector) = (70_000u64, 4096u32);
        let mut image = vec![0u8; 4096 * 600];
        for (at, bytes) in build(blocks, sector, 0x1a2b_3c4d, b"BOOT       ", files, Stamp::new(2026, 11, 26, 6, 30)).unwrap() {
            let at = at as usize;
            if at + bytes.len() > image.len() {
                image.resize(at + bytes.len(), 0);
            }
            image[at..at + bytes.len()].copy_from_slice(&bytes);
        }
        image
    }

    #[test]
    fn the_empty_filesystem_is_the_install_systems() {
        let image = image(&[]);
        assert_eq!(&image[3..11], b"ORCHARD ");
        assert_eq!(&image[82..90], b"FAT32   ");
        assert_eq!(u16::from_le_bytes([image[11], image[12]]), 4096);
        assert_eq!(image[510..512], [0x55, 0xaa]);
        assert_eq!(image[..4096], image[6 * 4096..7 * 4096], "backup boot sector");
        let fat = 8 * 4096;
        assert_eq!(image[fat..fat + 12], [0xf8, 0xff, 0xff, 0x0f, 0xff, 0xff, 0xff, 0x0f, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn files_chain_their_clusters_and_carry_long_names() {
        let big = vec![7u8; 9000];
        let image = image(&[
            File { path: "loader/entries/omarchy-a-1a2b3c4d-7.3.0.conf", data: b"title Omarchy A\n" },
            File { path: "omarchy-a-1a2b3c4d/7.3.0/linux", data: &big },
        ]);
        let fat_sectors = u32::from_le_bytes(image[36..40].try_into().unwrap()) as usize;
        let data = (8 + 2 * fat_sectors) * 4096;
        let text = String::from_utf8_lossy(&image[data..]);
        assert!(text.contains("LOADER~1") && text.contains("OMARCH~1"), "short aliases");
        let utf16: Vec<u8> = "omarchy-a-1a2".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert!(image[data..].windows(10).any(|w| w == &utf16[..10]), "long name entries");
        // The 9000-byte file takes three clusters, chained in the FAT.
        let at = image.windows(4096).position(|w| w.iter().all(|&b| b == 7)).unwrap();
        let cluster = (at - data) / 4096 + 2;
        let entry = |c: usize| u32::from_le_bytes(image[8 * 4096 + c * 4..8 * 4096 + c * 4 + 4].try_into().unwrap());
        assert_eq!((entry(cluster), entry(cluster + 1), entry(cluster + 2)), (cluster as u32 + 1, cluster as u32 + 2, END));
    }

    #[test]
    fn bad_paths_and_small_partitions_are_refused() {
        let stamp = Stamp::new(2026, 1, 1, 0, 0);
        assert!(build(1000, 4096, 1, b"BOOT       ", &[], stamp).is_err());
        let twice = [File { path: "a", data: b"" }, File { path: "a/b", data: b"" }];
        assert!(build(70_000, 4096, 1, b"BOOT       ", &twice, stamp).is_err());
        assert!(build(70_000, 4096, 1, b"BOOT       ", &[File { path: "a:b", data: b"" }], stamp).is_err());
    }
}

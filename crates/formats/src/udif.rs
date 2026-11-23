//! UDIF disk images (`.dmg`): the `koly` trailer and the block map in its
//! XML metadata. Only single-segment images with the data fork at the start
//! of the file, which is how Apple ships firmware images.
use crate::{lzfse, zlib, Error, Result};
use plist::Value;

pub const SECTOR: u64 = 512;
/// Size of the `koly` trailer at the end of the file.
pub const TRAILER: usize = 512;

/// What the trailer says about the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    /// End of the data fork, which starts at byte 0.
    pub data_end: u64,
    pub metadata_offset: u64,
    pub metadata_len: u64,
    /// Size of the disk the image describes.
    pub disk_size: u64,
}

impl Trailer {
    pub fn parse(bytes: &[u8]) -> Result<Trailer> {
        if bytes.len() != TRAILER
            || &bytes[..4] != b"koly"
            || be32(bytes, 4) != 4
            || be32(bytes, 8) != 512
        {
            return Err(Error("not a UDIF trailer"));
        }
        // Segment number and count must be 1/1, and the data fork must start
        // at byte 0 with no resource fork.
        if be32(bytes, 56) != 1 || be32(bytes, 60) != 1 || be64(bytes, 16) != 0 {
            return Err(Error("segmented UDIF images are not supported"));
        }
        if be64(bytes, 24) != 0 || be64(bytes, 48) != 0 {
            return Err(Error("UDIF data fork must start at byte 0"));
        }
        let t = Trailer {
            data_end: be64(bytes, 32),
            metadata_offset: be64(bytes, 0xd8),
            metadata_len: be64(bytes, 0xe0),
            disk_size: be64(bytes, 492)
                .checked_mul(SECTOR)
                .ok_or(Error("UDIF disk size overflow"))?,
        };
        if t.data_end == 0
            || t.data_end > t.metadata_offset
            || t.metadata_len == 0
            || t.disk_size == 0
        {
            return Err(Error("UDIF trailer layout is inconsistent"));
        }
        Ok(t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zero,
    Raw,
    Zlib,
    Lzfse,
}

/// A run of disk bytes and where its data is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub kind: Kind,
    /// Byte range on the disk.
    pub start: u64,
    pub end: u64,
    /// Byte range in the file. Empty for zero chunks.
    pub offset: u64,
    pub len: u64,
}

impl Chunk {
    pub fn disk_len(&self) -> u64 {
        self.end - self.start
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub first_sector: u64,
    pub sectors: u64,
}

/// The whole block map: partitions and their chunks, sorted by disk offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    pub partitions: Vec<Partition>,
    pub chunks: Vec<Chunk>,
}

/// Reads the block map from the XML metadata and checks it against the trailer.
pub fn map(xml: &[u8], trailer: &Trailer) -> Result<Map> {
    let value =
        Value::from_reader_xml(xml).map_err(|_| Error("UDIF metadata is not an XML plist"))?;
    let entries = value
        .as_dictionary()
        .and_then(|d| {
            d.get("resource-fork")?
                .as_dictionary()?
                .get("blkx")?
                .as_array()
        })
        .ok_or(Error("UDIF metadata has no block map"))?;
    let mut map = Map {
        partitions: Vec::new(),
        chunks: Vec::new(),
    };
    for entry in entries {
        let entry = entry
            .as_dictionary()
            .ok_or(Error("bad UDIF block map entry"))?;
        let name = entry
            .get("Name")
            .and_then(Value::as_string)
            .unwrap_or_default();
        let table = entry
            .get("Data")
            .and_then(Value::as_data)
            .ok_or(Error("UDIF block map entry has no table"))?;
        map.partitions
            .push(partition(name, table, trailer, &mut map.chunks)?);
    }
    map.chunks.sort_by_key(|c| c.start);
    if map.chunks.is_empty() || map.chunks.windows(2).any(|w| w[0].end > w[1].start) {
        return Err(Error("UDIF chunks are missing or overlap"));
    }
    Ok(map)
}

/// Reads one `mish` table, adding its chunks to `chunks`.
fn partition(
    name: &str,
    mish: &[u8],
    trailer: &Trailer,
    chunks: &mut Vec<Chunk>,
) -> Result<Partition> {
    const HEADER: usize = 204;
    const RUN: usize = 40;
    if mish.len() < HEADER || &mish[..4] != b"mish" || be32(mish, 4) != 1 {
        return Err(Error("bad UDIF block table"));
    }
    let (first, sectors, base) = (be64(mish, 8), be64(mish, 16), be64(mish, 24));
    let runs = be32(mish, 200) as usize;
    if runs == 0 || mish.len() != HEADER + RUN * runs {
        return Err(Error(
            "UDIF block table length does not match its run count",
        ));
    }
    let bytes = |s: u64| s.checked_mul(SECTOR).ok_or(Error("UDIF sector overflow"));
    let part_start = bytes(first)?;
    let part_end = bytes(
        first
            .checked_add(sectors)
            .ok_or(Error("UDIF sector overflow"))?,
    )?;
    if sectors == 0 || part_end > trailer.disk_size {
        return Err(Error("UDIF partition lies outside the disk"));
    }
    for i in 0..runs {
        let at = HEADER + RUN * i;
        let kind = match be32(mish, at) {
            0 | 2 => Kind::Zero,
            1 => Kind::Raw,
            0x8000_0005 => Kind::Zlib,
            0x8000_0007 => Kind::Lzfse,
            // A comment, not data.
            0x7fff_fffe => continue,
            // The terminator must be the last run.
            0xffff_ffff if i + 1 == runs => continue,
            0xffff_ffff => return Err(Error("UDIF runs after the terminator")),
            _ => return Err(Error("unsupported UDIF chunk type")),
        };
        let start = part_start
            .checked_add(bytes(be64(mish, at + 8))?)
            .ok_or(Error("UDIF chunk overflow"))?;
        let end = start
            .checked_add(bytes(be64(mish, at + 16))?)
            .ok_or(Error("UDIF chunk overflow"))?;
        let offset = base
            .checked_add(be64(mish, at + 24))
            .ok_or(Error("UDIF chunk overflow"))?;
        let len = be64(mish, at + 32);
        let consistent = match kind {
            Kind::Zero => len == 0,
            Kind::Raw => len == end - start,
            Kind::Zlib | Kind::Lzfse => len > 0,
        };
        if start == end || start < part_start || end > part_end || !consistent {
            return Err(Error(
                "UDIF chunk is empty, outside its partition or has the wrong size",
            ));
        }
        if kind != Kind::Zero && offset.checked_add(len).is_none_or(|e| e > trailer.data_end) {
            return Err(Error("UDIF chunk data lies outside the data fork"));
        }
        chunks.push(Chunk {
            kind,
            start,
            end,
            offset,
            len,
        });
    }
    Ok(Partition {
        name: name.to_owned(),
        first_sector: first,
        sectors,
    })
}

/// Decodes a compressed chunk's data.
pub fn decode(chunk: &Chunk, data: &[u8]) -> Result<Vec<u8>> {
    let size = usize::try_from(chunk.disk_len()).map_err(|_| Error("UDIF chunk too large"))?;
    match chunk.kind {
        Kind::Zlib => zlib::decode(data, size),
        Kind::Lzfse => lzfse::decode(data, size),
        Kind::Zero | Kind::Raw => Err(Error("UDIF chunk is not compressed")),
    }
}

/// The whole disk of a small image held in memory, with every checksum
/// checked: each partition's CRC32 over its data, the data fork's and the
/// master checksum.
pub fn read(image: &[u8]) -> Result<Vec<u8>> {
    let at = image.len().checked_sub(TRAILER).ok_or(Error("too small for a UDIF image"))?;
    let koly = &image[at..];
    let trailer = Trailer::parse(koly)?;
    let end = |offset: u64, len: u64| offset.checked_add(len).filter(|&e| e <= at as u64).map(|e| e as usize);
    let xml_end = end(trailer.metadata_offset, trailer.metadata_len).ok_or(Error("UDIF metadata outside the file"))?;
    let xml = &image[trailer.metadata_offset as usize..xml_end];
    let data_end = end(0, trailer.data_end).ok_or(Error("UDIF data fork outside the file"))?;
    if be32(koly, 80) == 2 && be32(koly, 88) != crc32fast::hash(&image[..data_end]) {
        return Err(Error("UDIF data fork checksum mismatch"));
    }
    let map = map(xml, &trailer)?;
    let size = usize::try_from(trailer.disk_size).map_err(|_| Error("UDIF disk too large"))?;
    let mut disk = vec![0u8; size];
    for chunk in &map.chunks {
        let (start, stop) = (chunk.start as usize, chunk.end as usize);
        let data = || &image[chunk.offset as usize..(chunk.offset + chunk.len) as usize];
        match chunk.kind {
            Kind::Zero => {}
            Kind::Raw => disk[start..stop].copy_from_slice(data()),
            Kind::Zlib | Kind::Lzfse => disk[start..stop].copy_from_slice(&decode(chunk, data())?),
        }
    }
    // Partition checksums: CRC32 over the data of every non-zero run.
    let value = Value::from_reader_xml(xml).map_err(|_| Error("UDIF metadata is not an XML plist"))?;
    let tables = value
        .as_dictionary()
        .and_then(|d| d.get("resource-fork")?.as_dictionary()?.get("blkx")?.as_array())
        .ok_or(Error("UDIF metadata has no block map"))?;
    let mut sums = Vec::new();
    for table in tables {
        let mish = table.as_dictionary().and_then(|d| d.get("Data")?.as_data()).ok_or(Error("bad UDIF block map entry"))?;
        let first = be64(mish, 8) * SECTOR;
        let mut crc = crc32fast::Hasher::new();
        for i in 0..be32(mish, 200) as usize {
            let run = 204 + 40 * i;
            if matches!(be32(mish, run), 0 | 2 | 0x7fff_fffe | 0xffff_ffff) {
                continue;
            }
            let start = (first + be64(mish, run + 8) * SECTOR) as usize;
            crc.update(&disk[start..start + (be64(mish, run + 16) * SECTOR) as usize]);
        }
        if be32(mish, 64) == 2 && be32(mish, 72) != crc.finalize() {
            return Err(Error("UDIF partition checksum mismatch"));
        }
        sums.extend_from_slice(&mish[72..76]);
    }
    if be32(koly, 352) == 2 && be32(koly, 360) != crc32fast::hash(&sums) {
        return Err(Error("UDIF master checksum mismatch"));
    }
    Ok(disk)
}

/// The UDIF images embedded in a larger file (such as palera1n's
/// executable), found by their trailers. Duplicates (the same image in each
/// slice of a universal binary) are returned once.
pub fn embedded(file: &[u8]) -> Vec<&[u8]> {
    let mut found: Vec<&[u8]> = Vec::new();
    let mut from = 0;
    while let Some(i) = file[from..].windows(4).position(|w| w == b"koly") {
        let at = from + i;
        from = at + 4;
        let Some(koly) = file.get(at..at + TRAILER) else { break };
        if be32(koly, 4) != 4 || be32(koly, 8) != 512 {
            continue;
        }
        let (xml_offset, xml_len) = (be64(koly, 216), be64(koly, 224));
        let Some(start) = (at as u64).checked_sub(xml_offset).and_then(|s| s.checked_sub(xml_len)) else { continue };
        if xml_len == 0 || start >= at as u64 {
            continue;
        }
        let image = &file[start as usize..at + TRAILER];
        if Trailer::parse(&image[image.len() - TRAILER..]).is_ok() && !found.contains(&image) {
            found.push(image);
        }
    }
    found
}

/// What [`write`] puts in the image's metadata.
pub struct WriteOptions<'a> {
    /// The partition's name, e.g. `whole disk (Apple_HFSX : 0)`.
    pub name: &'a str,
    /// The image's segment identifier, unique per image.
    pub segment_id: [u8; 16],
}

const CHUNK_SECTORS: u64 = 2048;

/// A zlib-compressed (UDZO) image of `disk`, one partition, in the layout
/// `hdiutil` gives a plain filesystem image: 1 MiB chunks, all-zero runs
/// stored as zero chunks, CRC32 checksums.
pub fn write(disk: &[u8], options: &WriteOptions) -> Result<Vec<u8>> {
    use std::io::Write;
    if disk.is_empty() || !disk.len().is_multiple_of(SECTOR as usize) {
        return Err(Error("a UDIF disk is a whole number of sectors"));
    }
    let sectors = disk.len() as u64 / SECTOR;
    let mut data = Vec::new();
    let mut runs: Vec<(u32, u64, u64, u64, u64)> = Vec::new();
    let mut crc = crc32fast::Hasher::new();
    let mut sector = 0;
    while sector < sectors {
        let count = CHUNK_SECTORS.min(sectors - sector);
        let bytes = &disk[(sector * SECTOR) as usize..((sector + count) * SECTOR) as usize];
        if bytes.iter().all(|&b| b == 0) {
            runs.push((2, sector, count, data.len() as u64, 0));
        } else {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            z.write_all(bytes).map_err(|_| Error("zlib failed"))?;
            let packed = z.finish().map_err(|_| Error("zlib failed"))?;
            runs.push((0x8000_0005, sector, count, data.len() as u64, packed.len() as u64));
            data.extend(packed);
            crc.update(bytes);
        }
        sector += count;
    }
    runs.push((0xffff_ffff, sectors, 0, data.len() as u64, 0));
    let part_crc = crc.finalize();
    let mut mish = vec![0u8; 204 + 40 * runs.len()];
    mish[..4].copy_from_slice(b"mish");
    put32(&mut mish, 4, 1);
    put64(&mut mish, 16, sectors);
    put32(&mut mish, 32, (CHUNK_SECTORS + 8) as u32);
    put32(&mut mish, 36, 0xffff_fffe);
    put32(&mut mish, 64, 2);
    put32(&mut mish, 68, 32);
    put32(&mut mish, 72, part_crc);
    put32(&mut mish, 200, runs.len() as u32);
    for (i, (kind, start, count, offset, len)) in runs.iter().enumerate() {
        let at = 204 + 40 * i;
        put32(&mut mish, at, *kind);
        put64(&mut mish, at + 8, *start);
        put64(&mut mish, at + 16, *count);
        put64(&mut mish, at + 24, *offset);
        put64(&mut mish, at + 32, *len);
    }
    // hdiutil's `plst` resource for a plain image.
    let mut plst = vec![0u8; 1032];
    plst[517..520].copy_from_slice(&[1, 0, 1]);
    let entry = |name: &str, id: &str, data: Vec<u8>| -> Value {
        let mut d = plist::Dictionary::new();
        d.insert("Attributes".into(), Value::String("0x0050".into()));
        if !name.is_empty() {
            d.insert("CFName".into(), Value::String(name.into()));
        }
        d.insert("Data".into(), Value::Data(data));
        d.insert("ID".into(), Value::String(id.into()));
        d.insert("Name".into(), Value::String(name.into()));
        Value::Dictionary(d)
    };
    let mut fork = plist::Dictionary::new();
    fork.insert("blkx".into(), Value::Array(vec![entry(options.name, "0", mish)]));
    fork.insert("plst".into(), Value::Array(vec![entry("", "0", plst)]));
    let mut root = plist::Dictionary::new();
    root.insert("resource-fork".into(), Value::Dictionary(fork));
    let mut xml = Vec::new();
    Value::Dictionary(root).to_writer_xml(&mut xml).map_err(|_| Error("UDIF metadata failed"))?;

    let data_len = data.len() as u64;
    let data_crc = crc32fast::hash(&data);
    let mut out = data;
    let xml_offset = out.len() as u64;
    out.extend(&xml);
    let mut koly = vec![0u8; TRAILER];
    koly[..4].copy_from_slice(b"koly");
    put32(&mut koly, 4, 4);
    put32(&mut koly, 8, 512);
    put32(&mut koly, 12, 1);
    put64(&mut koly, 32, data_len);
    put32(&mut koly, 56, 1);
    put32(&mut koly, 60, 1);
    koly[64..80].copy_from_slice(&options.segment_id);
    put32(&mut koly, 80, 2);
    put32(&mut koly, 84, 32);
    put32(&mut koly, 88, data_crc);
    put64(&mut koly, 216, xml_offset);
    put64(&mut koly, 224, xml.len() as u64);
    put32(&mut koly, 352, 2);
    put32(&mut koly, 356, 32);
    put32(&mut koly, 360, crc32fast::hash(&part_crc.to_be_bytes()));
    put32(&mut koly, 488, 2);
    put64(&mut koly, 492, sectors);
    out.extend(koly);
    Ok(out)
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_be_bytes());
}


fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn written_images_read_back_with_their_checksums() {
        let mut disk = vec![0u8; 5 << 20];
        disk[1024..1030].copy_from_slice(b"HXdata");
        disk[(3 << 20) + 7] = 9;
        let image = write(&disk, &WriteOptions { name: "whole disk (Apple_HFSX : 0)", segment_id: [7; 16] }).unwrap();
        assert!(image.len() < 64 << 10, "zero runs and zlib keep it small");
        assert_eq!(read(&image).unwrap(), disk);
        let mut broken = image.clone();
        broken[10] ^= 1;
        assert!(read(&broken).is_err());
    }
}

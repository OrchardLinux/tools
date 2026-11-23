//! A UDIF disk image read as the disk it describes, without unpacking it.
use orchard_cache::source::Source;
use orchard_cache::{Code, Error, Result};
use orchard_formats::udif::{self, Kind, Map, Partition, Trailer};
use std::collections::HashMap;

/// Metadata larger than this is refused. Firmware images have a few hundred KB.
const MAX_METADATA: u64 = 16 << 20;
/// Compressed chunks larger than this are refused. Apple uses about 1 MB.
const MAX_CHUNK: u64 = 64 << 20;
/// Decoded chunks kept in memory, so neighbouring reads don't decode again.
const CACHE_BYTES: usize = 64 << 20;

pub struct DiskImage<S> {
    source: S,
    map: Map,
    size: u64,
    cache: HashMap<usize, Vec<u8>>,
    cached: usize,
}

impl<S: Source> DiskImage<S> {
    pub fn open(mut source: S) -> Result<DiskImage<S>> {
        let at = source
            .size()
            .checked_sub(udif::TRAILER as u64)
            .ok_or_else(|| Error::new(Code::Format, "file is too small to be a disk image"))?;
        let trailer = Trailer::parse(&source.read_at(at, udif::TRAILER)?)?;
        if trailer.metadata_len > MAX_METADATA
            || trailer
                .metadata_offset
                .checked_add(trailer.metadata_len)
                .is_none_or(|end| end > at)
        {
            return Err(Error::new(
                Code::Format,
                "disk image metadata is too large or misplaced",
            ));
        }
        let xml = source.read_at(trailer.metadata_offset, trailer.metadata_len as usize)?;
        let map = udif::map(&xml, &trailer)?;
        if map.chunks.iter().any(|c| {
            matches!(c.kind, Kind::Zlib | Kind::Lzfse)
                && (c.len > MAX_CHUNK || c.disk_len() > MAX_CHUNK)
        }) {
            return Err(Error::new(
                Code::Format,
                "disk image has an oversized compressed chunk",
            ));
        }
        Ok(DiskImage {
            source,
            map,
            size: trailer.disk_size,
            cache: HashMap::new(),
            cached: 0,
        })
    }

    pub fn partitions(&self) -> &[Partition] {
        &self.map.partitions
    }

    /// The decoded bytes of compressed chunk `index`.
    fn decoded(&mut self, index: usize) -> Result<&[u8]> {
        if !self.cache.contains_key(&index) {
            let chunk = self.map.chunks[index];
            let data = self.source.read_at(chunk.offset, chunk.len as usize)?;
            let bytes = udif::decode(&chunk, &data)?;
            if self.cached + bytes.len() > CACHE_BYTES {
                self.cache.clear();
                self.cached = 0;
            }
            self.cached += bytes.len();
            self.cache.insert(index, bytes);
        }
        Ok(&self.cache[&index])
    }
}

impl<S: Source> Source for DiskImage<S> {
    fn size(&self) -> u64 {
        self.size
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let position = offset + out.len() as u64;
            let index = self.map.chunks.partition_point(|c| c.start <= position);
            let Some(chunk) = index
                .checked_sub(1)
                .map(|i| self.map.chunks[i])
                .filter(|c| position < c.end)
            else {
                return Err(Error::new(
                    Code::Format,
                    format!("disk image has no data at byte {position}"),
                ));
            };
            let index = index - 1;
            let count = (chunk.end - position).min((len - out.len()) as u64) as usize;
            let within = position - chunk.start;
            match chunk.kind {
                Kind::Zero => out.resize(out.len() + count, 0),
                Kind::Raw => out.extend(self.source.read_at(chunk.offset + within, count)?),
                Kind::Zlib | Kind::Lzfse => {
                    let within = within as usize;
                    out.extend_from_slice(&self.decoded(index)?[within..within + count]);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// One chunk to put in a test image: its type code and decoded bytes.
    struct Run(u32, Vec<u8>);

    /// A minimal single-partition UDIF image.
    fn image(runs: &[Run]) -> Vec<u8> {
        let mut file = Vec::new();
        let mut table = Vec::new();
        let mut sector = 0u64;
        for Run(code, data) in runs {
            let stored = match code {
                0 => Vec::new(),
                1 => data.clone(),
                0x8000_0005 => {
                    let mut e =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    e.write_all(data).unwrap();
                    e.finish().unwrap()
                }
                0x8000_0007 => {
                    let mut s = b"bvx-".to_vec();
                    s.extend((data.len() as u32).to_be_bytes().iter().rev());
                    s.extend(data);
                    s.extend(b"bvx$");
                    s
                }
                _ => unreachable!(),
            };
            let sectors = data.len() as u64 / udif::SECTOR;
            table.extend(code.to_be_bytes());
            table.extend([0; 4]);
            table.extend(sector.to_be_bytes());
            table.extend(sectors.to_be_bytes());
            table.extend(
                (if stored.is_empty() {
                    0
                } else {
                    file.len() as u64
                })
                .to_be_bytes(),
            );
            table.extend((stored.len() as u64).to_be_bytes());
            file.extend(&stored);
            sector += sectors;
        }
        table.extend(0xffff_ffffu32.to_be_bytes());
        table.extend([0; 36]);
        let mut mish = b"mish".to_vec();
        mish.extend(1u32.to_be_bytes());
        mish.extend(0u64.to_be_bytes());
        mish.extend(sector.to_be_bytes());
        mish.extend(0u64.to_be_bytes());
        mish.resize(200, 0);
        mish.extend((runs.len() as u32 + 1).to_be_bytes());
        mish.extend(table);

        let mut entry = plist::Dictionary::new();
        entry.insert("Name".into(), "disk image (Apple_APFS : 1)".into());
        entry.insert("Data".into(), plist::Value::Data(mish));
        let mut fork = plist::Dictionary::new();
        fork.insert("blkx".into(), vec![plist::Value::Dictionary(entry)].into());
        let mut root = plist::Dictionary::new();
        root.insert("resource-fork".into(), plist::Value::Dictionary(fork));
        let mut xml = Vec::new();
        plist::Value::Dictionary(root)
            .to_writer_xml(&mut xml)
            .unwrap();

        let data_end = file.len().max(1) as u64;
        file.resize(data_end as usize, 0);
        let metadata_offset = file.len() as u64;
        file.extend(&xml);
        let mut koly = vec![0; udif::TRAILER];
        koly[..4].copy_from_slice(b"koly");
        koly[4..8].copy_from_slice(&4u32.to_be_bytes());
        koly[8..12].copy_from_slice(&512u32.to_be_bytes());
        koly[32..40].copy_from_slice(&data_end.to_be_bytes());
        koly[56..60].copy_from_slice(&1u32.to_be_bytes());
        koly[60..64].copy_from_slice(&1u32.to_be_bytes());
        koly[0xd8..0xe0].copy_from_slice(&metadata_offset.to_be_bytes());
        koly[0xe0..0xe8].copy_from_slice(&(xml.len() as u64).to_be_bytes());
        koly[492..500].copy_from_slice(&sector.to_be_bytes());
        file.extend(koly);
        file
    }

    fn sample() -> (Vec<u8>, Vec<u8>) {
        let runs = [
            Run(1, vec![1; 1024]),
            Run(0, vec![0; 512]),
            Run(0x8000_0005, (0..2048).map(|i| (i % 7) as u8).collect()),
            Run(0x8000_0007, vec![9; 1536]),
        ];
        let disk = runs.iter().flat_map(|r| r.1.clone()).collect();
        (image(&runs), disk)
    }

    #[test]
    fn reads_every_chunk_type() {
        let (file, disk) = sample();
        let mut image = DiskImage::open(file).unwrap();
        assert_eq!(image.size(), disk.len() as u64);
        assert_eq!(image.map.chunks.len(), 4);
        assert_eq!(image.partitions()[0].name, "disk image (Apple_APFS : 1)");
        assert_eq!(image.read_at(0, disk.len()).unwrap(), disk);
    }

    #[test]
    fn reads_across_chunk_boundaries() {
        let (file, disk) = sample();
        let mut image = DiskImage::open(file).unwrap();
        for (offset, len) in [(1000, 100), (1500, 600), (3580, 1000), (5119, 1)] {
            assert_eq!(
                image.read_at(offset, len).unwrap(),
                disk[offset as usize..offset as usize + len]
            );
        }
        assert!(image.read_at(5119, 2).is_err());
    }

    #[test]
    fn rejects_damaged_files() {
        let (mut file, _) = sample();
        let at = file.len() - udif::TRAILER;
        file[at] = b'x';
        assert!(DiskImage::open(file).is_err());
        assert!(DiskImage::open(vec![0; 100]).is_err());
    }
}

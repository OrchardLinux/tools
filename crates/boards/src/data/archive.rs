//! Members of a ZIP archive such as an IPSW, read without fetching the
//! whole archive.
use orchard_cache::source::{Slice, Source};
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::zip::{self, Entry, Method};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};

/// Directories larger than this are refused. Real IPSWs stay far below it.
const MAX_DIRECTORY: u64 = 64 << 20;
/// Bytes per read while streaming a member: few round trips, bounded memory.
const CHUNK: u64 = 8 << 20;

pub struct Archive<S> {
    source: S,
    entries: BTreeMap<String, Entry>,
    directory: u64,
}

impl<S: Source> Archive<S> {
    pub fn open(mut source: S) -> Result<Archive<S>> {
        let tail_len = source.size().min(zip::MAX_TAIL as u64);
        let tail_offset = source.size() - tail_len;
        let tail = source.read_at(tail_offset, tail_len as usize)?;
        let directory = match zip::end(&tail, tail_offset)? {
            zip::End::Directory(d) => d,
            zip::End::Zip64 { locator } => {
                let record = zip::zip64_locator(&source.read_at(locator, zip::ZIP64_LOCATOR)?)?;
                zip::zip64_end(&source.read_at(record, zip::ZIP64_END)?, record)?
            }
        };
        if directory.size > MAX_DIRECTORY
            || directory
                .offset
                .checked_add(directory.size)
                .is_none_or(|end| end > directory.end)
        {
            return Err(Error::new(
                Code::Format,
                "ZIP directory is too large or misplaced",
            ));
        }
        let bytes = source.read_at(directory.offset, directory.size as usize)?;
        let mut entries = BTreeMap::new();
        for entry in zip::entries(&bytes, directory.count)? {
            if let Some(dup) = entries.insert(entry.name.clone(), entry) {
                return Err(Error::new(
                    Code::Format,
                    format!("ZIP member {} appears twice", dup.name),
                ));
            }
        }
        Ok(Archive {
            source,
            entries,
            directory: directory.offset,
        })
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn entry(&self, name: &str) -> Result<&Entry> {
        self.entries
            .get(name)
            .ok_or_else(|| Error::new(Code::NotFound, "not in the archive").context(name))
    }

    /// Reads a whole member into memory, refusing members larger than `limit`.
    pub fn read(&mut self, name: &str, limit: u64) -> Result<Vec<u8>> {
        if self.entry(name)?.size > limit {
            return Err(
                Error::new(Code::TooLarge, format!("larger than {limit} bytes")).context(name),
            );
        }
        let mut out = Vec::new();
        self.copy(name, &mut out)?;
        Ok(out)
    }

    /// Streams a member into `out`, checking its size and CRC-32. Returns the
    /// number of bytes written.
    pub fn copy(&mut self, name: &str, out: &mut impl Write) -> Result<u64> {
        let (entry, start) = self.locate(name)?;
        self.copy_from(&entry, start, out).context(name)
    }

    /// Random access to a stored (uncompressed) member, for disk images.
    /// Partial reads are not covered by the member's CRC-32.
    pub fn stored(&mut self, name: &str) -> Result<Slice<&mut S>> {
        let (entry, start) = self.locate(name)?;
        if entry.method != Method::Stored {
            return Err(
                Error::new(Code::Format, "compressed, so it can't be read at random").context(name),
            );
        }
        Slice::new(&mut self.source, start, entry.size).context(name)
    }

    fn copy_from(&mut self, entry: &Entry, start: u64, out: &mut impl Write) -> Result<u64> {
        let mut chunks = Chunks {
            source: &mut self.source,
            next: start,
            end: start + entry.compressed_size,
            buffer: io::Cursor::new(Vec::new()),
        };
        let (written, crc) = match entry.method {
            Method::Stored => copy_checked(&mut chunks, out, entry.size)?,
            Method::Deflate => {
                let mut inflate = flate2::read::DeflateDecoder::new(&mut chunks);
                let result = copy_checked(&mut inflate, out, entry.size)?;
                if inflate.total_in() != entry.compressed_size {
                    return Err(Error::new(Code::Format, "data after the compressed stream"));
                }
                result
            }
            Method::Other(_) => unreachable!("rejected by locate"),
        };
        if written != entry.size || crc != entry.crc32 {
            return Err(Error::new(Code::Format, "wrong size or CRC-32"));
        }
        Ok(written)
    }

    /// The entry and where its data starts, after checking its local header.
    fn locate(&mut self, name: &str) -> Result<(Entry, u64)> {
        let entry = self.entry(name)?.clone();
        self.data_start(&entry)
            .map(|start| (entry, start))
            .context(name)
    }

    fn data_start(&mut self, entry: &Entry) -> Result<u64> {
        if entry.encrypted || matches!(entry.method, Method::Other(_)) {
            return Err(Error::new(
                Code::Format,
                "encrypted or uses an unsupported compression",
            ));
        }
        if entry.method == Method::Stored && entry.size != entry.compressed_size {
            return Err(Error::new(Code::Format, "stored, but its two sizes differ"));
        }
        let header = self.source.read_at(entry.header, zip::LOCAL_HEADER)?;
        let local = zip::local_header(&header, entry)?;
        let name_at = entry.header + zip::LOCAL_HEADER as u64;
        if self.source.read_at(name_at, local.name_len)? != entry.name.as_bytes() {
            return Err(Error::new(Code::Format, "local header names another file"));
        }
        let start = name_at + (local.name_len + local.extra_len) as u64;
        if start
            .checked_add(entry.compressed_size)
            .is_none_or(|end| end > self.directory)
        {
            return Err(Error::new(Code::Format, "overlaps the ZIP directory"));
        }
        Ok(start)
    }
}

fn copy_checked(from: &mut impl Read, out: &mut impl Write, size: u64) -> Result<(u64, u32)> {
    let mut buffer = vec![0; 64 << 10];
    let mut crc = crc32fast::Hasher::new();
    let mut written = 0u64;
    loop {
        let n = from.read(&mut buffer).map_err(read_error)?;
        if n == 0 {
            return Ok((written, crc.finalize()));
        }
        written += n as u64;
        if written > size {
            return Err(Error::new(
                Code::Format,
                "longer than its directory entry says",
            ));
        }
        out.write_all(&buffer[..n])?;
        crc.update(&buffer[..n]);
    }
}

/// Unwraps source errors that passed through `io::Read`. Anything else came
/// from the decompressor, so the data is bad.
fn read_error(e: io::Error) -> Error {
    if e.get_ref().is_some_and(|inner| inner.is::<Error>()) {
        return *e.into_inner().unwrap().downcast::<Error>().unwrap();
    }
    Error::new(Code::Format, e.to_string())
}

/// A member's bytes as an `io::Read`, fetched in large chunks.
struct Chunks<'a, S> {
    source: &'a mut S,
    next: u64,
    end: u64,
    buffer: io::Cursor<Vec<u8>>,
}

impl<S: Source> Read for Chunks<'_, S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buffer.position() == self.buffer.get_ref().len() as u64 {
            let len = (self.end - self.next).min(CHUNK) as usize;
            if len == 0 {
                return Ok(0);
            }
            self.buffer = io::Cursor::new(
                self.source
                    .read_at(self.next, len)
                    .map_err(io::Error::other)?,
            );
            self.next += len as u64;
        }
        self.buffer.read(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A minimal ZIP writer: (name, data, deflate) per member.
    fn zip(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut directory) = (Vec::new(), Vec::new());
        for (name, data, deflate) in members {
            let stored = if *deflate {
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let crc = crc32fast::hash(data);
            let fixed = |sig: &[u8], central: bool| {
                let mut h = sig.to_vec();
                if central {
                    h.extend(20u16.to_le_bytes());
                }
                h.extend(20u16.to_le_bytes());
                h.extend(0u16.to_le_bytes());
                h.extend(method.to_le_bytes());
                h.extend([0; 4]);
                h.extend(crc.to_le_bytes());
                h.extend((stored.len() as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend(0u16.to_le_bytes());
                h
            };
            let offset = out.len() as u32;
            out.extend(fixed(b"PK\x03\x04", false));
            out.extend(name.as_bytes());
            out.extend(&stored);
            directory.extend(fixed(b"PK\x01\x02", true));
            // Comment length, disk, internal and external attributes.
            directory.extend([0; 10]);
            directory.extend(offset.to_le_bytes());
            directory.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend(&directory);
        out.extend(b"PK\x05\x06\0\0\0\0");
        out.extend((members.len() as u16).to_le_bytes());
        out.extend((members.len() as u16).to_le_bytes());
        out.extend((directory.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    fn sample() -> Vec<u8> {
        zip(&[
            ("BuildManifest.plist", b"<plist/>", true),
            ("Firmware/all_flash/DeviceTree.im4p", &[7; 5000], false),
        ])
    }

    #[test]
    fn reads_stored_and_deflated_members() {
        let mut archive = Archive::open(sample()).unwrap();
        assert_eq!(archive.entries().count(), 2);
        assert_eq!(
            archive.read("BuildManifest.plist", 100).unwrap(),
            b"<plist/>"
        );
        assert_eq!(
            archive
                .read("Firmware/all_flash/DeviceTree.im4p", 5000)
                .unwrap(),
            [7; 5000]
        );
    }

    #[test]
    fn random_access_into_a_stored_member() {
        let mut archive = Archive::open(sample()).unwrap();
        let mut slice = archive
            .stored("Firmware/all_flash/DeviceTree.im4p")
            .unwrap();
        assert_eq!(slice.size(), 5000);
        assert_eq!(slice.read_at(4998, 2).unwrap(), [7, 7]);
        assert!(archive.stored("BuildManifest.plist").is_err());
    }

    #[test]
    fn limits_and_missing_members() {
        let mut archive = Archive::open(sample()).unwrap();
        let e = archive.read("BuildManifest.plist", 3).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (Code::TooLarge, "BuildManifest.plist: larger than 3 bytes")
        );
        let e = archive.read("nope", 100).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (Code::NotFound, "nope: not in the archive")
        );
    }

    #[test]
    fn detects_corrupted_data() {
        let mut bytes = sample();
        let at = bytes.windows(3).position(|w| w == [7, 7, 7]).unwrap();
        bytes[at] = 8;
        let mut archive = Archive::open(bytes).unwrap();
        let e = archive
            .read("Firmware/all_flash/DeviceTree.im4p", 5000)
            .unwrap_err();
        assert_eq!(e.code, Code::Format);
    }

    #[test]
    fn duplicate_names_are_refused() {
        assert!(Archive::open(zip(&[("a", b"1", false), ("a", b"2", false)])).is_err());
    }
}

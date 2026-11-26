//! The root image onto its partition: a zstd stream of the 4 KiB blocks the
//! release's map names, in the map's order (orchard's `orchard-write`). One
//! thread reads, hashes and decodes while this one writes, so the disk and
//! the decoder don't wait for each other.
use crate::disk::Part;
use crate::download::hex;
use orchard_cache::{Code, Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;

const BLOCK: u64 = 4096;
const PIECE: usize = 4 << 20;

/// The release's `root.map.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Map {
    pub block_size: u64,
    /// `[start, count]` runs of blocks, in stream order.
    pub ranges: Vec<[u64; 2]>,
}

impl Map {
    pub fn parse(json: &[u8]) -> Result<Map> {
        let map: Map = serde_json::from_slice(json).map_err(|e| Error::new(Code::Format, format!("the root map is unreadable: {e}")))?;
        if map.block_size != BLOCK || map.ranges.is_empty() {
            return Err(Error::new(Code::Format, "the root map is not one of 4 KiB blocks"));
        }
        Ok(map)
    }

    pub fn restored_bytes(&self) -> u64 {
        self.ranges.iter().map(|r| r[1] * BLOCK).sum()
    }

    /// The first byte past the last block the map names.
    pub fn reach(&self) -> u64 {
        self.ranges.iter().map(|r| (r[0] + r[1]) * BLOCK).max().unwrap_or(0)
    }
}

struct Hashing<R> {
    inner: R,
    hash: Sha256,
    count: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }
}

/// Writes the image at `stream` (`bytes` long, with this SHA-256) onto
/// `part` as `map` says. `wrote` hears the restored bytes so far.
pub fn restore(stream: &Path, bytes: u64, sha256: &str, map: &Map, part: &Part, wrote: &dyn Fn(u64)) -> Result<()> {
    if map.reach() > part.bytes {
        return Err(Error::new(Code::TooLarge, "the root image does not fit on its partition"));
    }
    let file = std::fs::File::open(stream).map_err(|e| Error::from(e).context(stream.display()))?;
    let (pieces, incoming) = mpsc::sync_channel::<Result<Vec<u8>>>(4);
    let expected = sha256.to_owned();
    let decoder = std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut source = Hashing { inner: std::io::BufReader::with_capacity(1 << 20, file), hash: Sha256::new(), count: 0 };
            {
                let mut decoded = ruzstd::decoding::StreamingDecoder::new(&mut source)
                    .map_err(|e| Error::new(Code::Format, format!("the root image is not a zstd stream: {e}")))?;
                loop {
                    let mut piece = vec![0u8; PIECE];
                    let mut filled = 0;
                    while filled < PIECE {
                        let n = decoded.read(&mut piece[filled..]).map_err(|e| Error::new(Code::Format, format!("the root image is damaged: {e}")))?;
                        if n == 0 {
                            break;
                        }
                        filled += n;
                    }
                    piece.truncate(filled);
                    if piece.is_empty() || pieces.send(Ok(piece)).is_err() {
                        break;
                    }
                    if filled < PIECE {
                        break;
                    }
                }
            }
            // What the decoder left unread still belongs to the file's hash.
            std::io::copy(&mut source, &mut std::io::sink())?;
            if source.count != bytes || hex(&source.hash.finalize()) != expected {
                return Err(Error::new(Code::Format, "the root image differs from what the release states"));
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = pieces.send(Err(e));
        }
    });
    let mut ranges = map.ranges.iter();
    let (mut at, mut left) = (0u64, 0u64);
    let mut total = 0u64;
    let mut outcome = Ok(());
    'pieces: for piece in incoming {
        let piece = match piece {
            Ok(piece) => piece,
            Err(e) => {
                outcome = Err(e);
                break;
            }
        };
        let mut data = &piece[..];
        while !data.is_empty() {
            if left == 0 {
                let Some(range) = ranges.next() else {
                    outcome = Err(Error::new(Code::Format, "the root image holds more than its map names"));
                    break 'pieces;
                };
                (at, left) = (range[0] * BLOCK, range[1] * BLOCK);
            }
            let take = (left as usize).min(data.len());
            if let Err(e) = part.write_at(&data[..take], at) {
                outcome = Err(e);
                break 'pieces;
            }
            at += take as u64;
            left -= take as u64;
            total += take as u64;
            data = &data[take..];
        }
        wrote(total);
    }
    let _ = decoder.join();
    outcome?;
    if total != map.restored_bytes() {
        return Err(Error::new(Code::Format, "the root image holds less than its map names"));
    }
    part.sync()
}

//! Apple's `complzss` container. iBoot before iOS 14 only accepts a
//! kernelcache in this form.
use crate::{Error, Result};

const HEADER: usize = 0x180;
const RING: usize = 4096;
const MAX_MATCH: usize = 18;

/// Compresses using literals only: 12.5% larger than the input, which pre-14
/// iBoot accepts, and needs no match search.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let size = u32::try_from(data.len()).map_err(|_| Error("LZSS input exceeds 4 GiB"))?;
    let packed = u32::try_from(data.len() + data.len().div_ceil(8))
        .map_err(|_| Error("LZSS output exceeds 4 GiB"))?;
    let mut out = vec![0; HEADER];
    out[..8].copy_from_slice(b"complzss");
    out[8..12].copy_from_slice(&adler32(data).to_be_bytes());
    out[12..16].copy_from_slice(&size.to_be_bytes());
    out[16..20].copy_from_slice(&packed.to_be_bytes());
    // Version 1, as ipsw writes it, so the output stays byte-identical.
    out[20..24].copy_from_slice(&1u32.to_be_bytes());
    for chunk in data.chunks(8) {
        out.push(0xff);
        out.extend_from_slice(chunk);
    }
    Ok(out)
}

/// Returns the decompressed data and whatever follows the compressed stream
/// (the monitor stub on A7–A9 kernelcaches). Refuses output over `limit`.
pub fn decompress(data: &[u8], limit: usize) -> Result<(Vec<u8>, &[u8])> {
    if data.len() < HEADER || &data[..8] != b"complzss" {
        return Err(Error("not a complzss container"));
    }
    let word = |p: usize| u32::from_be_bytes(data[p..p + 4].try_into().unwrap());
    let (checksum, size, packed) = (word(8), word(12) as usize, word(16) as usize);
    if size > limit {
        return Err(Error("LZSS output exceeds limit"));
    }
    let src = data
        .get(HEADER..HEADER + packed)
        .ok_or(Error("truncated LZSS stream"))?;
    let mut ring = [b' '; RING];
    ring[RING - MAX_MATCH..].fill(0);
    let mut r = RING - MAX_MATCH;
    let mut out = Vec::with_capacity(size);
    let mut i = 0;
    while i < src.len() {
        let flags = src[i];
        i += 1;
        for bit in 0..8 {
            if i == src.len() {
                break;
            }
            let (position, count) = if flags & 1 << bit != 0 {
                ring[r] = src[i];
                i += 1;
                (r, 1)
            } else {
                let &[low, high] = src.get(i..i + 2).ok_or(Error("truncated LZSS reference"))?
                else {
                    unreachable!()
                };
                i += 2;
                (
                    usize::from(low) | usize::from(high & 0xf0) << 4,
                    usize::from(high & 0x0f) + 3,
                )
            };
            for k in 0..count {
                let c = ring[(position + k) % RING];
                out.push(c);
                ring[r] = c;
                r = (r + 1) % RING;
            }
            if out.len() > size {
                return Err(Error("LZSS output exceeds declared size"));
            }
        }
    }
    if out.len() != size || adler32(&out) != checksum {
        return Err(Error("LZSS size or checksum mismatch"));
    }
    Ok((out, &data[HEADER + packed..]))
}

fn adler32(data: &[u8]) -> u32 {
    // 5552 bytes is the most that can be summed before u32 overflows.
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &c in chunk {
            a += u32::from(c);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler32_reference() {
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        let long = vec![0xff; 100_000];
        let (mut a, mut b) = (1u64, 0u64);
        for &c in &long {
            a = (a + u64::from(c)) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(&long), (b << 16 | a) as u32);
    }

    #[test]
    fn round_trip_with_trailer() {
        for n in [0, 1, 7, 8, 9, 4095, 8192] {
            let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let mut packed = compress(&data).unwrap();
            packed.extend_from_slice(b"stub");
            assert_eq!(decompress(&packed, n).unwrap(), (data, &b"stub"[..]));
        }
    }

    #[test]
    fn back_references() {
        // "abc" as literals, then a reference copying 6 bytes from ring slot
        // 4078 (where output starts), and a reference into the initial spaces.
        let stream = [0b0000_0111, b'a', b'b', b'c', 0xee, 0xf3, 0x00, 0x00];
        let expected = b"abcabcabc   ";
        let mut data = vec![0; HEADER];
        data[..8].copy_from_slice(b"complzss");
        data[8..12].copy_from_slice(&adler32(expected).to_be_bytes());
        data[12..16].copy_from_slice(&(expected.len() as u32).to_be_bytes());
        data[16..20].copy_from_slice(&(stream.len() as u32).to_be_bytes());
        data.extend_from_slice(&stream);
        assert_eq!(decompress(&data, 100).unwrap().0, expected);
    }

    #[test]
    fn rejects_corruption() {
        let mut packed = compress(b"hello world").unwrap();
        packed[8] ^= 1;
        assert!(decompress(&packed, 100).is_err());
        assert!(decompress(&compress(b"hello").unwrap(), 4).is_err());
        assert!(decompress(b"complzss", 100).is_err());
    }
}

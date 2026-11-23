//! zlib streams of a known decoded size, as UDIF and APFS store them.
use crate::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};

/// Decodes one complete zlib stream that must produce exactly `expected` bytes
/// and use all of `input`.
pub fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let (out, used) = stream(input, expected)?;
    if used != input.len() {
        return Err(Error("data after the zlib stream"));
    }
    Ok(out)
}

/// Like [`decode`], but allows bytes after the end of the stream, as in
/// decmpfs resource fork chunks.
pub fn decode_padded(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    Ok(stream(input, expected)?.0)
}

fn stream(input: &[u8], expected: usize) -> Result<(Vec<u8>, usize)> {
    // One spare byte shows whether the stream would have produced more.
    let mut out = vec![0; expected + 1];
    let mut z = Decompress::new(true);
    let status = z
        .decompress(input, &mut out, FlushDecompress::Finish)
        .map_err(|_| Error("corrupt zlib stream"))?;
    if status != Status::StreamEnd || z.total_out() != expected as u64 {
        return Err(Error("zlib stream has the wrong size"));
    }
    out.truncate(expected);
    Ok((out, z.total_in() as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn exact_size_only() {
        let z = compress(&[3; 1000]);
        assert_eq!(decode(&z, 1000).unwrap(), [3; 1000]);
        assert!(decode(&z, 999).is_err());
        assert!(decode(&z, 1001).is_err());
        let mut trailing = z.clone();
        trailing.push(0);
        assert!(decode(&trailing, 1000).is_err());
        assert_eq!(decode_padded(&trailing, 1000).unwrap(), [3; 1000]);
    }
}

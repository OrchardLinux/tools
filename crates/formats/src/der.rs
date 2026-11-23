//! The subset of DER that IMG4 uses: definite lengths up to four bytes and
//! multi-byte tag numbers for the four-letter private tags in manifests.
use crate::{Error, Result};

pub const INTEGER: u8 = 0x02;
pub const OCTET_STRING: u8 = 0x04;
pub const IA5_STRING: u8 = 0x16;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;
/// Context-specific, constructed, tag number 0.
pub const EXPLICIT_0: u8 = 0xa0;
/// Private, constructed, tag number in the following bytes.
pub const PRIVATE: u8 = 0xff;

/// One element and the bytes after it.
#[derive(Debug, Clone, Copy)]
pub struct Item<'a> {
    /// The first tag byte: class, constructed bit and short tag number.
    pub tag: u8,
    /// The tag number, read from the following bytes when it is 31 or more.
    pub number: u64,
    pub body: &'a [u8],
    /// The whole element, header included.
    pub encoded: &'a [u8],
    pub rest: &'a [u8],
}

impl<'a> Item<'a> {
    /// The body as an IA5 string.
    pub fn ia5(&self) -> Result<&'a str> {
        if self.tag != IA5_STRING || !self.body.is_ascii() {
            return Err(Error("expected an IA5 string"));
        }
        // ASCII is valid UTF-8.
        Ok(std::str::from_utf8(self.body).unwrap())
    }

    /// The body as a non-negative integer of at most 32 bits.
    pub fn u32(&self) -> Result<u32> {
        let body = match self.body {
            [0, rest @ ..] if !rest.is_empty() => rest,
            body => body,
        };
        if self.tag != INTEGER || body.is_empty() || body.len() > 4 || self.body[0] & 0x80 != 0 {
            return Err(Error("expected a 32-bit unsigned integer"));
        }
        Ok(body.iter().fold(0, |n, b| n << 8 | u32::from(*b)))
    }

    /// The single element inside this one's body.
    pub fn only_child(&self) -> Result<Item<'a>> {
        let child = read(self.body)?;
        if !child.rest.is_empty() {
            return Err(Error("unexpected data after element"));
        }
        Ok(child)
    }
}

pub fn read(data: &[u8]) -> Result<Item<'_>> {
    let truncated = Error("truncated DER");
    let tag = *data.first().ok_or(truncated)?;
    let mut p = 1;
    let mut number = u64::from(tag & 0x1f);
    if number == 0x1f {
        number = 0;
        loop {
            let b = *data.get(p).ok_or(truncated)?;
            p += 1;
            if number >> 57 != 0 {
                return Err(Error("DER tag number too large"));
            }
            number = number << 7 | u64::from(b & 0x7f);
            if b & 0x80 == 0 {
                break;
            }
        }
        if number < 0x1f {
            return Err(Error("non-canonical DER tag"));
        }
    }
    let lead = *data.get(p).ok_or(truncated)?;
    p += 1;
    let mut len = usize::from(lead);
    if lead & 0x80 != 0 {
        let count = usize::from(lead & 0x7f);
        let bytes = data.get(p..p + count).ok_or(truncated)?;
        if !(1..=4).contains(&count) || bytes[0] == 0 {
            return Err(Error("unsupported DER length"));
        }
        len = bytes.iter().fold(0, |n, b| n << 8 | usize::from(*b));
        if len < 0x80 {
            return Err(Error("non-canonical DER length"));
        }
        p += count;
    }
    let end = p
        .checked_add(len)
        .filter(|&e| e <= data.len())
        .ok_or(truncated)?;
    Ok(Item {
        tag,
        number,
        body: &data[p..end],
        encoded: &data[..end],
        rest: &data[end..],
    })
}

/// Every element in `data`, in order.
pub fn items(mut data: &[u8]) -> Result<Vec<Item<'_>>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let item = read(data)?;
        data = item.rest;
        out.push(item);
    }
    Ok(out)
}

/// Encodes one element with a single-byte tag.
pub fn write(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 0x80 {
        out.push(body.len() as u8);
    } else {
        let bytes = body.len().to_be_bytes();
        let first = bytes
            .iter()
            .position(|&b| b != 0)
            .unwrap_or(bytes.len() - 1);
        out.push(0x80 | (bytes.len() - first) as u8);
        out.extend_from_slice(&bytes[first..]);
    }
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_round_trip() {
        for n in [0, 1, 127, 128, 255, 256, 70_000] {
            let body = vec![7; n];
            let encoded = write(OCTET_STRING, &body);
            let item = read(&encoded).unwrap();
            assert_eq!(
                (item.tag, item.body.len(), item.rest.len()),
                (OCTET_STRING, n, 0)
            );
        }
    }

    #[test]
    fn rejects_non_canonical_and_truncated() {
        assert!(read(&[OCTET_STRING, 0x81, 0x05, 0, 0, 0, 0, 0]).is_err());
        assert!(read(&[OCTET_STRING, 0x82, 0x00, 0x80]).is_err());
        assert!(read(&[OCTET_STRING, 3, 1, 2]).is_err());
        assert!(read(&[PRIVATE, 0x80]).is_err());
    }

    #[test]
    fn integers() {
        assert_eq!(read(&[INTEGER, 1, 0x7f]).unwrap().u32(), Ok(0x7f));
        assert_eq!(read(&[INTEGER, 2, 0, 0xff]).unwrap().u32(), Ok(0xff));
        assert!(read(&[INTEGER, 1, 0x80]).unwrap().u32().is_err());
        assert!(read(&[INTEGER, 5, 1, 0, 0, 0, 0]).unwrap().u32().is_err());
    }
}

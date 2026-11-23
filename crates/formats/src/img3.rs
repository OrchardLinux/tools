//! IMG3 firmware containers, as the 32-bit chips' iBoot images use them. An
//! IMG3 is a header and a sequence of four-letter tags; the payload is the `DATA` tag,
//! and a `KBAG` tag means it is AES-encrypted. Tag names are stored little-
//! endian, so `DATA` appears as `ATAD` in the bytes.
use crate::{crypto::Key, Error, Result};
use std::ops::Range;

/// A parsed IMG3, borrowing the original bytes. It remembers every tag's
/// place so [`plain`](Image::plain) can rebuild the container around a new
/// payload.
pub struct Image<'a> {
    bytes: &'a [u8],
    tags: Vec<Range<usize>>,
    /// The payload bytes inside the `DATA` tag, padding included.
    data: Range<usize>,
    /// The payload's real length, which the padding may exceed.
    size: usize,
    /// Whether a `KBAG` tag is present, i.e. the payload is encrypted.
    pub encrypted: bool,
}

fn word(bytes: &[u8], at: usize) -> Result<usize> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(Error("truncated IMG3"))?
            .try_into()
            .unwrap(),
    ) as usize)
}

impl<'a> Image<'a> {
    /// Reads the header and walks every tag, locating the one `DATA` tag.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.get(..4) != Some(b"3gmI")
            || bytes.len() < 20
            || word(bytes, 4)? != bytes.len()
            || word(bytes, 8)? != bytes.len() - 20
        {
            return Err(Error("invalid IMG3 header"));
        }
        let mut image = Self {
            bytes,
            tags: Vec::new(),
            data: 0..0,
            size: 0,
            encrypted: false,
        };
        let mut at = 20;
        while at < bytes.len() {
            let total = word(bytes, at + 4)?;
            let size = word(bytes, at + 8)?;
            let end = at
                .checked_add(total)
                .ok_or(Error("IMG3 tag size overflow"))?;
            if total < 12 || size > total - 12 || end > bytes.len() {
                return Err(Error("invalid IMG3 tag size"));
            }
            match &bytes[at..at + 4] {
                b"ATAD" => {
                    if !image.data.is_empty() {
                        return Err(Error("repeated IMG3 DATA tag"));
                    }
                    image.data = at + 12..end;
                    image.size = size;
                }
                b"GABK" => image.encrypted = true,
                _ => {}
            }
            image.tags.push(at..end);
            at = end;
        }
        if image.data.is_empty() {
            return Err(Error("missing IMG3 DATA tag"));
        }
        Ok(image)
    }

    /// The unique production AES-256 bag, bound to the image's stored type.
    pub fn keybag(&self, kind: &[u8; 4]) -> Result<[u8; 48]> {
        if &self.bytes[16..20] != kind {
            return Err(Error("IMG3 keybag image type mismatch"));
        }
        let mut found = None;
        let mut seen = [false; 2];
        for tag in &self.tags {
            if &self.bytes[tag.start..tag.start + 4] != b"GABK" {
                continue;
            }
            if word(self.bytes, tag.start + 8)? != 56 {
                return Err(Error("invalid IMG3 keybag size"));
            }
            let bag = &self.bytes[tag.start + 12..tag.start + 68];
            let index = word(bag, 0)?;
            if !(1..=2).contains(&index) || word(bag, 4)? != 256 || seen[index - 1] {
                return Err(Error("unsupported or ambiguous IMG3 keybag"));
            }
            seen[index - 1] = true;
            if index == 1 {
                found = Some(bag[8..].try_into().unwrap());
            }
        }
        found.ok_or(Error("production IMG3 keybag missing"))
    }

    /// The payload, decrypted with `key` when the image is encrypted.
    pub fn decode(&self, key: Option<&Key>) -> Result<Vec<u8>> {
        let mut data = self.bytes[self.data.clone()].to_vec();
        if self.encrypted {
            key.ok_or(Error("encrypted IMG3 needs a key"))?
                .decrypt(&mut data)?;
        }
        data.truncate(self.size);
        Ok(data)
    }

    /// Re-encrypts `data` into a copy of this image, keeping every tag. The
    /// image must be encrypted and `data` the same length as its payload, so
    /// the result is a drop-in replacement the device will accept.
    pub fn encrypt(&self, data: &[u8], key: &Key) -> Result<Vec<u8>> {
        if !self.encrypted
            || data.len() != self.size
            || data.len().div_ceil(16) * 16 != self.data.len()
        {
            return Err(Error("IMG3 payload differs from encrypted template"));
        }
        let mut payload = data.to_vec();
        payload.resize(self.data.len(), 0);
        key.encrypt(&mut payload)?;
        let mut image = self.bytes.to_vec();
        image[self.data.clone()].copy_from_slice(&payload);
        Ok(image)
    }

    /// A new image with `data` as a plaintext payload and the `KBAG` tag
    /// dropped, so a patched image can be booted without re-encrypting it.
    /// The other tags are copied unchanged and the header sizes are rewritten.
    pub fn plain(&self, data: &[u8]) -> Result<Vec<u8>> {
        let padded = data.len().div_ceil(16) * 16;
        let tag_size = u32::try_from(padded + 12).map_err(|_| Error("IMG3 payload too large"))?;
        let mut image = self.bytes[..20].to_vec();
        image[12..16].fill(0);
        for tag in &self.tags {
            match &self.bytes[tag.start..tag.start + 4] {
                b"GABK" => {}
                b"ATAD" => {
                    image.extend(b"ATAD");
                    image.extend(tag_size.to_le_bytes());
                    image.extend((data.len() as u32).to_le_bytes());
                    image.extend(data);
                    image.resize(image.len() + padded - data.len(), 0);
                }
                _ => image.extend(&self.bytes[tag.clone()]),
            }
        }
        let size = u32::try_from(image.len()).map_err(|_| Error("IMG3 image too large"))?;
        image[4..8].copy_from_slice(&size.to_le_bytes());
        image[8..12].copy_from_slice(&(size - 20).to_le_bytes());
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    /// One tag: its four stored bytes (already reversed, e.g. `ATAD`), the
    /// `size` field, and the body.
    fn tag(stored: &[u8; 4], size: usize, body: &[u8]) -> Vec<u8> {
        let mut t = stored.to_vec();
        t.extend((12 + body.len() as u32).to_le_bytes());
        t.extend((size as u32).to_le_bytes());
        t.extend(body);
        t
    }

    /// An IMG3 wrapping the given tags, with correct header sizes.
    fn img3(tags: &[Vec<u8>]) -> Vec<u8> {
        let body = tags.concat();
        let full = 20 + body.len();
        let mut img = b"3gmI".to_vec();
        img.extend((full as u32).to_le_bytes());
        img.extend((body.len() as u32).to_le_bytes());
        img.extend([0u8; 8]);
        img.extend(body);
        img
    }

    fn key() -> Key {
        Key {
            key: hex("000102030405060708090a0b0c0d0e0f").unwrap(),
            iv: [0; 16],
        }
    }

    #[test]
    fn production_bag_checks_type_size_bits_and_duplicates() {
        let bag = |n: u32, bits: u32| {
            tag(
                b"GABK",
                56,
                &[
                    n.to_le_bytes().as_slice(),
                    bits.to_le_bytes().as_slice(),
                    &[5; 48],
                ]
                .concat(),
            )
        };
        let image = |bags: Vec<Vec<u8>>| {
            let mut tags = vec![tag(b"ATAD", 16, &[0; 16])];
            tags.extend(bags);
            let mut bytes = img3(&tags);
            bytes[16..20].copy_from_slice(b"ssbi");
            bytes
        };
        let bytes = image(vec![bag(2, 256), bag(1, 256)]);
        assert_eq!(
            Image::parse(&bytes).unwrap().keybag(b"ssbi").unwrap(),
            [5; 48]
        );
        assert!(Image::parse(&bytes).unwrap().keybag(b"cebi").is_err());
        for bags in [
            vec![],
            vec![bag(2, 256)],
            vec![bag(1, 128)],
            vec![bag(3, 256)],
            vec![bag(1, 256), bag(1, 256)],
            vec![tag(b"GABK", 4, &[0; 4])],
        ] {
            assert!(Image::parse(&image(bags)).unwrap().keybag(b"ssbi").is_err());
        }
    }

    #[test]
    fn parse_rejects_malformed_images() {
        let data = tag(b"ATAD", 4, b"abcd");
        assert!(Image::parse(b"xxxx").is_err());
        assert!(Image::parse(&[0; 8]).is_err());
        // A header whose declared size does not match the bytes.
        let mut bad = img3(&[data]);
        bad[4] ^= 1;
        assert!(Image::parse(&bad).is_err());
        // A tag that runs past the end.
        let mut long = tag(b"ATAD", 4, b"abcd");
        long[4] = 0xff;
        assert!(Image::parse(&img3(&[long])).is_err());
        // No DATA tag at all.
        assert!(Image::parse(&img3(&[tag(b"SREV", 3, b"1.0")])).is_err());
    }

    #[test]
    fn decodes_a_plaintext_payload() {
        let payload = b"hello iBoot";
        let image = img3(&[
            tag(b"SREV", 3, b"1.0"),
            tag(b"ATAD", payload.len(), payload),
        ]);
        let parsed = Image::parse(&image).unwrap();
        assert!(!parsed.encrypted);
        assert_eq!(parsed.decode(None).unwrap(), payload);
    }

    #[test]
    fn decodes_and_re_encrypts_an_encrypted_payload() {
        let payload: Vec<u8> = (0..20).collect(); // 20 real bytes, padded to 32
        let mut body = payload.clone();
        body.resize(32, 0);
        key().encrypt(&mut body).unwrap();
        let image = img3(&[tag(b"GABK", 0, &[]), tag(b"ATAD", payload.len(), &body)]);

        let parsed = Image::parse(&image).unwrap();
        assert!(parsed.encrypted);
        assert_eq!(
            parsed.decode(None).unwrap_err(),
            Error("encrypted IMG3 needs a key")
        );
        assert_eq!(parsed.decode(Some(&key())).unwrap(), payload);
        // Re-encrypting the same payload reproduces the original container.
        assert_eq!(parsed.encrypt(&payload, &key()).unwrap(), image);
        // A payload of the wrong length is refused.
        assert!(parsed.encrypt(&payload[..19], &key()).is_err());
    }

    #[test]
    fn plain_drops_the_keybag_and_round_trips() {
        let payload: Vec<u8> = (0..20).collect();
        let mut body = payload.clone();
        body.resize(32, 0);
        key().encrypt(&mut body).unwrap();
        let image = img3(&[tag(b"GABK", 0, &[]), tag(b"ATAD", payload.len(), &body)]);
        let parsed = Image::parse(&image).unwrap();

        let patched: Vec<u8> = (100..120).collect();
        let plain = parsed.plain(&patched).unwrap();
        let reparsed = Image::parse(&plain).unwrap();
        assert!(!reparsed.encrypted, "the keybag is gone");
        assert_eq!(reparsed.decode(None).unwrap(), patched);
    }
}

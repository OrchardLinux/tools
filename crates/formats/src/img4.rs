//! IMG4 boot images and their IM4M manifests, as the 64-bit chips' iBoot uses them.
//! An IMG4 is DER: the payload is an IM4P container and the signature is an
//! IM4M manifest. [`wrap`] builds one; [`decode`] unwraps and decompresses a
//! payload; [`encrypted`] reports whether a payload needs a key first.
use crate::{
    crypto::Key,
    der::{self, EXPLICIT_0, IA5_STRING, OCTET_STRING, SEQUENCE},
    im4p, lzfse, lzss, Error, Result,
};

/// Whether the IM4P in `bytes` carries a keybag, i.e. its payload is
/// encrypted. A keybag is an octet string after the payload holding a
/// non-empty sequence.
pub fn encrypted(bytes: &[u8]) -> Result<bool> {
    im4p::payload(bytes)?;
    let outer = der::read(bytes)?;
    let fields = der::items(outer.body)?;
    for field in fields.iter().skip(4).filter(|f| f.tag == OCTET_STRING) {
        let bags = der::read(field.body)?;
        if bags.tag != SEQUENCE || !bags.rest.is_empty() {
            return Err(Error("invalid IM4P keybags"));
        }
        if !bags.body.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The IM4P payload in `bytes`, decrypted with `key` when encrypted and then
/// decompressed if it is LZSS or LZFSE. `limit` bounds both the stored payload
/// and the decompressed result.
pub fn decode(bytes: &[u8], key: Option<&Key>, limit: usize) -> Result<Vec<u8>> {
    let payload = im4p::payload(bytes)?;
    if payload.len() > limit {
        return Err(Error("IM4P payload exceeds limit"));
    }
    let mut data = payload.to_vec();
    if encrypted(bytes)? {
        key.ok_or(Error("encrypted IM4P needs a key"))?
            .decrypt(&mut data)?;
    }
    if data.starts_with(b"complzss") {
        let (plain, padding) = lzss::decompress(&data, limit)?;
        if padding.iter().any(|&b| b != 0) {
            return Err(Error("unexpected bytes after compressed firmware"));
        }
        Ok(plain)
    } else if data.starts_with(b"bvx") {
        let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        lzfse::decode_limited(&data[..end], limit)
    } else {
        Ok(data)
    }
}

/// Checks that `bytes` is an IM4M manifest: a sequence beginning with the
/// string `IM4M`.
pub fn manifest(bytes: &[u8]) -> Result<()> {
    let item = der::read(bytes)?;
    if item.tag != SEQUENCE || !item.rest.is_empty() || der::read(item.body)?.ia5()? != "IM4M" {
        return Err(Error("not an IM4M manifest"));
    }
    Ok(())
}

/// Builds an IMG4 from a four-letter `kind` (such as `ibss`), a plaintext
/// `data` payload, and an `im4m` manifest. The manifest is checked first.
pub fn wrap(kind: &[u8; 4], data: &[u8], im4m: &[u8]) -> Result<Vec<u8>> {
    manifest(im4m)?;
    let mut payload = der::write(IA5_STRING, b"IM4P");
    payload.extend(der::write(IA5_STRING, kind));
    payload.extend(der::write(IA5_STRING, b""));
    payload.extend(der::write(OCTET_STRING, data));
    let mut image = der::write(IA5_STRING, b"IMG4");
    image.extend(der::write(SEQUENCE, &payload));
    image.extend(der::write(EXPLICIT_0, im4m));
    Ok(der::write(SEQUENCE, &image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    /// A minimal IM4M manifest: a sequence beginning with the string `IM4M`.
    fn im4m() -> Vec<u8> {
        der::write(SEQUENCE, &der::write(IA5_STRING, b"IM4M"))
    }

    /// An IM4P container, optionally with a keybag so it reads as encrypted.
    fn im4p(kind: &[u8], payload: &[u8], keybag: bool) -> Vec<u8> {
        let mut body = der::write(IA5_STRING, b"IM4P");
        body.extend(der::write(IA5_STRING, kind));
        body.extend(der::write(IA5_STRING, b"description"));
        body.extend(der::write(OCTET_STRING, payload));
        if keybag {
            // Any non-empty sequence stands in for the real keybags.
            body.extend(der::write(OCTET_STRING, &der::write(SEQUENCE, &[1, 2, 3])));
        }
        der::write(SEQUENCE, &body)
    }

    fn key() -> Key {
        Key {
            key: hex("000102030405060708090a0b0c0d0e0f").unwrap(),
            iv: [0; 16],
        }
    }

    #[test]
    fn reports_whether_a_payload_is_encrypted() {
        assert!(!encrypted(&im4p(b"krnl", b"plain", false)).unwrap());
        assert!(encrypted(&im4p(b"krnl", &[0; 16], true)).unwrap());
    }

    #[test]
    fn decodes_plaintext_and_encrypted_payloads() {
        let plain = b"not compressed";
        assert_eq!(
            decode(&im4p(b"krnl", plain, false), None, 1 << 20).unwrap(),
            plain
        );

        let payload: Vec<u8> = (0..16).collect();
        let mut encrypted_payload = payload.clone();
        key().encrypt(&mut encrypted_payload).unwrap();
        let image = im4p(b"krnl", &encrypted_payload, true);
        assert_eq!(
            decode(&image, None, 1 << 20).unwrap_err(),
            Error("encrypted IM4P needs a key")
        );
        assert_eq!(decode(&image, Some(&key()), 1 << 20).unwrap(), payload);
    }

    #[test]
    fn decode_honours_the_limit() {
        let image = im4p(b"krnl", &[0; 100], false);
        assert_eq!(
            decode(&image, None, 50).unwrap_err(),
            Error("IM4P payload exceeds limit")
        );
    }

    #[test]
    fn manifest_accepts_only_im4m() {
        assert!(manifest(&im4m()).is_ok());
        assert!(manifest(&der::write(SEQUENCE, &der::write(IA5_STRING, b"IM4P"))).is_err());
        assert!(manifest(b"junk").is_err());
    }

    #[test]
    fn wrap_builds_a_decodable_image_and_checks_the_manifest() {
        let data = b"patched iBSS";
        let image = wrap(b"ibss", data, &im4m()).unwrap();
        let outer = der::read(&image).unwrap();
        let fields = der::items(outer.body).unwrap();
        assert_eq!(fields[0].ia5().unwrap(), "IMG4");
        // The second field is the IM4P; its payload is what we put in.
        assert_eq!(decode(fields[1].encoded, None, 1 << 20).unwrap(), data);
        // A bad manifest is refused before anything is built.
        assert!(wrap(b"ibss", data, b"junk").is_err());
    }
}

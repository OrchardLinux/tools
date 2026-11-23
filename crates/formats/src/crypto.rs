//! AES-CBC for Apple's encrypted firmware payloads. IMG3 and IM4P both store
//! whole blocks, so nothing here pads.
use crate::{Error, Result};
use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};

/// The key and IV of one firmware file. The key is 16, 24 or 32 bytes and
/// selects AES-128, AES-192 or AES-256.
pub struct Key {
    pub key: Vec<u8>,
    pub iv: [u8; 16],
}

impl Key {
    /// Decrypts `bytes` in place. Their length must be a multiple of 16.
    pub fn decrypt(&self, bytes: &mut [u8]) -> Result<()> {
        macro_rules! decrypt {
            ($aes:ty) => {
                cbc::Decryptor::<$aes>::new_from_slices(&self.key, &self.iv)
                    .map_err(|_| Error("invalid AES key length"))?
                    .decrypt_padded_mut::<NoPadding>(bytes)
                    .map_err(|_| Error("AES ciphertext is not block aligned"))?
            };
        }
        match self.key.len() {
            16 => {
                decrypt!(aes::Aes128);
            }
            24 => {
                decrypt!(aes::Aes192);
            }
            32 => {
                decrypt!(aes::Aes256);
            }
            _ => return Err(Error("invalid AES key length")),
        }
        Ok(())
    }

    /// Encrypts `bytes` in place. Their length must be a multiple of 16.
    pub fn encrypt(&self, bytes: &mut [u8]) -> Result<()> {
        let size = bytes.len();
        macro_rules! encrypt {
            ($aes:ty) => {
                cbc::Encryptor::<$aes>::new_from_slices(&self.key, &self.iv)
                    .map_err(|_| Error("invalid AES key length"))?
                    .encrypt_padded_mut::<NoPadding>(bytes, size)
                    .map_err(|_| Error("AES plaintext is not block aligned"))?
            };
        }
        match self.key.len() {
            16 => {
                encrypt!(aes::Aes128);
            }
            24 => {
                encrypt!(aes::Aes192);
            }
            32 => {
                encrypt!(aes::Aes256);
            }
            _ => return Err(Error("invalid AES key length")),
        }
        Ok(())
    }
}

/// The bytes a string of hexadecimal digit pairs stands for. Keys, IVs and
/// byte strings in board data are written this way.
pub fn hex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error("invalid hexadecimal bytes"));
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16)
                .map_err(|_| Error("invalid hexadecimal bytes"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(hex_key: &str) -> Key {
        Key {
            key: hex(hex_key).unwrap(),
            iv: hex("000102030405060708090a0b0c0d0e0f")
                .unwrap()
                .try_into()
                .unwrap(),
        }
    }

    /// The first CBC block of NIST SP 800-38A, for AES-128 and AES-256.
    #[test]
    fn matches_the_nist_vectors() {
        let plain = hex("6bc1bee22e409f96e93d7e117393172a").unwrap();
        for (k, cipher) in [
            (
                "2b7e151628aed2a6abf7158809cf4f3c",
                "7649abac8119b246cee98e9b12e9197d",
            ),
            (
                "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4",
                "f58c4c04d6e5f1ba779eabfb5f7bfbd6",
            ),
        ] {
            let key = key(k);
            let mut bytes = plain.clone();
            key.encrypt(&mut bytes).unwrap();
            assert_eq!(bytes, hex(cipher).unwrap());
            key.decrypt(&mut bytes).unwrap();
            assert_eq!(bytes, plain);
        }
    }

    #[test]
    fn chains_blocks_in_both_directions() {
        let key = key("000102030405060708090a0b0c0d0e0f1011121314151617");
        let plain = vec![7; 64];
        let mut bytes = plain.clone();
        key.encrypt(&mut bytes).unwrap();
        assert_ne!(bytes[..16], bytes[16..32]);
        key.decrypt(&mut bytes).unwrap();
        assert_eq!(bytes, plain);
    }

    #[test]
    fn refuses_partial_blocks_and_odd_keys() {
        let good = key("2b7e151628aed2a6abf7158809cf4f3c");
        assert!(good.decrypt(&mut [0; 15]).is_err());
        assert!(good.encrypt(&mut [0; 17]).is_err());
        let odd = key("2b7e151628aed2a6abf7158809cf4f");
        assert_eq!(
            odd.decrypt(&mut [0; 16]),
            Err(Error("invalid AES key length"))
        );
        assert_eq!(
            odd.encrypt(&mut [0; 16]),
            Err(Error("invalid AES key length"))
        );
    }

    #[test]
    fn reads_hexadecimal() {
        assert_eq!(hex("00ffA5").unwrap(), [0x00, 0xff, 0xa5]);
        assert_eq!(hex("").unwrap(), Vec::<u8>::new());
        assert!(hex("abc").is_err());
        assert!(hex("0x12").is_err());
        assert!(hex("+1").is_err());
    }
}

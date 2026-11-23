//! Broadcom TRX firmware images, and adding NVRAM settings to one, after
//! Project Cascadia's trx-add-nvram.py.
use crate::{Error, Result};

const HEADER: usize = 28;

/// Appends NVRAM settings (`key=value` lines, `#` comments) to a TRX that has
/// an empty NVRAM slot, with `macaddr` set to `placeholder_mac`.
pub fn add_nvram(firmware: &[u8], nvram: &[u8], placeholder_mac: &str) -> Result<Vec<u8>> {
    if firmware.len() < HEADER {
        return Err(Error("truncated TRX"));
    }
    let word = |at: usize| u32::from_le_bytes(firmware[at..at + 4].try_into().unwrap()) as usize;
    let (size, crc, code_size, old_nvram) = (word(4), word(8) as u32, word(16), word(24));
    // Apple's header length covers more than firmware and NVRAM, so only the
    // firmware part and the NVRAM slot after it are used.
    if &firmware[..4] != b"HDR0"
        || !(HEADER..=firmware.len()).contains(&size)
        || HEADER + code_size + old_nvram > firmware.len()
    {
        return Err(Error("bad TRX header"));
    }
    if crc32fast::hash(&firmware[12..size]) ^ 0xffff_ffff != crc {
        return Err(Error("TRX checksum mismatch"));
    }
    let slot = HEADER + code_size;
    if firmware[slot..slot + old_nvram].iter().any(|&b| b != 0) {
        return Err(Error("TRX already contains NVRAM"));
    }
    let text = std::str::from_utf8(nvram)
        .ok()
        .filter(|t| t.is_ascii())
        .ok_or(Error("NVRAM is not ASCII"))?;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || line.contains('\0') {
            return Err(Error("bad NVRAM entry"));
        }
        // The recipe carries public settings; the unit's own address is set
        // on the device at boot.
        if key == "macaddr" {
            return Err(Error("recipe NVRAM must not set macaddr"));
        }
        lines.push(format!("{key}={value}"));
    }
    lines.push(format!("macaddr={placeholder_mac}"));
    let mut body = (lines.join("\0") + "\0").into_bytes();
    body.resize((body.len() + 4) & !3, 0);
    let words = u32::try_from(body.len() / 4)
        .ok()
        .filter(|&w| w <= 0xffff)
        .ok_or(Error("NVRAM is too large"))?;
    body.extend(((!words & 0xffff) << 16 | words).to_le_bytes());

    let mut image = firmware[..slot].to_vec();
    image.extend(&body);
    let len = image.len() as u32;
    image[4..8].copy_from_slice(&len.to_le_bytes());
    image[24..28].copy_from_slice(&(body.len() as u32).to_le_bytes());
    let crc = crc32fast::hash(&image[12..]) ^ 0xffff_ffff;
    image[8..12].copy_from_slice(&crc.to_le_bytes());
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trx(code: &[u8], slot: usize) -> Vec<u8> {
        let mut t = vec![0; HEADER];
        t[..4].copy_from_slice(b"HDR0");
        t[16..20].copy_from_slice(&(code.len() as u32).to_le_bytes());
        t[24..28].copy_from_slice(&(slot as u32).to_le_bytes());
        t.extend(code);
        t.extend(vec![0; slot]);
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        let crc = crc32fast::hash(&t[12..]) ^ 0xffff_ffff;
        t[8..12].copy_from_slice(&crc.to_le_bytes());
        t
    }

    #[test]
    fn appends_settings_and_placeholder_address() {
        let out = add_nvram(
            &trx(b"code", 8),
            b"# comment\nboardtype=0x1\n\n aa = b # x\n",
            "02:00:00:00:00:00",
        )
        .unwrap();
        let body = &out[HEADER + 4..out.len() - 4];
        assert!(body.starts_with(b"boardtype=0x1\0aa=b\0macaddr=02:00:00:00:00:00\0"));
        assert_eq!(body.len() % 4, 0);
        let words = (body.len() / 4) as u32;
        assert_eq!(
            out[out.len() - 4..],
            ((!words & 0xffff) << 16 | words).to_le_bytes()
        );
        // The result is a valid TRX again, without room for more NVRAM.
        assert!(add_nvram(&out, b"", "02:00:00:00:00:00").is_err());
    }

    #[test]
    fn refuses_bad_input() {
        let mut damaged = trx(b"code", 8);
        damaged[30] ^= 1;
        assert!(add_nvram(&damaged, b"", "x").is_err());
        assert!(add_nvram(&trx(b"code", 8), b"macaddr=11:22:33:44:55:66", "x").is_err());
    }
}

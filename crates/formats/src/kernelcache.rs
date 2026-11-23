//! Prelinked ARM64 kernelcaches (the `MH_EXECUTE` kind, iOS 15 and older on
//! these devices), and firmware embedded in their kexts. Kexts are found by
//! their prelink info, never by fixed offsets.
use crate::macho::{self, Section, Segment};
use crate::osserialize::{self, Value};
use crate::{im4p, img4, lzfse, lzss, Error, Result};

/// Decompressed kernels larger than this are refused.
const MAX_KERNEL: usize = 256 << 20;

/// The bundle ids `smartio` accepts.
pub const SMARTIO_KEXTS: &[&str] = &[
    "com.apple.driver.AppleT8010SmartIO",
    "com.apple.driver.AppleS8000SmartIO",
];

/// Decodes a kernelcache IM4P to the ARM64 kernel Mach-O.
pub fn decode(im4p_bytes: &[u8]) -> Result<Vec<u8>> {
    let payload = im4p::payload(im4p_bytes)?;
    let kernel = if payload.starts_with(b"bvx") {
        lzfse::decode_limited(payload, MAX_KERNEL)?
    } else if payload.starts_with(b"complzss") {
        lzss::decompress(payload, MAX_KERNEL)?.0
    } else {
        return Err(Error("kernelcache payload is neither LZFSE nor LZSS"));
    };
    Ok(macho::arm64(&kernel)?.to_vec())
}

/// Wraps `kernel` as the restore kernelcache iBoot's `bootx` starts: LZSS
/// compressed, in an IMG4 of type `rkrn` signed with `im4m`. `stub` follows
/// the compressed stream in the same payload; iBoot on the chips that load
/// an iBEC starts the kernel through that monitor.
pub fn restore(kernel: &[u8], stub: Option<&[u8]>, im4m: &[u8]) -> Result<Vec<u8>> {
    let mut payload = lzss::compress(kernel)?;
    payload.extend_from_slice(stub.unwrap_or_default());
    img4::wrap(b"rkrn", &payload, im4m)
}

pub struct Kernelcache<'a> {
    data: &'a [u8],
    segments: Vec<Segment>,
    sections: Vec<Section>,
}

impl<'a> Kernelcache<'a> {
    pub fn new(data: &'a [u8]) -> Result<Kernelcache<'a>> {
        let header = macho::parse(data, 0)?;
        if header.filetype != macho::MH_EXECUTE {
            return Err(Error("kernelcache is not a prelinked MH_EXECUTE kernel"));
        }
        Ok(Kernelcache {
            data,
            segments: header.segments,
            sections: header.sections,
        })
    }

    /// The bytes at virtual address `addr`, through the one file-backed
    /// segment that covers them.
    pub fn read(&self, addr: u64, size: u64) -> Result<&'a [u8]> {
        let mut offsets = self
            .segments
            .iter()
            .filter(|s| {
                s.vmaddr <= addr && addr.saturating_add(size) <= s.vmaddr.saturating_add(s.filesize)
            })
            .map(|s| s.fileoff + (addr - s.vmaddr));
        let (Some(offset), None) = (offsets.next(), offsets.next()) else {
            return Err(Error("address has no single file-backed mapping"));
        };
        self.data
            .get(offset as usize..(offset + size) as usize)
            .ok_or(Error("mapping extends past the kernelcache"))
    }

    /// The sections of the one prelinked kext with bundle id `bundle`.
    pub fn kext(&self, bundle: &str) -> Result<Vec<Section>> {
        let info = self.section("__PRELINK_INFO", "__info", &self.sections)?;
        let info = osserialize::xml(self.read(info.addr, info.size)?)?;
        let kexts = info
            .get("_PrelinkInfoDictionary")
            .and_then(Value::as_array)
            .ok_or(Error("prelink info has no kext list"))?;
        let mut matching = kexts
            .iter()
            .filter(|k| k.get("CFBundleIdentifier").and_then(Value::as_str) == Some(bundle));
        let (Some(kext), None) = (matching.next(), matching.next()) else {
            return Err(Error("kernelcache does not hold exactly one such kext"));
        };
        let load = kext
            .get("_PrelinkExecutableLoadAddr")
            .and_then(Value::as_u64)
            .ok_or(Error("kext has no load address"))?;
        let start = self.read(load, 32)?.as_ptr() as usize - self.data.as_ptr() as usize;
        Ok(macho::parse(self.data, start)?.sections)
    }

    pub fn section<'s>(
        &self,
        segment: &str,
        name: &str,
        sections: &'s [Section],
    ) -> Result<&'s Section> {
        let mut found = sections
            .iter()
            .filter(|s| s.segment == segment && s.name == name);
        match (found.next(), found.next()) {
            (Some(s), None) => Ok(s),
            _ => Err(Error("kernelcache section is missing or repeated")),
        }
    }
}

/// SmartIO firmware: the `__DATA.__firmware` section of a SmartIO kext.
pub fn smartio<'a>(kc: &Kernelcache<'a>, bundle: &str) -> Result<&'a [u8]> {
    if !SMARTIO_KEXTS.contains(&bundle) {
        return Err(Error("unknown SmartIO kext"));
    }
    let sections = kc.kext(bundle)?;
    let firmware = kc.section("__DATA", "__firmware", &sections)?;
    if !(4096..=4 << 20).contains(&firmware.size) {
        return Err(Error("SmartIO firmware has an unlikely size"));
    }
    kc.read(firmware.addr, firmware.size)
}

/// A10 GPU firmware: a build-variant C string followed by an ARM image in
/// AGXFirmwareKextG9PRTBuddy's __TEXT.__const. The recipe pins the image hash.
pub fn agx_g9p<'a>(kc: &Kernelcache<'a>) -> Result<&'a [u8]> {
    let sections = kc.kext("com.apple.AGXFirmwareKextG9PRTBuddy")?;
    let section = kc.section("__TEXT", "__const", &sections)?;
    agx_g9p_image(kc.read(section.addr, section.size)?)
}

fn agx_g9p_image(constant: &[u8]) -> Result<&[u8]> {
    let start = constant
        .iter()
        .position(|b| *b != 0)
        .ok_or(Error("no build variant before the GPU firmware"))?;
    let variant = &constant[start..];
    let end = variant
        .iter()
        .position(|b| *b == 0)
        .ok_or(Error("unterminated GPU firmware build variant"))?;
    if end == 0 || !variant[..end].iter().all(|b| (0x20..=0x7e).contains(b)) {
        return Err(Error("invalid GPU firmware build variant"));
    }
    let image = &variant[end + 1..];
    if image.len() < 4096 || !image.len().is_multiple_of(4) || image[3] != 0xea {
        return Err(Error(
            "GPU firmware does not start with an ARM reset vector",
        ));
    }
    Ok(image)
}

/// VXD (DEVA) video decoder firmware from AppleD5500. Its `__DATA.__data`
/// holds a record {name, build date, decoder memory size, image size, image}
/// whose pointers lead into `__TEXT.__cstring` and `__TEXT.__const`; the
/// record is found by that pattern and a name ending in `_Deva`.
pub fn vxd<'a>(kc: &Kernelcache<'a>) -> Result<&'a [u8]> {
    let sections = kc.kext("com.apple.driver.AppleD5500")?;
    let data = kc.section("__DATA", "__data", &sections)?;
    let cstring = kc.section("__TEXT", "__cstring", &sections)?;
    let constant = kc.section("__TEXT", "__const", &sections)?;
    let inside =
        |s: &Section, addr: u64, size: u64| s.addr <= addr && addr + size <= s.addr + s.size;
    let text = |addr: u64| -> Option<&str> {
        let raw = kc
            .read(addr, (cstring.addr + cstring.size - addr).min(64))
            .ok()?;
        let s = std::str::from_utf8(raw.split(|&b| b == 0).next()?).ok()?;
        s.chars().all(|c| !c.is_control()).then_some(s)
    };
    let bytes = kc.read(data.addr, data.size)?;
    let quad = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let word = |at: usize| u64::from(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
    let mut records = (0..bytes.len().saturating_sub(31))
        .step_by(8)
        .filter_map(|at| {
            let (name, build, memory, size, image) = (
                quad(at),
                quad(at + 8),
                word(at + 16),
                word(at + 20),
                quad(at + 24),
            );
            let plausible = inside(cstring, name, 1)
                && inside(cstring, build, 1)
                && (4097..=4 << 20).contains(&size)
                && memory <= size
                && inside(constant, image, size)
                && text(name).is_some_and(|n| n.ends_with("_Deva"));
            plausible.then_some((image, size))
        });
    match (records.next(), records.next()) {
        (Some((image, size)), None) => kc.read(image, size),
        _ => Err(Error(
            "AppleD5500 does not hold exactly one VXD firmware record",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::{self, IA5_STRING, SEQUENCE};

    #[test]
    fn gpu_firmware_follows_the_build_variant_and_starts_with_a_branch() {
        let mut constant = b"\0\0Release\0".to_vec();
        let start = constant.len();
        constant.extend(vec![0; 4096]);
        constant[start..start + 4].copy_from_slice(&[0x0e, 0, 0, 0xea]);
        assert_eq!(agx_g9p_image(&constant).unwrap(), &constant[start..]);
        assert!(agx_g9p_image(&constant[..constant.len() - 1]).is_err());
        constant[start + 3] = 0;
        assert!(agx_g9p_image(&constant).is_err());
        for malformed in [b"".as_slice(), b"\0\0", b"Release", b"\x01\0", b"Release\0"] {
            assert!(agx_g9p_image(malformed).is_err());
        }
    }

    #[test]
    fn restore_kernelcache_is_lzss_with_the_stub_behind_it() {
        let im4m = der::write(SEQUENCE, &der::write(IA5_STRING, b"IM4M"));
        let kernel: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
        for stub in [None, Some(&b"monitor stub"[..])] {
            let image = restore(&kernel, stub, &im4m).unwrap();
            let fields = der::items(der::read(&image).unwrap().body).unwrap();
            assert_eq!(fields[0].ia5().unwrap(), "IMG4");
            let im4p = der::items(fields[1].body).unwrap();
            assert_eq!(im4p[1].ia5().unwrap(), "rkrn");
            let (plain, rest) =
                lzss::decompress(im4p::payload(fields[1].encoded).unwrap(), 1 << 20).unwrap();
            assert_eq!(plain, kernel);
            assert_eq!(rest, stub.unwrap_or_default());
        }
        assert!(restore(&kernel, None, b"junk").is_err());
    }
}

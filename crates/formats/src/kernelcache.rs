//! ARM64 kernelcaches, and firmware embedded in their kexts. Two kinds: the
//! prelinked `MH_EXECUTE` kernel (iOS 15 and older on these devices), whose
//! kexts are found by their prelink info, and the `MH_FILESET` (iOS 16 and
//! later), whose kexts are fileset entries named by bundle id. Never by
//! fixed offsets.
use crate::macho::{self, Section, Segment};
use crate::osserialize::{self, Value};
use crate::{im4p, img4, lzfse, lzss, Error, Result};

/// Decompressed kernels larger than this are refused.
const MAX_KERNEL: usize = 256 << 20;

/// The bundle ids `smartio` accepts: A10, A9, A8 and A8X, A7, A10X. The
/// first is what `smartio-from-kernelcache@1` means.
pub const SMARTIO_KEXTS: &[&str] = &[
    "com.apple.driver.AppleT8010SmartIO",
    "com.apple.driver.AppleS8000SmartIO",
    "com.apple.driver.AppleT7000SmartIO",
    "com.apple.driver.AppleS5L8960XSmartIO",
    "com.apple.driver.AppleT8011SmartIO",
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
    /// The fileset entries; empty for a prelinked kernel.
    entries: Vec<macho::Entry>,
}

impl<'a> Kernelcache<'a> {
    pub fn new(data: &'a [u8]) -> Result<Kernelcache<'a>> {
        let header = macho::parse(data, 0)?;
        if header.filetype != macho::MH_EXECUTE && header.filetype != macho::MH_FILESET {
            return Err(Error(
                "kernelcache is neither a prelinked kernel nor a fileset",
            ));
        }
        if header.filetype == macho::MH_FILESET && header.entries.is_empty() {
            return Err(Error("fileset kernelcache has no entries"));
        }
        Ok(Kernelcache {
            data,
            segments: header.segments,
            sections: header.sections,
            entries: header.entries,
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

    /// The sections of the one kext with bundle id `bundle`.
    pub fn kext(&self, bundle: &str) -> Result<Vec<Section>> {
        self.kext_or_merged(bundle)?
            .ok_or(Error("kext is merged into the kernel and has no sections of its own"))
    }

    /// The same, or `None` for a kext whose segments are merged into the
    /// kernel's own: some chips' iOS 12 kernelcaches give a kext a
    /// `ModuleIndex` and no load address, and it has no Mach-O of its own.
    pub fn kext_or_merged(&self, bundle: &str) -> Result<Option<Vec<Section>>> {
        if !self.entries.is_empty() {
            return self.fileset_kext(bundle).map(Some);
        }
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
        if kext.get("_PrelinkExecutableLoadAddr").is_none() && kext.get("ModuleIndex").is_some() {
            return Ok(None);
        }
        let load = kext
            .get("_PrelinkExecutableLoadAddr")
            .and_then(Value::as_u64)
            .ok_or(Error("kext has no load address"))?;
        let start = self.read(load, 32)?.as_ptr() as usize - self.data.as_ptr() as usize;
        Ok(Some(macho::parse(self.data, start)?.sections))
    }

    /// The sections of the fileset entry with id `bundle`, from the Mach-O
    /// header at the entry's file offset. Their addresses are in the
    /// kernelcache's own address space, so `read` serves them.
    fn fileset_kext(&self, bundle: &str) -> Result<Vec<Section>> {
        let mut matching = self.entries.iter().filter(|e| e.id == bundle);
        let (Some(entry), None) = (matching.next(), matching.next()) else {
            return Err(Error("kernelcache does not hold exactly one such kext"));
        };
        let start = usize::try_from(entry.fileoff)
            .map_err(|_| Error("fileset entry lies past the kernelcache"))?;
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

/// The bytes of section `segment`.`section` of the kext with bundle id
/// `bundle`: firmware a kext carries as one whole section. The recipe pins
/// the hash.
pub fn kext_section<'a>(
    kc: &Kernelcache<'a>,
    bundle: &str,
    segment: &str,
    section: &str,
) -> Result<&'a [u8]> {
    let sections = kc.kext(bundle)?;
    let found = kc.section(segment, section, &sections)?;
    if found.size == 0 {
        return Err(Error("kext section is empty"));
    }
    kc.read(found.addr, found.size)
}

/// SmartIO firmware: the `__DATA.__firmware` section of a SmartIO kext.
pub fn smartio<'a>(kc: &Kernelcache<'a>, bundle: &str) -> Result<&'a [u8]> {
    if !SMARTIO_KEXTS.contains(&bundle) {
        return Err(Error("unknown SmartIO kext"));
    }
    // A kext merged into the kernel keeps its firmware in the kernel's one
    // `__DATA.__firmware` section.
    let own = kc.kext_or_merged(bundle)?;
    let sections = own.as_deref().unwrap_or(&kc.sections);
    let firmware = kc.section("__DATA", "__firmware", sections)?;
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

/// The size of the A9 GPU firmware this version knows: the C0 image of
/// iPadOS 16.7.10. The driver's firmware layout belongs to that one image.
pub const AGX_G5P_SIZE: u64 = 119_112;
/// Its SHA-256.
pub const AGX_G5P_SHA256: &str = "33ae20cbbe2498e4f119ba4e9a55467e4e0192d4c86d5bf1903adce63ffa5dca";

/// A9 GPU firmware: all of AGXFirmwareKextG5P's `__TEXT.__const`, a META LDR
/// the driver loads into the GPU's own processor. The LDR has no header to
/// check here, so the caller checks [`AGX_G5P_SHA256`].
pub fn agx_g5p<'a>(kc: &Kernelcache<'a>) -> Result<&'a [u8]> {
    let sections = kc.kext("com.apple.AGXFirmwareKextG5P")?;
    let section = kc.section("__TEXT", "__const", &sections)?;
    if section.size != AGX_G5P_SIZE {
        return Err(Error("unsupported G5P C0 firmware size"));
    }
    kc.read(section.addr, section.size)
}

/// The first word of a META LDR.
const LDR_MAGIC: u32 = 0x01AA_5500;
/// The A8 GPU's border colour table: its size, and its first nonzero word
/// with that word's offset.
const G4P_BORDER_SIZE: usize = 0x800;
const G4P_BORDER_MARK: (usize, u32) = (0x440, 0x0204_0000);

/// A8 GPU firmware candidates: every META LDR in the decoded kernel whose
/// records walk cleanly (AGXFirmwareKextG4P holds one). The iOS 12
/// kernelcaches have no per-kext load addresses in their prelink info, so
/// the image is found by its structure; the recipe's hash picks it.
pub fn agx_g4p_ldrs(kernel: &[u8]) -> Vec<&[u8]> {
    let magic = LDR_MAGIC.to_le_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = find(&kernel[at..], &magic) {
        let start = at + offset;
        if let Ok(length) = ldr_length(&kernel[start..]) {
            found.push(&kernel[start..(start + length).min(kernel.len())]);
        }
        at = start + magic.len();
    }
    found
}

/// A7 GPU firmware candidates: AGXFirmwareKextG3 holds a META LDR of the
/// A8's kind ("METAFW_G3", HAL G3_B0), found the same way as
/// [`agx_g4p_ldrs`]: the iOS 12.5.8 kernelcaches of the A7 boards hold
/// exactly one LDR whose records walk cleanly; the recipe's hash pins it.
pub fn agx_g3_ldrs(kernel: &[u8]) -> Vec<&[u8]> {
    agx_g4p_ldrs(kernel)
}

/// The A8 GPU's border colour table candidates: 0x800 bytes of AGXG4P's
/// data that open with 0x440 zero bytes and then the word 0x02040000.
pub fn agx_g4p_borders(kernel: &[u8]) -> Vec<&[u8]> {
    let (offset, word) = G4P_BORDER_MARK;
    let needle = word.to_le_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(hit) = find(&kernel[at..], &needle) {
        let mark = at + hit;
        if let Some(start) = mark.checked_sub(offset) {
            if start % 4 == 0
                && start + G4P_BORDER_SIZE <= kernel.len()
                && kernel[start..mark].iter().all(|b| *b == 0)
            {
                found.push(&kernel[start..start + G4P_BORDER_SIZE]);
            }
        }
        at = mark + 1;
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The length of the META LDR at the start of `blob`: the end of its last
/// L1 record or L2 block, at least its header and code offset.
fn ldr_length(blob: &[u8]) -> Result<usize> {
    let u16_at = |at: usize| -> Option<usize> {
        Some(u16::from_le_bytes(blob.get(at..at + 2)?.try_into().ok()?) as usize)
    };
    let u32_at = |at: usize| -> Option<usize> {
        Some(u32::from_le_bytes(blob.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    if blob.len() < 16 {
        return Err(Error("truncated LDR header"));
    }
    if u32_at(0) != Some(LDR_MAGIC as usize) {
        return Err(Error("not a META LDR"));
    }
    let (code, mut pos) = (u32_at(4).unwrap(), u32_at(8).unwrap());
    let mut end = code.max(16);
    let mut seen = std::collections::HashSet::new();
    while pos != 0xFFFF_FFFF {
        if !seen.insert(pos) || pos + 8 > blob.len() {
            return Err(Error("bad LDR record list"));
        }
        let (command, size, following) = (
            u16_at(pos).unwrap(),
            u16_at(pos + 2).unwrap(),
            u32_at(pos + 4).unwrap(),
        );
        if size < 12 || size % 4 != 0 || pos + size > blob.len() {
            return Err(Error("bad LDR L1 record"));
        }
        let words = (size - 12) / 4;
        let index = if command & 0x10 != 0 || command == 3 || command == 5 {
            0
        } else if command == 0 {
            1
        } else if command == 4 {
            2
        } else {
            return Err(Error("unknown LDR L1 command"));
        };
        if words <= index {
            return Err(Error("bad LDR L1 record"));
        }
        let block = u32_at(pos + 8 + 4 * index).unwrap();
        if block + 4 > blob.len() {
            return Err(Error("bad LDR L2 offset"));
        }
        let block_size = u16_at(block + 2).unwrap();
        if block_size < 6 || block + block_size > blob.len() {
            return Err(Error("bad LDR L2 block"));
        }
        end = end.max(pos + size).max(block + block_size);
        pos = following;
    }
    Ok(end)
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
        // Printable ASCII, as the firmware's name is.
        let s = std::str::from_utf8(raw.split(|&b| b == 0).next()?).ok()?;
        s.bytes().all(|b| (0x20..=0x7e).contains(&b)).then_some(s)
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

    /// Where the made-up kernelcaches below are mapped, and their size.
    const BASE: u64 = 0xffff_fff0_0700_4000;
    const SIZE: usize = 0x4000;
    /// The two kexts each holds, and their Mach-Os' file offsets.
    const KEXTS: [(&str, usize); 2] = [("com.example.one", 0x1000), ("com.example.two", 0x2000)];

    /// A 64-bit ARM64 Mach-O header with these load commands.
    fn header(filetype: u32, commands: &[Vec<u8>]) -> Vec<u8> {
        let total: usize = commands.iter().map(Vec::len).sum();
        let mut v = Vec::new();
        for word in [0xfeed_facf, 0x0100_000c, 0, filetype, commands.len() as u32] {
            v.extend(u32::to_le_bytes(word));
        }
        for word in [total as u32, 0, 0] {
            v.extend(word.to_le_bytes());
        }
        commands.iter().for_each(|c| v.extend(c));
        v
    }

    fn fixed(name: &str) -> [u8; 16] {
        let mut raw = [0; 16];
        raw[..name.len()].copy_from_slice(name.as_bytes());
        raw
    }

    /// An LC_SEGMENT_64 mapping `size` file bytes from `fileoff` at
    /// `vmaddr`, with sections given as (name, address, size).
    fn segment(
        name: &str,
        vmaddr: u64,
        fileoff: usize,
        size: usize,
        sections: &[(&str, u64, u64)],
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(0x19u32.to_le_bytes());
        v.extend(((72 + 80 * sections.len()) as u32).to_le_bytes());
        v.extend(fixed(name));
        for quad in [vmaddr, size as u64, fileoff as u64, size as u64] {
            v.extend(quad.to_le_bytes());
        }
        for word in [3u32, 3, sections.len() as u32, 0] {
            v.extend(word.to_le_bytes());
        }
        for (section, addr, len) in sections {
            v.extend(fixed(section));
            v.extend(fixed(name));
            v.extend(addr.to_le_bytes());
            v.extend(len.to_le_bytes());
            v.extend([0; 32]);
        }
        v
    }

    /// An LC_FILESET_ENTRY whose id string follows the fixed fields.
    fn entry(id: &str, vmaddr: u64, fileoff: usize) -> Vec<u8> {
        let mut name = id.as_bytes().to_vec();
        name.resize((id.len() + 8) & !7, 0);
        let mut v = Vec::new();
        v.extend(0x8000_0035u32.to_le_bytes());
        v.extend(((32 + name.len()) as u32).to_le_bytes());
        v.extend(vmaddr.to_le_bytes());
        v.extend((fileoff as u64).to_le_bytes());
        v.extend([32, 0, 0, 0, 0, 0, 0, 0]);
        v.extend(name);
        v
    }

    fn put(file: &mut [u8], at: usize, bytes: &[u8]) {
        file[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Writes each kext's own Mach-O at its offset: a `__DATA` segment with
    /// a 0x40-byte `fwFirmwareImage` of its own bytes, an empty section and
    /// one beyond the kernelcache's mapping.
    fn kexts(file: &mut [u8]) {
        for (i, (_, at)) in KEXTS.iter().enumerate() {
            let data = BASE + *at as u64 + 0x100;
            let sections = [
                ("fwFirmwareImage", data, 0x40),
                ("__empty", data + 0x40, 0),
                ("__outside", BASE + 0x8000, 0x10),
            ];
            let commands = [
                segment("__TEXT", BASE + *at as u64, *at, 0x100, &[]),
                segment("__DATA", data, at + 0x100, 0x100, &sections),
            ];
            put(file, *at, &header(0xb, &commands));
            let image: Vec<u8> = (0..0x40).map(|b| (b * (i + 3)) as u8).collect();
            put(file, at + 0x100, &image);
        }
    }

    /// A fileset kernelcache with these entries.
    fn fileset(entries: &[(&str, usize)]) -> Vec<u8> {
        let mut file = vec![0; SIZE];
        let mut commands = vec![segment("__TEXT", BASE, 0, SIZE, &[])];
        commands.extend(
            entries
                .iter()
                .map(|(id, at)| entry(id, BASE + *at as u64, *at)),
        );
        put(&mut file, 0, &header(macho::MH_FILESET, &commands));
        kexts(&mut file);
        file
    }

    /// A prelinked kernelcache whose prelink info lists both kexts.
    fn prelinked() -> Vec<u8> {
        let listed: String = KEXTS
            .iter()
            .map(|(id, at)| {
                format!(
                    "<dict><key>CFBundleIdentifier</key><string>{id}</string>\
                     <key>_PrelinkExecutableLoadAddr</key><integer size=\"64\">{:#x}</integer></dict>",
                    BASE + *at as u64
                )
            })
            .collect();
        let info =
            format!("<dict><key>_PrelinkInfoDictionary</key><array>{listed}</array></dict>\0");
        let mut file = vec![0; SIZE];
        let commands = [
            segment("__TEXT", BASE, 0, 0x1000, &[]),
            segment(
                "__PRELINK_INFO",
                BASE + 0x3000,
                0x3000,
                0x1000,
                &[("__info", BASE + 0x3000, info.len() as u64)],
            ),
            segment("__PRELINK_TEXT", BASE + 0x1000, 0x1000, 0x2000, &[]),
        ];
        put(&mut file, 0, &header(macho::MH_EXECUTE, &commands));
        put(&mut file, 0x3000, info.as_bytes());
        kexts(&mut file);
        file
    }

    #[test]
    fn a_kext_section_by_bundle_id_in_both_kinds_of_kernelcache() {
        for file in [fileset(&KEXTS), prelinked()] {
            let kc = Kernelcache::new(&file).unwrap();
            for (i, (bundle, at)) in KEXTS.iter().enumerate() {
                let image = kext_section(&kc, bundle, "__DATA", "fwFirmwareImage").unwrap();
                assert_eq!(image, &file[at + 0x100..at + 0x140]);
                assert_eq!(image[1], (i + 3) as u8);
                // An empty section, one outside the mapping, and names the
                // kext does not have.
                assert!(kext_section(&kc, bundle, "__DATA", "__empty").is_err());
                assert!(kext_section(&kc, bundle, "__DATA", "__outside").is_err());
                assert!(kext_section(&kc, bundle, "__TEXT", "fwFirmwareImage").is_err());
                assert!(kext_section(&kc, bundle, "__DATA", "other").is_err());
            }
            assert!(kext_section(&kc, "com.example.three", "__DATA", "fwFirmwareImage").is_err());
        }
    }

    #[test]
    fn fileset_entries_must_be_unique_and_inside_the_file() {
        let (one, two) = (KEXTS[0], KEXTS[1]);
        let twice = fileset(&[one, two, one]);
        let kc = Kernelcache::new(&twice).unwrap();
        assert!(kc.kext(one.0).is_err());
        assert!(kc.kext(two.0).is_ok());
        let past = fileset(&[one, ("com.example.two", SIZE + 0x1000)]);
        assert!(Kernelcache::new(&past).unwrap().kext(two.0).is_err());
        // A fileset without entries, and an entry whose id lies outside its
        // command.
        assert!(Kernelcache::new(&fileset(&[])).is_err());
        let mut bad = fileset(&[one]);
        let id = 32 + 72 + 24;
        put(&mut bad, id, &200u32.to_le_bytes());
        assert!(Kernelcache::new(&bad).is_err());
        // Neither kind: a kext's own Mach-O.
        assert!(Kernelcache::new(&twice[one.1..]).is_err());
    }

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

    /// A META LDR: header, one L1 record (command 0) and its L2 block.
    fn ldr(fill: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        for word in [LDR_MAGIC, 16, 16] {
            v.extend(word.to_le_bytes());
        }
        v.extend([0; 4]);
        // 20 bytes: command 0, the next record (none), and the L2 block at 36.
        v.extend([0, 0, 20, 0]);
        for word in [0xFFFF_FFFF, 0, 36, 0] {
            v.extend(u32::to_le_bytes(word));
        }
        v.extend([0x00, 0x80]);
        v.extend(((4 + fill.len()) as u16).to_le_bytes());
        v.extend(fill);
        v
    }

    fn border(seed: usize) -> Vec<u8> {
        let (offset, word) = G4P_BORDER_MARK;
        let mut v = vec![0; offset];
        v.extend(word.to_le_bytes());
        v.extend((0..G4P_BORDER_SIZE - offset - 4).map(|i| ((seed + i) % 251 + 1) as u8));
        v
    }

    #[test]
    fn every_ldr_whose_records_walk_is_a_candidate() {
        let (one, two) = (ldr(b"firmware one"), ldr(b"firmware two, longer"));
        let mut kernel = vec![0xff; 7];
        kernel.extend(&one);
        kernel.extend([0; 9]);
        kernel.extend(&two);
        kernel.extend(b"tail");
        assert_eq!(agx_g4p_ldrs(&kernel), vec![&one[..], &two[..]]);

        let mut unknown = one.clone();
        unknown[16] = 7; // an unknown L1 command
        let mut looped = one.clone();
        looped[20..24].copy_from_slice(&16u32.to_le_bytes()); // the record points at itself
        let mut outside = one.clone();
        outside[28..32].copy_from_slice(&1000u32.to_le_bytes()); // its L2 block lies beyond the end
        for broken in [unknown, looped, outside, one[..one.len() - 1].to_vec()] {
            assert!(agx_g4p_ldrs(&broken).is_empty());
        }
    }

    #[test]
    fn the_a7_ldr_is_found_as_the_a8_ldr_is() {
        let one = ldr(b"g3 firmware");
        let mut kernel = vec![0xff; 5];
        kernel.extend(&one);
        assert_eq!(agx_g3_ldrs(&kernel), vec![&one[..]]);
        assert_eq!(agx_g3_ldrs(&kernel), agx_g4p_ldrs(&kernel));
    }

    #[test]
    fn the_border_table_is_found_by_its_structure() {
        let (table, other) = (border(3), border(9));
        let mut kernel = vec![1; 12];
        kernel.extend(&table);
        kernel.extend([1; 4]);
        kernel.extend(&other);
        assert_eq!(agx_g4p_borders(&kernel), vec![&table[..], &other[..]]);
        // A marker after nonzero bytes, off a word boundary, or too close to
        // the end is not a table.
        let (offset, word) = G4P_BORDER_MARK;
        let mut marked = vec![1; offset];
        marked.extend(word.to_le_bytes());
        marked.extend(vec![0; G4P_BORDER_SIZE]);
        assert!(agx_g4p_borders(&marked).is_empty());
        let mut shifted = vec![0; 2];
        shifted.extend(&table);
        assert!(agx_g4p_borders(&shifted).is_empty());
        assert!(agx_g4p_borders(&table[..G4P_BORDER_SIZE - 1]).is_empty());
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

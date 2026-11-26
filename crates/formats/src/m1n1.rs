//! The image m1n1 starts from: m1n1 itself followed by what it is to boot.
//!
//! m1n1 walks the bytes after its own end and recognises each part by how
//! it starts: a logo, `name=value` variable lines, device trees, a kernel,
//! and an initramfs behind a tag that gives its size.
use crate::{Error, Result};

/// The largest image the boot stages that start m1n1 take.
pub const MAX_IMAGE: usize = 128 << 20;

const LOGO_TAG: &[u8] = b"m1n1_logo_256128";
/// A logo is the tag, then a 256×256 and a 128×128 picture of 4-byte pixels.
pub const LOGO_SIZE: usize = LOGO_TAG.len() + 4 * (256 * 256 + 128 * 128);
/// A splash (orchard's boot contract): `OSPL`, width and height as LE u32,
/// then width × height XRGB8888 pixels. Orchard's m1n1 draws it centred in
/// place of its own logo.
const SPLASH_TAG: &[u8] = b"OSPL";
const INITRAMFS_TAG: &[u8] = b"m1n1_initramfs";

/// Whether `logo` is a well-formed m1n1 logo or an orchard splash.
fn logo_ok(logo: &[u8]) -> bool {
    if logo.starts_with(LOGO_TAG) {
        return logo.len() == LOGO_SIZE;
    }
    if logo.len() < 12 || !logo.starts_with(SPLASH_TAG) {
        return false;
    }
    let dim = |at: usize| u32::from_le_bytes(logo[at..at + 4].try_into().unwrap()) as u64;
    let (w, h) = (dim(4), dim(8));
    w > 0 && h > 0 && w <= 4096 && h <= 4096 && logo.len() as u64 == 12 + w * h * 4
}
const MACHO_ARM64: [u8; 8] = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 1];
/// The 32-bit build (A5 to A6X): `MH_MAGIC`, CPU type ARM.
const MACHO_ARM: [u8; 8] = [0xce, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0];
const LC_SEGMENT: u32 = 1;
const PAGE: usize = 4096;

/// What m1n1 is to boot. Device trees keep their order.
#[derive(Debug, Clone, Copy, Default)]
pub struct Payload<'a> {
    pub logo: Option<&'a [u8]>,
    pub bootargs: Option<&'a str>,
    pub dtbs: &'a [&'a [u8]],
    pub kernel: &'a [u8],
    pub initramfs: Option<&'a [u8]>,
}

/// Whether `m1n1` is the Mach-O build, which iBoot starts, rather than the
/// plain binary.
pub fn is_macho(m1n1: &[u8]) -> bool {
    m1n1.starts_with(&MACHO_ARM64)
}

/// Whether `m1n1` is the 32-bit Mach-O build, which a 32-bit iBEC starts.
pub fn is_macho_arm(m1n1: &[u8]) -> bool {
    m1n1.starts_with(&MACHO_ARM)
}

fn word(bytes: &[u8], at: usize) -> Result<u32> {
    let four = bytes.get(at..at + 4).ok_or(Error("m1n1's Mach-O header is cut short"))?;
    Ok(u32::from_le_bytes(four.try_into().expect("4 bytes")))
}

/// Where the `__LAST` segment command of the 32-bit build is. iBoot loads
/// what the segment covers, so a payload behind m1n1 has to be in it; the
/// build leaves it empty at the end of the file.
fn last_segment(macho: &[u8]) -> Result<usize> {
    let commands = word(macho, 16)? as usize;
    let end = 28usize.checked_add(word(macho, 20)? as usize).filter(|end| *end <= macho.len()).ok_or(Error("m1n1's Mach-O header is cut short"))?;
    let mut at = 28;
    for _ in 0..commands {
        let size = word(macho, at + 4)? as usize;
        if size < 8 || at + size > end {
            return Err(Error("m1n1's Mach-O has an invalid load command"));
        }
        let name = macho.get(at + 8..at + 24).unwrap_or_default();
        if word(macho, at)? == LC_SEGMENT && size >= 56 && name.starts_with(b"__LAST\0") {
            let empty = word(macho, at + 28)? == 0 && word(macho, at + 36)? == 0;
            let at_end = word(macho, at + 32)? as usize == macho.len() && word(macho, at + 24)? as usize % PAGE == 0;
            if !empty || !at_end {
                return Err(Error("m1n1 already carries a payload, or is not as orchard builds it"));
            }
            return Ok(at);
        }
        at += size;
    }
    Err(Error("m1n1 has no __LAST segment for a payload"))
}

/// The 32-bit build with `payload` behind it, its last segment sized to it;
/// eight zero bytes end the payload.
fn behind_arm(macho: &[u8], payload: &[&[u8]]) -> Result<Vec<u8>> {
    let at = last_segment(macho)?;
    let size = payload.iter().map(|p| p.len()).sum::<usize>() + 8;
    if macho.len() + size > MAX_IMAGE {
        return Err(Error("the image would be larger than 128 MiB"));
    }
    let mut image = macho.to_vec();
    for part in payload {
        image.extend_from_slice(part);
    }
    image.extend_from_slice(&[0; 8]);
    image[at + 28..at + 32].copy_from_slice(&(size.next_multiple_of(PAGE) as u32).to_le_bytes());
    image[at + 36..at + 40].copy_from_slice(&(size as u32).to_le_bytes());
    Ok(image)
}

/// What a 32-bit iBEC starts with `bootx`: an IMG3 of type `krnl` holding
/// `macho` in Apple's LZSS container, unencrypted and unsigned. `macho` is
/// the 32-bit build as it is, or as [`pack`] left it.
pub fn kernel_image_arm(macho: &[u8]) -> Result<Vec<u8>> {
    if !is_macho_arm(macho) {
        return Err(Error("not m1n1's 32-bit Mach-O build"));
    }
    // m1n1 alone still gets the end of an (empty) payload behind it.
    let alone;
    let macho = match last_segment(macho) {
        Ok(_) => {
            alone = behind_arm(macho, &[])?;
            &alone[..]
        }
        Err(_) => macho,
    };
    Ok(crate::img3::unsigned(b"krnl", &crate::lzss::compress(macho)?))
}

/// Puts the payload behind `m1n1`. The 64-bit Mach-O build's payload ends
/// with four zero bytes; the plain binary's ends with the image; the 32-bit
/// Mach-O build's is covered by its last segment and takes a zImage.
pub fn pack(m1n1: &[u8], payload: &Payload) -> Result<Vec<u8>> {
    if m1n1.is_empty() {
        return Err(Error("m1n1 is empty"));
    }
    if payload.kernel.is_empty() {
        return Err(Error("the kernel is empty"));
    }
    if payload.dtbs.is_empty() {
        return Err(Error("a kernel needs at least one device tree"));
    }
    for dtb in payload.dtbs {
        check_dtb(dtb)?;
    }
    let bootargs = payload.bootargs.map(variable).transpose()?;
    if let Some(logo) = payload.logo {
        if !logo_ok(logo) {
            return Err(Error(
                "a logo is m1n1_logo_256128 with a 256x256 and a 128x128 picture, or an OSPL splash",
            ));
        }
    }
    let initramfs = match payload.initramfs {
        Some(archive) => Some((tag(archive)?, archive)),
        None => None,
    };

    if is_macho_arm(m1n1) {
        // m1n1 reads a zImage's length from its header: padded, so that
        // what follows stays aligned.
        let padding = [0u8; 4];
        let mut parts: Vec<&[u8]> = Vec::new();
        parts.extend(payload.logo);
        parts.extend(bootargs.as_deref());
        parts.extend(payload.dtbs);
        parts.push(payload.kernel);
        parts.push(&padding[..payload.kernel.len().next_multiple_of(4) - payload.kernel.len()]);
        if let Some((header, archive)) = &initramfs {
            parts.push(header);
            parts.push(archive);
        }
        return behind_arm(m1n1, &parts);
    }
    let mut parts: Vec<&[u8]> = vec![m1n1];
    parts.extend(payload.logo);
    parts.extend(bootargs.as_deref());
    parts.extend(payload.dtbs);
    parts.push(payload.kernel);
    if let Some((header, archive)) = &initramfs {
        parts.push(header);
        parts.push(archive);
    }
    if is_macho(m1n1) {
        parts.push(&[0; 4]);
    }
    let size = parts
        .iter()
        .try_fold(0usize, |sum, part| sum.checked_add(part.len()))
        .filter(|size| *size <= MAX_IMAGE)
        .ok_or(Error("the image would be larger than 128 MiB"))?;
    let mut image = Vec::with_capacity(size);
    for part in parts {
        image.extend_from_slice(part);
    }
    Ok(image)
}

/// The boot arguments as the variable line m1n1 reads them from. One line,
/// so the text cannot bring a second variable with it.
fn variable(bootargs: &str) -> Result<Vec<u8>> {
    let one_line = bootargs.bytes().all(|b| (0x20..0x7f).contains(&b));
    if bootargs.is_empty() || bootargs.len() > 4096 || !one_line {
        return Err(Error(
            "boot arguments are one line of 1 to 4096 printable ASCII characters",
        ));
    }
    Ok(format!("chosen.bootargs={bootargs}\n").into_bytes())
}

/// The tag and size that go before an initramfs. Without them m1n1 would
/// take a plain archive for the end of the payload and unpack a compressed
/// one itself.
fn tag(archive: &[u8]) -> Result<Vec<u8>> {
    if archive.is_empty() {
        return Err(Error("the initramfs is empty"));
    }
    if archive.starts_with(INITRAMFS_TAG) {
        return Err(Error(
            "the initramfs already has m1n1's tag; give the archive itself",
        ));
    }
    let size = u32::try_from(archive.len()).map_err(|_| Error("the initramfs is over 4 GiB"))?;
    Ok([INITRAMFS_TAG, &size.to_le_bytes()].concat())
}

/// A device tree must fill its slot exactly: m1n1 finds the next part by
/// the size in the tree's header.
fn check_dtb(dtb: &[u8]) -> Result<()> {
    if dtb.len() < 40 || dtb[..4] != [0xd0, 0x0d, 0xfe, 0xed] {
        return Err(Error("a device tree is not a flattened device tree"));
    }
    let size = u32::from_be_bytes(dtb[4..8].try_into().expect("4 bytes")) as usize;
    if size != dtb.len() {
        return Err(Error(
            "a device tree's header gives another size than the file has",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-bit Mach-O with one empty `__LAST` segment at its end, as
    /// orchard builds m1n1 for the 32-bit chips.
    fn macho_arm() -> Vec<u8> {
        let mut m = MACHO_ARM.to_vec();
        m.extend([0u8; 8]);
        m.extend(1u32.to_le_bytes()); // one load command
        m.extend(56u32.to_le_bytes()); // of 56 bytes
        m.extend(0u32.to_le_bytes());
        m.extend(LC_SEGMENT.to_le_bytes());
        m.extend(56u32.to_le_bytes());
        m.extend(b"__LAST\0\0\0\0\0\0\0\0\0\0");
        m.extend(0x8000u32.to_le_bytes()); // vmaddr
        m.extend(0u32.to_le_bytes()); // vmsize
        m.extend(84u32.to_le_bytes()); // fileoff: the end of the file
        m.extend([0u8; 20]);
        assert_eq!(m.len(), 84);
        m
    }

    #[test]
    fn the_32_bit_build_takes_its_payload_in_its_last_segment() {
        let macho = macho_arm();
        let tree = dtb(40);
        let dtbs = [tree.as_slice()];
        let payload = Payload { logo: None, bootargs: Some("a=b"), dtbs: &dtbs, kernel: b"zImag", initramfs: Some(b"cpio") };
        let image = pack(&macho, &payload).unwrap();
        // Variables, tree, kernel padded to 4, tag and size, archive, 8 zeros.
        let size = "chosen.bootargs=a=b\n".len() + 40 + 8 + INITRAMFS_TAG.len() + 4 + 4 + 8;
        assert_eq!(image.len(), macho.len() + size);
        assert_eq!(word(&image, 28 + 28).unwrap() as usize, PAGE);
        assert_eq!(word(&image, 28 + 36).unwrap() as usize, size);
        assert!(image.ends_with(&[b'c', b'p', b'i', b'o', 0, 0, 0, 0, 0, 0, 0, 0]));
        // Packed once: a second payload is refused.
        assert!(pack(&image, &payload).is_err());

        // For bootx: an IMG3 of type krnl around the LZSS container.
        let wrapped = kernel_image_arm(&image).unwrap();
        assert_eq!(&wrapped[..4], b"3gmI");
        assert_eq!(&wrapped[16..20], b"lnrk");
        let parsed = crate::img3::Image::parse(&wrapped).unwrap();
        let (plain, _) = crate::lzss::decompress(&parsed.decode(None).unwrap(), 1 << 20).unwrap();
        assert_eq!(plain, image);
        // m1n1 alone gets the end of an empty payload.
        let alone = kernel_image_arm(&macho).unwrap();
        let (plain, _) = crate::lzss::decompress(&crate::img3::Image::parse(&alone).unwrap().decode(None).unwrap(), 1 << 20).unwrap();
        assert_eq!(plain.len(), macho.len() + 8);
        assert!(kernel_image_arm(&MACHO_ARM64).is_err());
    }

    /// The smallest header `check_dtb` accepts, padded to `size` bytes.
    fn dtb(size: u32) -> Vec<u8> {
        let mut tree = vec![0xd0, 0x0d, 0xfe, 0xed];
        tree.extend_from_slice(&size.to_be_bytes());
        tree.resize(size as usize, 0);
        tree
    }

    #[test]
    fn parts_in_order() {
        let tree = dtb(40);
        let payload = Payload {
            bootargs: Some("quiet"),
            dtbs: &[&tree],
            kernel: b"KERNEL",
            initramfs: Some(b"ARCHIVE"),
            ..Payload::default()
        };
        let expected = [
            b"M1N1".as_slice(),
            b"chosen.bootargs=quiet\n",
            &tree,
            b"KERNEL",
            b"m1n1_initramfs\x07\0\0\0",
            b"ARCHIVE",
        ]
        .concat();
        assert_eq!(pack(b"M1N1", &payload).unwrap(), expected);
    }

    #[test]
    fn kernel_and_tree_alone() {
        let tree = dtb(48);
        let payload = Payload {
            dtbs: &[&tree],
            kernel: b"K",
            ..Payload::default()
        };
        assert_eq!(
            pack(b"M1N1", &payload).unwrap(),
            [b"M1N1".as_slice(), &tree, b"K"].concat()
        );
    }

    #[test]
    fn the_macho_build_gets_the_end_marker() {
        let tree = dtb(40);
        let payload = Payload {
            dtbs: &[&tree],
            kernel: b"K",
            ..Payload::default()
        };
        let macho = [MACHO_ARM64.as_slice(), b"rest"].concat();
        assert!(is_macho(&macho));
        assert!(!is_macho(b"M1N1"));
        let image = pack(&macho, &payload).unwrap();
        assert_eq!(image, [macho.as_slice(), &tree, b"K", &[0; 4]].concat());
    }

    #[test]
    fn what_cannot_be_packed() {
        let tree = dtb(40);
        let good = Payload {
            dtbs: &[&tree],
            kernel: b"K",
            ..Payload::default()
        };
        let fails = |m1n1: &[u8], payload: Payload| pack(m1n1, &payload).is_err();
        assert!(!fails(b"M", good));
        assert!(fails(b"", good));
        assert!(fails(
            b"M",
            Payload {
                kernel: b"",
                ..good
            }
        ));
        assert!(fails(b"M", Payload { dtbs: &[], ..good }));
        // Not a device tree, and one whose header disagrees with its size.
        assert!(fails(
            b"M",
            Payload {
                dtbs: &[b"junk"],
                ..good
            }
        ));
        let mut short = dtb(48);
        short.truncate(44);
        assert!(fails(
            b"M",
            Payload {
                dtbs: &[&short],
                ..good
            }
        ));
        for bootargs in ["", "two\nlines", "caf\u{e9}"] {
            let bootargs = Some(bootargs);
            assert!(fails(b"M", Payload { bootargs, ..good }));
        }
        let tagged = b"m1n1_initramfs\x01\0\0\0x";
        for initramfs in [b"".as_slice(), tagged] {
            let initramfs = Some(initramfs);
            assert!(fails(b"M", Payload { initramfs, ..good }));
        }
        let logo = Some(b"m1n1_logo_256128 too short".as_slice());
        assert!(fails(b"M", Payload { logo, ..good }));
        let mut splash = b"OSPL".to_vec();
        splash.extend(2u32.to_le_bytes());
        splash.extend(1u32.to_le_bytes());
        splash.extend([0u8; 8]);
        assert!(logo_ok(&splash));
        assert!(!logo_ok(&splash[..splash.len() - 1]), "short splash");
    }
}

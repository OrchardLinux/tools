//! The bundle Cascadia's staging loader starts Linux from on the 32-bit
//! boards, as orchard's `src/orchard/image/cascadia.py` assembles it.
//!
//! An autogo iBEC loads one file at the board's load address and jumps to
//! it. The file is the loader in its first 32 KiB, the zImage with the
//! device tree behind it, and the initramfs. One loader serves every board:
//! it is built with only where it is loaded and where the kernel sits, and
//! carries a checked configuration record that is filled in here with the
//! board's addresses and with where the tree and the initramfs are. It
//! copies the initramfs to its address, turns the MMU off and enters the
//! zImage. The tree's `/chosen` `linux,initrd-start` and `-end` are set in
//! place to that copy. The device's serial number, where it is known, goes
//! to the tree's root as `serial-number`, which is what m1n1 copies from
//! Apple's tree on the 64-bit boards. Nothing else in the tree changes.
use crate::fdt::Tree;
use crate::{Error, Result};

/// Where the loader keeps its configuration record, and the record's size:
/// a four-byte magic and sixteen little-endian words.
const CONFIG_AT: usize = 0x7f00;
const CONFIG_SIZE: usize = 68;
/// The loader's part of the bundle; the kernel starts behind it.
const LOADER_SPACE: u32 = 0x8000;
/// `cpsid if`, the loader's first instruction.
const ENTRY: [u8; 4] = [0xc0, 0x00, 0x0c, 0xf1];

/// The words of the configuration record, by their place behind the magic.
const VERSION: usize = 0;
const HEADER_SIZE: usize = 1;
const LOAD_ADDRESS: usize = 2;
const KERNEL_OFFSET: usize = 3;
const DTB_OFFSET: usize = 4;
const DTB_SIZE: usize = 5;
const INITRD_OFFSET: usize = 6;
const INITRD_SIZE: usize = 7;
const INITRD_ADDRESS: usize = 8;
const IMAGE_SIZE: usize = 9;
const WATCHDOG_ADDRESS: usize = 10;
const L2_ADDRESS: usize = 11;
const RAM_START: usize = 12;
const RAM_END: usize = 13;
const INITRD_LIMIT: usize = 14;
const CRC32: usize = 15;

/// A board's addresses for the handoff, from its boot data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// Where the iBEC loads the bundle and enters it.
    pub load_address: u32,
    /// The kernel's place in the bundle.
    pub kernel_offset: u32,
    pub watchdog_address: u32,
    /// The L2 cache controller, or 0 for none.
    pub l2_address: u32,
    pub ram_start: u32,
    pub ram_end: u32,
    /// Where the loader copies the initramfs, and where that space ends.
    pub initrd_address: u32,
    pub initrd_limit: u32,
}

impl Profile {
    /// Fails unless the ranges are in order, apart and aligned as the loader
    /// expects.
    pub fn check(&self) -> Result<()> {
        let (base, target) = (self.load_address, self.initrd_address);
        let ordered = self.ram_start <= base
            && u64::from(base) + u64::from(LOADER_SPACE) < u64::from(target)
            && target < self.initrd_limit
            && self.initrd_limit <= self.ram_end;
        if self.kernel_offset != LOADER_SPACE
            || base != self.ram_start
            || base % 0x800_0000 != 0
            || !ordered
            || target % 4096 != 0
        {
            return Err(Error(
                "handoff memory ranges overlap or have invalid alignment",
            ));
        }
        if !self.watchdog_address.is_multiple_of(4) || !self.l2_address.is_multiple_of(4) {
            return Err(Error("handoff register addresses must be word aligned"));
        }
        Ok(())
    }

    /// The record's words the loader is built with, the same on every
    /// board it serves.
    fn built(&self) -> [(usize, u32); 2] {
        [
            (LOAD_ADDRESS, self.load_address),
            (KERNEL_OFFSET, self.kernel_offset),
        ]
    }

    /// The record's words that are the board's own, filled in when the
    /// bundle is packed.
    fn board(&self) -> [(usize, u32); 6] {
        [
            (WATCHDOG_ADDRESS, self.watchdog_address),
            (L2_ADDRESS, self.l2_address),
            (RAM_START, self.ram_start),
            (RAM_END, self.ram_end),
            (INITRD_ADDRESS, self.initrd_address),
            (INITRD_LIMIT, self.initrd_limit),
        ]
    }
}

/// Whether `file` is Cascadia's loader as its build leaves it or as a
/// bundle carries it: its entry instruction and its configuration record.
pub fn is_loader(file: &[u8]) -> bool {
    file.len() >= CONFIG_AT + CONFIG_SIZE
        && file[..4] == ENTRY
        && &file[CONFIG_AT..CONFIG_AT + 4] == b"CB32"
}

/// An assembled bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// Everything the iBEC is sent: loader, kernel, device tree, initramfs.
    pub image: Vec<u8>,
    loader: usize,
}

impl Bundle {
    /// The loader as it stands at the start of the bundle, with its
    /// configuration filled in.
    pub fn loader(&self) -> &[u8] {
        &self.image[..self.loader]
    }
}

fn le32(data: &[u8], at: usize) -> Result<u32> {
    let bytes = data.get(at..at + 4).ok_or(Error("truncated image"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    let bytes = data.get(at..at + 4).ok_or(Error("truncated device tree"))?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

fn aligned(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}

/// The configuration words of a loader as its build leaves it, for a board
/// that loads it where it was built for: nothing filled in yet.
fn pristine(loader: &[u8], profile: &Profile) -> Result<[u32; 16]> {
    let fits = (CONFIG_AT + CONFIG_SIZE..=LOADER_SPACE as usize).contains(&loader.len());
    if !fits || loader[..4] != ENTRY {
        return Err(Error(
            "the loader does not start with its entry instruction",
        ));
    }
    let record = &loader[CONFIG_AT..CONFIG_AT + CONFIG_SIZE];
    let mut words = [0u32; 16];
    for (index, word) in words.iter_mut().enumerate() {
        *word = le32(record, 4 + index * 4)?;
    }
    if &record[..4] != b"CB32" || words[VERSION] != 1 || words[HEADER_SIZE] != CONFIG_SIZE as u32 {
        return Err(Error("unknown loader configuration format"));
    }
    if profile
        .built()
        .iter()
        .any(|&(at, value)| words[at] != value)
    {
        return Err(Error(
            "the loader is built for another load address or kernel offset",
        ));
    }
    // orchard builds a loader for its board, with the board's addresses in
    // the record (`boot/cascadia/build --profile`); they have to be this
    // board's. What a bundle adds (the tree, the initramfs, the size and the
    // checksum) has to be empty still.
    let board = profile.board();
    for (at, word) in words.iter().enumerate().skip(DTB_OFFSET) {
        match board.iter().find(|(place, _)| *place == at) {
            Some(&(_, value)) if *word == value || *word == 0 => {}
            Some(_) => return Err(Error("the loader is built for another board's addresses")),
            None if *word == 0 => {}
            None => return Err(Error("the loader's configuration is already filled in")),
        }
    }
    Ok(words)
}

/// The offsets of the values of `/chosen` `linux,initrd-start` and `-end` in
/// `dtb`, each one cell.
fn initrd_cells(dtb: &[u8]) -> Result<(usize, usize)> {
    if dtb.len() < 40 {
        return Err(Error("truncated device tree"));
    }
    let word = |index: usize| be32(dtb, index * 4).map(|w| w as usize);
    let (total, tree_at, strings_at) = (word(1)?, word(2)?, word(3)?);
    let (version, last, strings_size, tree_size) = (word(5)?, word(6)?, word(8)?, word(9)?);
    if word(0)? != 0xd00d_feed || total != dtb.len() || version < 17 || last > 17 {
        return Err(Error("unsupported device tree header"));
    }
    let inside =
        |at: usize, size: usize| at >= 40 && at.checked_add(size).is_some_and(|e| e <= total);
    if tree_at % 4 != 0
        || tree_size == 0
        || !inside(tree_at, tree_size)
        || !inside(strings_at, strings_size)
    {
        return Err(Error("invalid device tree block bounds"));
    }
    let end = tree_at + tree_size;
    let names = &dtb[strings_at..strings_at + strings_size];
    let string = |data: &'_ [u8], at: usize| -> Result<usize> {
        let rest = data
            .get(at..)
            .ok_or(Error("device tree string outside its block"))?;
        rest.iter()
            .position(|&b| b == 0)
            .ok_or(Error("unterminated device tree string"))
    };
    let mut at = tree_at;
    // The names of the nodes that are open, the root first.
    let mut open: Vec<&[u8]> = Vec::new();
    let (mut first, mut last) = (None, None);
    loop {
        if at + 4 > end {
            return Err(Error("the device tree lacks its end token"));
        }
        let token = be32(dtb, at)?;
        at += 4;
        match token {
            1 => {
                let length = string(&dtb[..end], at)?;
                if open.len() >= 128 {
                    return Err(Error("the device tree is nested too deeply"));
                }
                open.push(&dtb[at..at + length]);
                at = aligned((at + length + 1) as u64, 4) as usize;
            }
            2 => {
                open.pop().ok_or(Error("unbalanced device tree end-node"))?;
            }
            3 => {
                if open.is_empty() || at + 8 > end {
                    return Err(Error("invalid device tree property position"));
                }
                let (length, name_at) = (be32(dtb, at)? as usize, be32(dtb, at + 4)? as usize);
                at += 8;
                if at.checked_add(length).is_none_or(|e| e > end) {
                    return Err(Error("a device tree property extends past its block"));
                }
                let name = &names[name_at.min(names.len())..];
                let name = &name[..string(names, name_at)?];
                let slot = match name {
                    b"linux,initrd-start" => Some(&mut first),
                    b"linux,initrd-end" => Some(&mut last),
                    _ => None,
                };
                if let (Some(slot), [b"", b"chosen"]) = (slot, open.as_slice()) {
                    if length != 4 || slot.is_some() {
                        return Err(Error("a /chosen initrd property is not one cell"));
                    }
                    *slot = Some(at);
                }
                at = aligned((at + length) as u64, 4) as usize;
            }
            4 => {}
            9 if open.is_empty() => break,
            _ => return Err(Error("unknown or misplaced device tree token")),
        }
    }
    match (first, last) {
        (Some(first), Some(last)) => Ok((first, last)),
        _ => Err(Error(
            "the device tree has no /chosen linux,initrd-start and -end to set",
        )),
    }
}

/// Where the ARM decompressor's work ends in memory: the inflated kernel,
/// its BSS, and room for the relocated zImage, a grown device tree, stack
/// and heap above it. Read from the zImage's size table (Linux 6.12).
fn decompression_end(kernel: &[u8], dtb_size: usize, profile: &Profile) -> Result<u64> {
    if kernel.len() < 0x3c || !kernel.len().is_multiple_of(8) {
        return Err(Error(
            "the zImage must end on an eight-byte boundary for its device tree",
        ));
    }
    if dtb_size > 0x10_0000 {
        return Err(Error("the device tree is too large for the decompressor"));
    }
    if (le32(kernel, 0x30)?, le32(kernel, 0x34)?) != (0x0403_0201, 0x4545_4545) {
        return Err(Error("the zImage has no little-endian size-table header"));
    }
    let table = (le32(kernel, 0x38)? as usize)
        .checked_sub(le32(kernel, 0x28)? as usize)
        .filter(|&at| at >= 0x3c && at + 28 <= kernel.len())
        .ok_or(Error("the zImage size table is outside the kernel"))?;
    let field = |index: usize| le32(kernel, table + index * 4);
    let (size_at, bss, text_offset, heap) = (field(2)? as usize, field(3)?, field(4)?, field(5)?);
    if (field(0)?, field(1)?, field(6)?) != (6, 0x5a53_4c4b, 0)
        || size_at < 0x3c
        || size_at + 4 > kernel.len()
    {
        return Err(Error("unsupported zImage size table"));
    }
    let inflated = le32(kernel, size_at)?;
    if inflated == 0 || text_offset < 0x8000 || heap > 0x10_0000 {
        return Err(Error("invalid zImage decompression parameters"));
    }
    // The decompressor lets the tree grow by half, up to 1 MiB; 128 KiB
    // more cover its own relocated code and stack.
    let tree = aligned((dtb_size + dtb_size / 2) as u64, 8).clamp(0x8000, 0x10_0000);
    let work = aligned(
        kernel.len() as u64 + tree + u64::from(heap) + 0x2_0000,
        4096,
    );
    Ok(u64::from(profile.ram_start)
        + u64::from(text_offset)
        + u64::from(inflated)
        + u64::from(bss)
        + work)
}

/// Assembles the bundle for one board: the loader as built, the unchanged
/// zImage `kernel`, the board's device tree and the initramfs, for the
/// board's addresses in `profile`. `serial` is the serial number of the
/// device the bundle is for.
pub fn bundle(
    loader: &[u8],
    kernel: &[u8],
    dtb: &[u8],
    initramfs: &[u8],
    profile: &Profile,
    serial: Option<&str>,
) -> Result<Bundle> {
    profile.check()?;
    let mut words = pristine(loader, profile)?;
    if kernel.len() < 0x30 || le32(kernel, 0x24)? != 0x016f_2818 {
        return Err(Error("the kernel is not an ARM zImage"));
    }
    let (start, end) = (le32(kernel, 0x28)?, le32(kernel, 0x2c)?);
    if end <= start || (end - start) as usize != kernel.len() {
        return Err(Error(
            "the zImage's length differs from its header, or bytes follow it",
        ));
    }
    let space = u64::from(profile.initrd_limit - profile.initrd_address);
    if initramfs.is_empty() || initramfs.len() as u64 > space {
        return Err(Error("the initramfs does not fit its place in memory"));
    }
    let named;
    let dtb = match serial {
        Some(serial) => {
            let plain = serial.bytes().all(|b| b.is_ascii_alphanumeric());
            if !plain || !(1..=32).contains(&serial.len()) {
                return Err(Error("a serial number is 1 to 32 letters and digits"));
            }
            let mut tree = Tree::read(dtb)?;
            tree.root
                .set("serial-number", [serial.as_bytes(), &[0]].concat());
            named = tree.write();
            named.as_slice()
        }
        None => dtb,
    };
    let (first, last) = initrd_cells(dtb)?;
    let mut tree = dtb.to_vec();
    tree[first..first + 4].copy_from_slice(&profile.initrd_address.to_be_bytes());
    let initrd_end = profile.initrd_address + initramfs.len() as u32;
    tree[last..last + 4].copy_from_slice(&initrd_end.to_be_bytes());

    if decompression_end(kernel, tree.len(), profile)? > u64::from(profile.initrd_address) {
        return Err(Error(
            "the kernel's decompression would run into the initramfs",
        ));
    }
    let kernel_at = u64::from(profile.kernel_offset);
    let dtb_at = kernel_at + kernel.len() as u64;
    let initrd_at = aligned(dtb_at + tree.len() as u64, 8);
    let size = initrd_at + initramfs.len() as u64;
    if u64::from(profile.load_address) + size > u64::from(profile.initrd_address) {
        return Err(Error("the bundle would overlap the initramfs's place"));
    }
    for (at, value) in profile.board() {
        words[at] = value;
    }
    // Below the initramfs's 32-bit address, so every offset fits a word.
    words[DTB_OFFSET] = dtb_at as u32;
    words[DTB_SIZE] = tree.len() as u32;
    words[INITRD_OFFSET] = initrd_at as u32;
    words[INITRD_SIZE] = initramfs.len() as u32;
    words[IMAGE_SIZE] = size as u32;

    let mut image = loader.to_vec();
    let record = &mut image[CONFIG_AT..CONFIG_AT + CONFIG_SIZE];
    for (index, word) in words.iter().enumerate().take(CRC32) {
        record[4 + index * 4..8 + index * 4].copy_from_slice(&word.to_le_bytes());
    }
    let crc = crc32fast::hash(&record[..CONFIG_SIZE - 4]);
    record[CONFIG_SIZE - 4..].copy_from_slice(&crc.to_le_bytes());

    image.resize(kernel_at as usize, 0);
    image.extend_from_slice(kernel);
    image.extend_from_slice(&tree);
    image.resize(initrd_at as usize, 0);
    image.extend_from_slice(initramfs);
    Ok(Bundle {
        image,
        loader: loader.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    const PROFILE: Profile = Profile {
        load_address: 0x8000_0000,
        kernel_offset: 0x8000,
        watchdog_address: 0x0f10_3020,
        l2_address: 0x3e00_0000,
        ram_start: 0x8000_0000,
        ram_end: 0xa000_0000,
        initrd_address: 0x8800_0000,
        initrd_limit: 0x9000_0000,
    };

    /// A second board's profile: no L2 controller, less memory.
    const SMALL: Profile = Profile {
        watchdog_address: 0x0f20_0000,
        l2_address: 0,
        ram_end: 0x9000_0000,
        initrd_address: 0x8400_0000,
        initrd_limit: 0x8800_0000,
        ..PROFILE
    };

    /// The one loader as its build leaves it: the entry instruction, and
    /// the configuration record with where it is loaded, where the kernel
    /// sits, and nothing of a board.
    fn loader(profile: &Profile) -> Vec<u8> {
        let mut loader = vec![0u8; CONFIG_AT + CONFIG_SIZE];
        loader[..4].copy_from_slice(&ENTRY);
        loader[CONFIG_AT..CONFIG_AT + 4].copy_from_slice(b"CB32");
        let mut words = [0u32; 16];
        words[VERSION] = 1;
        words[HEADER_SIZE] = CONFIG_SIZE as u32;
        for (at, value) in profile.built() {
            words[at] = value;
        }
        for (index, word) in words.iter().enumerate() {
            let at = CONFIG_AT + 4 + index * 4;
            loader[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        loader
    }

    /// A zImage of `size` bytes whose size table says it inflates to
    /// `inflated` bytes.
    fn zimage(size: usize, inflated: u32) -> Vec<u8> {
        let mut kernel: Vec<u8> = (0..size).map(|i| (i % 253) as u8).collect();
        let mut put =
            |at: usize, value: u32| kernel[at..at + 4].copy_from_slice(&value.to_le_bytes());
        put(0x24, 0x016f_2818);
        put(0x28, 0);
        put(0x2c, size as u32);
        put(0x30, 0x0403_0201);
        put(0x34, 0x4545_4545);
        put(0x38, 0x40);
        // The table: six fields, its magic, where the inflated size is, BSS,
        // text offset, heap, and the end mark.
        for (index, value) in [6, 0x5a53_4c4b, 0x60, 0x1000, 0x8000, 0x1_0000, 0]
            .into_iter()
            .enumerate()
        {
            put(0x40 + index * 4, value);
        }
        put(0x60, inflated);
        kernel
    }

    /// A device tree: the root with `model`, and `/chosen` with boot
    /// arguments and, when `placeholders`, the two initrd cells.
    fn dtb(placeholders: bool) -> Vec<u8> {
        let mut names = Vec::new();
        let mut tree = Vec::new();
        let word = |tree: &mut Vec<u8>, value: u32| tree.extend(value.to_be_bytes());
        let mut property = |tree: &mut Vec<u8>, name: &str, value: &[u8]| {
            tree.extend(3u32.to_be_bytes());
            tree.extend((value.len() as u32).to_be_bytes());
            tree.extend((names.len() as u32).to_be_bytes());
            names.extend(name.as_bytes());
            names.push(0);
            tree.extend(value);
            tree.resize(tree.len().next_multiple_of(4), 0);
        };
        word(&mut tree, 1);
        word(&mut tree, 0);
        property(&mut tree, "model", b"Test Board\0");
        word(&mut tree, 1);
        tree.extend(b"chosen\0\0");
        property(&mut tree, "bootargs", b"console=tty0\0");
        if placeholders {
            property(&mut tree, "linux,initrd-start", &[0; 4]);
            property(&mut tree, "linux,initrd-end", &[0; 4]);
        }
        word(&mut tree, 2);
        // A node behind /chosen with the same property names says nothing.
        word(&mut tree, 1);
        tree.extend(b"other\0\0\0");
        property(&mut tree, "linux,initrd-start", &[0; 8]);
        word(&mut tree, 2);
        word(&mut tree, 2);
        word(&mut tree, 9);

        let tree_at = 40 + 16;
        let strings_at = tree_at + tree.len();
        let total = strings_at + names.len();
        let mut out = Vec::new();
        for value in [
            0xd00d_feed,
            total,
            tree_at,
            strings_at,
            40,
            17,
            16,
            0,
            names.len(),
            tree.len(),
        ] {
            out.extend((value as u32).to_be_bytes());
        }
        out.extend([0; 16]);
        out.extend(tree);
        out.extend(names);
        out
    }

    fn initramfs() -> Vec<u8> {
        (0..3001).map(|i| (i % 7) as u8).collect()
    }

    #[test]
    fn the_bundle_is_loader_kernel_tree_and_initramfs() {
        // One loader for both boards: nothing of a board is in it.
        assert_eq!(loader(&PROFILE), loader(&SMALL));
        for profile in [PROFILE, SMALL] {
            let (kernel, tree, initrd) = (zimage(0x400, 0x10_0000), dtb(true), initramfs());
            let bundle =
                bundle(&loader(&profile), &kernel, &tree, &initrd, &profile, None).unwrap();
            let image = &bundle.image;

            // The kernel sits behind the loader's 32 KiB, unchanged, and the
            // tree right behind it.
            assert_eq!(image[0x8000..0x8400], kernel[..]);
            let tree_at = 0x8400;
            let initrd_at = (tree_at + tree.len()).next_multiple_of(8);
            assert_eq!(image[initrd_at..], initrd[..]);

            // The tree differs only in the two cells, which name the copy.
            let packed = &image[tree_at..tree_at + tree.len()];
            let (first, last) = initrd_cells(&tree).unwrap();
            assert_eq!(be32(packed, first).unwrap(), profile.initrd_address);
            assert_eq!(be32(packed, last).unwrap(), profile.initrd_address + 3001);
            let changed = (0..tree.len()).filter(|&i| packed[i] != tree[i]).count();
            assert!(changed <= 8);

            // The record says where everything is, under its checksum.
            let record = &image[CONFIG_AT..CONFIG_AT + CONFIG_SIZE];
            let word = |index: usize| le32(record, 4 + index * 4).unwrap();
            assert_eq!(word(DTB_OFFSET) as usize, tree_at);
            assert_eq!(word(DTB_SIZE) as usize, tree.len());
            assert_eq!(word(INITRD_OFFSET) as usize, initrd_at);
            assert_eq!(word(INITRD_SIZE), 3001);
            assert_eq!(word(IMAGE_SIZE) as usize, image.len());
            // The board's addresses are in the record too, for the loader
            // to work from.
            assert_eq!(word(INITRD_ADDRESS), profile.initrd_address);
            assert_eq!(word(INITRD_LIMIT), profile.initrd_limit);
            assert_eq!(word(WATCHDOG_ADDRESS), profile.watchdog_address);
            assert_eq!(word(L2_ADDRESS), profile.l2_address);
            assert_eq!(
                (word(RAM_START), word(RAM_END)),
                (profile.ram_start, profile.ram_end)
            );
            assert_eq!(word(CRC32), crc32fast::hash(&record[..64]));

            // Both are told from other files by what they hold.
            assert!(is_loader(&loader(&profile)) && is_loader(image));
            assert!(!is_loader(&kernel) && !is_loader(&image[..CONFIG_AT]));
            // The loader on its own is the bundle's start, record included.
            assert_eq!(bundle.loader().len(), CONFIG_AT + CONFIG_SIZE);
            assert_eq!(bundle.loader(), &image[..CONFIG_AT + CONFIG_SIZE]);
        }
    }

    #[test]
    fn the_same_bytes_as_orchards_assembler() {
        // What orchard's `image/cascadia.py` assembles from these inputs with
        // the first profile, measured against it.
        let bundle = bundle(
            &loader(&PROFILE),
            &zimage(0x400, 0x10_0000),
            &dtb(true),
            &initramfs(),
            &PROFILE,
            None,
        )
        .unwrap();
        let hash: String = Sha256::digest(&bundle.image)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            hash,
            "03df769fabbdd8e2a03deca9fb4ba06e699b561b48d06f45d1d76f14d37de19b"
        );
    }

    #[test]
    fn the_serial_number_goes_to_the_trees_root() {
        let (kernel, tree, initrd) = (zimage(0x400, 0x10_0000), dtb(true), initramfs());
        let pack = |serial| bundle(&loader(&PROFILE), &kernel, &tree, &initrd, &PROFILE, serial);
        let named = pack(Some("F4KXY12ZAB")).unwrap();
        // The tree in the bundle: the property behind the root's others,
        // the initrd cells set, everything else as it was.
        let record = &named.image[CONFIG_AT..CONFIG_AT + CONFIG_SIZE];
        let word = |index: usize| le32(record, 4 + index * 4).unwrap() as usize;
        let packed = &named.image[word(DTB_OFFSET)..word(DTB_OFFSET) + word(DTB_SIZE)];
        let read = Tree::read(packed).unwrap();
        let before = Tree::read(&tree).unwrap();
        assert_eq!(read.root.properties.len(), before.root.properties.len() + 1);
        assert_eq!(read.root.properties.last().unwrap().0, "serial-number");
        assert_eq!(read.root.properties.last().unwrap().1, b"F4KXY12ZAB\0");
        assert_eq!(read.root.children.len(), before.root.children.len());
        let chosen = &read.root.children[0];
        assert_eq!(chosen.properties[0], before.root.children[0].properties[0]);
        assert_eq!(chosen.properties[1].1, PROFILE.initrd_address.to_be_bytes());
        // What orchard's `image/cascadia.py --serial` assembles from these
        // inputs, measured against it.
        let hash: String = Sha256::digest(&named.image)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            hash,
            "b0a7e49c567a8227e4bf5f45b1a1c6803b12d4a1c209d6e5b5a44fdbca792bc6"
        );

        // Without one, the tree is only touched in its two cells.
        assert_ne!(pack(None).unwrap().image, named.image);
        for bad in ["", "two words", &"x".repeat(33)] {
            assert_eq!(
                pack(Some(bad)).unwrap_err().0,
                "a serial number is 1 to 32 letters and digits"
            );
        }
    }

    #[test]
    fn a_loader_that_is_not_the_one_as_built_is_refused() {
        let (kernel, tree, initrd) = (zimage(0x400, 0x10_0000), dtb(true), initramfs());
        let pack = |loader: &[u8]| {
            bundle(loader, &kernel, &tree, &initrd, &PROFILE, None)
                .unwrap_err()
                .0
        };

        // One built to be loaded somewhere else.
        let mut elsewhere = loader(&PROFILE);
        elsewhere[CONFIG_AT + 4 + LOAD_ADDRESS * 4 + 3] = 0x40;
        assert_eq!(
            pack(&elsewhere),
            "the loader is built for another load address or kernel offset"
        );
        let mut data = loader(&PROFILE);
        data[0] = 0;
        assert_eq!(
            pack(&data),
            "the loader does not start with its entry instruction"
        );
        assert_eq!(
            pack(&loader(&PROFILE)[..CONFIG_AT]),
            "the loader does not start with its entry instruction"
        );
        // One that was packed before cannot be packed again.
        let packed = bundle(&loader(&PROFILE), &kernel, &tree, &initrd, &PROFILE, None).unwrap();
        assert_eq!(
            pack(packed.loader()),
            "the loader's configuration is already filled in"
        );
    }

    #[test]
    fn inputs_that_do_not_fit_are_refused() {
        let (kernel, tree, initrd) = (zimage(0x400, 0x10_0000), dtb(true), initramfs());
        let pack = |kernel: &[u8], tree: &[u8], initrd: &[u8], profile: &Profile| {
            bundle(&loader(profile), kernel, tree, initrd, profile, None)
                .unwrap_err()
                .0
        };

        assert_eq!(
            pack(&kernel, &dtb(false), &initrd, &PROFILE),
            "the device tree has no /chosen linux,initrd-start and -end to set"
        );
        assert_eq!(
            pack(&kernel, &tree[..tree.len() - 1], &initrd, &PROFILE),
            "unsupported device tree header"
        );
        assert_eq!(
            pack(&kernel, &tree, &[], &PROFILE),
            "the initramfs does not fit its place in memory"
        );
        assert_eq!(
            pack(&[0; 0x400], &tree, &initrd, &PROFILE),
            "the kernel is not an ARM zImage"
        );
        // Bytes behind the zImage would sit where its tree belongs.
        let mut longer = kernel.clone();
        longer.extend([0; 8]);
        assert_eq!(
            pack(&longer, &tree, &initrd, &PROFILE),
            "the zImage's length differs from its header, or bytes follow it"
        );
        // A kernel that inflates into the initramfs's place: 64 MiB of room
        // on the smaller board.
        assert_eq!(
            pack(&zimage(0x400, 0x400_0000), &tree, &initrd, &SMALL),
            "the kernel's decompression would run into the initramfs"
        );
        bundle(
            &loader(&PROFILE),
            &zimage(0x400, 0x400_0000),
            &tree,
            &initrd,
            &PROFILE,
            None,
        )
        .unwrap();
    }

    #[test]
    fn a_profile_must_be_in_order() {
        PROFILE.check().unwrap();
        SMALL.check().unwrap();
        let bad = [
            Profile {
                kernel_offset: 0x4000,
                ..PROFILE
            },
            Profile {
                load_address: 0x8100_0000,
                ram_start: 0x8100_0000,
                ..PROFILE
            },
            Profile {
                initrd_address: 0x8800_0100,
                ..PROFILE
            },
            Profile {
                initrd_limit: 0xa000_1000,
                ..PROFILE
            },
            Profile {
                initrd_address: 0x9000_0000,
                ..PROFILE
            },
            Profile {
                watchdog_address: 0x0f10_3021,
                ..PROFILE
            },
        ];
        for profile in bad {
            assert!(profile.check().is_err(), "{profile:?}");
        }
    }
}

//! Instruction patches for decoded iBoot images: the ARM32 [`Autogo`]
//! trampoline and ARM64 [`anchor`] patches.
//!
//! Autogo uses Thumb-2 code: on these chips recovery mode will not start
//! an uploaded image on its own, so the host would have to send a command to
//! a device that is about to stop answering. The patch makes recovery jump to
//! the uploaded image itself once the upload is complete, so no such command
//! is needed. The addresses it needs differ per iBoot build and come from the
//! board data.
pub mod anchor;

use crate::{Error, Result};

/// Where in a decrypted iBoot image the autogo patch acts. `hook` and
/// `trampoline` are offsets into the image; the other three are addresses in
/// the running iBoot. All come from the recipe's `autogo` patch step.
pub struct Autogo {
    /// Offset of the instruction that is redirected to the trampoline.
    pub hook: u32,
    /// The bytes expected at `hook`. A different iBoot build has other code
    /// there; refusing a mismatch keeps the patch from corrupting it.
    pub original: Vec<u8>,
    /// Offset of unused space that receives the trampoline instructions.
    pub trampoline: u32,
    /// Address of iBoot's cache flush, called before the image is entered.
    pub cache_flush: u32,
    /// Address of the byte iBoot sets when an upload has finished.
    pub done_flag: u32,
    /// Address where the replaced instruction's work continues.
    pub usb_continue: u32,
}

/// Encodes a Thumb-2 `B.W` (or `BL` when `link`) from `from` to `to`, as the
/// four little-endian bytes of its two halfwords. Both addresses are the
/// runtime addresses of the branch and its target.
fn branch(from: u32, to: u32, link: bool) -> Result<[u8; 4]> {
    let delta = i64::from(to) - i64::from(from) - 4;
    if delta & 1 != 0 || !(-(1 << 24)..(1 << 24)).contains(&delta) {
        return Err(Error("Thumb branch target out of range"));
    }
    let bits = delta as u32;
    let sign = (bits >> 24) & 1;
    let j1 = 1 ^ ((bits >> 23) & 1) ^ sign;
    let j2 = 1 ^ ((bits >> 22) & 1) ^ sign;
    let first = 0xf000 | sign << 10 | ((bits >> 12) & 0x3ff);
    let second = if link { 0xd000 } else { 0x9000 } | j1 << 13 | j2 << 11 | ((bits >> 1) & 0x7ff);
    let mut bytes = [0; 4];
    bytes[..2].copy_from_slice(&(first as u16).to_le_bytes());
    bytes[2..].copy_from_slice(&(second as u16).to_le_bytes());
    Ok(bytes)
}

/// Encodes a Thumb-2 `MOVW r0, #value` (or `MOVT r0, #value` when `top`), as
/// four little-endian bytes. Two of these load a 32-bit address into r0.
fn mov16(value: u16, top: bool) -> [u8; 4] {
    let first = if top { 0xf2c0 } else { 0xf240 } | (value >> 12) | ((value >> 11) & 1) << 10;
    let second = ((value >> 8) & 7) << 12 | (value & 0xff);
    let mut bytes = [0; 4];
    bytes[..2].copy_from_slice(&first.to_le_bytes());
    bytes[2..].copy_from_slice(&second.to_le_bytes());
    bytes
}

impl Autogo {
    /// Applies the patch to `image`, a decrypted iBoot. The image's load
    /// address is read from its header and turns the offsets into runtime
    /// addresses. Fails without changing `image` when the header is too short,
    /// the bytes at `hook` are not the expected ones, the trampoline space is
    /// too small or already used, or an address does not fit.
    pub fn apply(&self, image: &mut [u8]) -> Result<()> {
        let base = u32::from_le_bytes(
            image
                .get(0x20..0x24)
                .ok_or(Error("truncated iBoot header"))?
                .try_into()
                .unwrap(),
        ) & !0xfffff;
        let hook = self.hook as usize;
        let cave = self.trampoline as usize;
        if self.original.len() < 4
            || image.get(hook..hook + self.original.len()) != Some(&self.original)
        {
            return Err(Error("autogo hook bytes differ"));
        }
        let address = base
            .checked_add(self.trampoline)
            .ok_or(Error("autogo address overflow"))?;
        // The branch that replaces the hooked instruction, computed before any
        // write so a failure here leaves the image untouched.
        let redirect = branch(
            base.checked_add(self.hook)
                .ok_or(Error("autogo address overflow"))?,
            address,
            false,
        )?;
        // ldr r1,[r4,#16]; cmp r1,#0x8000; bhs done; ldr r6,[r5,#4]
        let mut code = vec![0x21, 0x69, 0xb1, 0xf5, 0x00, 0x4f, 0x04, 0xd2, 0x6e, 0x68];
        code.extend(branch(
            address
                .checked_add(10)
                .ok_or(Error("autogo address overflow"))?,
            self.cache_flush,
            true,
        )?);
        code.extend([0x31, 0x46, 0x88, 0x47]); // mov r1,r6; blx r1
        code.extend(mov16(self.done_flag as u16, false));
        code.extend(mov16((self.done_flag >> 16) as u16, true));
        // movs r1,#1; strb r1,[r0]; add.w r0,r5,#8; pop.w {r4,r5,r7,lr}
        code.extend([
            0x01, 0x21, 0x01, 0x70, 0x05, 0xf1, 0x08, 0x00, 0xbd, 0xe8, 0xb0, 0x40,
        ]);
        code.extend(branch(
            address
                .checked_add(code.len() as u32)
                .ok_or(Error("autogo address overflow"))?,
            self.usb_continue,
            false,
        )?);
        let target = image
            .get_mut(cave..cave + code.len())
            .ok_or(Error("autogo trampoline outside image"))?;
        if target.iter().any(|b| !matches!(b, 0 | 255)) {
            return Err(Error("autogo trampoline space is occupied"));
        }
        target.copy_from_slice(&code);
        image[hook..hook + 4].copy_from_slice(&redirect);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOK: usize = 0x1000;
    const CAVE: usize = 0x45c80;

    /// `branch` encodes the two halfwords of a Thumb-2 wide branch. The
    /// vectors are what the assembler produces for the same source and target.
    #[test]
    fn branch_matches_the_assembler() {
        assert_eq!(
            branch(HOOK as u32, CAVE as u32, false).unwrap(),
            [0x44, 0xf0, 0x3e, 0xbe]
        );
        // A short forward call (BL), and a backward branch.
        assert_eq!(
            branch(0x100, 0x110, true).unwrap(),
            [0x00, 0xf0, 0x06, 0xf8]
        );
        assert_eq!(
            branch(0x110, 0x100, false).unwrap(),
            [0xff, 0xf7, 0xf6, 0xbf]
        );
    }

    #[test]
    fn branch_rejects_odd_or_distant_targets() {
        assert!(branch(0x1000, 0x1001, false).is_err());
        assert!(branch(0, (1 << 24) + 8, false).is_err());
        assert!(branch((1 << 24) + 8, 0, false).is_err());
    }

    /// `mov16` encodes MOVW and MOVT of r0; two of them build a 32-bit address.
    #[test]
    fn mov16_matches_the_assembler() {
        assert_eq!(mov16(0xb2b0, false), [0x4b, 0xf2, 0xb0, 0x20]); // movw r0,#0xb2b0
        assert_eq!(mov16(0x9ff4, true), [0xc9, 0xf6, 0xf4, 0x70]); // movt r0,#0x9ff4
    }

    /// An image loaded at `base`, with known bytes at the hook and room for
    /// the trampoline, and a patch whose addresses fit it.
    fn sample(base: u32) -> (Vec<u8>, Autogo) {
        let mut image = vec![0u8; CAVE + 64];
        image[0x20..0x24].copy_from_slice(&base.to_le_bytes());
        image[HOOK..HOOK + 6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        let patch = Autogo {
            hook: HOOK as u32,
            original: vec![1, 2, 3, 4, 5, 6],
            trampoline: CAVE as u32,
            cache_flush: base + 0x2000,
            done_flag: 0x9ff4_b2b0,
            usb_continue: base + 0x3000,
        };
        (image, patch)
    }

    #[test]
    fn redirects_the_hook_and_fills_the_trampoline() {
        let base = 0x9ff0_0000;
        let (mut image, patch) = sample(base);
        patch.apply(&mut image).unwrap();
        // The hooked instruction now branches to the trampoline address.
        assert_eq!(
            image[HOOK..HOOK + 4],
            branch(base + HOOK as u32, base + CAVE as u32, false).unwrap()
        );
        // The trampoline, once empty, now holds code.
        assert!(image[CAVE..CAVE + 48].iter().any(|&b| b != 0));
    }

    #[test]
    fn refuses_a_short_header() {
        let patch = sample(0x9ff0_0000).1;
        assert_eq!(
            patch.apply(&mut [0u8; 8]),
            Err(Error("truncated iBoot header"))
        );
    }

    #[test]
    fn refuses_unexpected_hook_bytes() {
        let (mut image, patch) = sample(0x9ff0_0000);
        image[HOOK] = 0xff;
        let before = image.clone();
        assert_eq!(
            patch.apply(&mut image),
            Err(Error("autogo hook bytes differ"))
        );
        assert_eq!(image, before, "a refused patch leaves the image unchanged");
    }

    #[test]
    fn refuses_an_occupied_trampoline() {
        let (mut image, patch) = sample(0x9ff0_0000);
        image[CAVE + 2] = 0x42;
        let before = image.clone();
        assert_eq!(
            patch.apply(&mut image),
            Err(Error("autogo trampoline space is occupied"))
        );
        assert_eq!(image, before, "a refused patch leaves the image unchanged");
    }

    #[test]
    fn refuses_a_trampoline_past_the_image() {
        let (mut image, mut patch) = sample(0x9ff0_0000);
        patch.trampoline = image.len() as u32 - 4;
        assert_eq!(
            patch.apply(&mut image),
            Err(Error("autogo trampoline outside image"))
        );
    }
}

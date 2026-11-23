//! Cascadia's ARM64 iBSS anchor patches: validation result, Image4
//! authentication continuation and the boot-argument literal.
//!
//! Reference: rzxvx/Project-Cascadia@091d441f19e101699f7e1835a80ccb48d6e54eec,
//! devices/a12/boot/patch_iboot.py and prepare-xr-linux-bootset.py.
use crate::{Error, Result};
use std::ops::Range;

const ANCHOR: &[u8] = b"Unknown ASN1 type %llu\n\0";
const MOV_X0_X20: u32 = 0xaa14_03e0;
const MOV_X0_ZERO: u32 = 0xd280_0000;
const FRAME_RESTORE: u32 = 0xa94d_7bfd;

/// The formatting call's destination holds 0x400 bytes, including the NUL.
/// Percent signs would be interpreted as format conversions by that call.
pub fn validate_args(args: &str) -> Result<()> {
    if args.is_empty()
        || args.len() >= 0x400
        || !args.is_ascii()
        || args.bytes().any(|b| matches!(b, 0 | b'%'))
    {
        return Err(Error(
            "cascadia-anchor needs 1..1023 ASCII boot-argument bytes without NUL or percent",
        ));
    }
    Ok(())
}

fn word(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn signed(value: u32, bits: u32) -> i64 {
    i64::from((value << (32 - bits)) as i32 >> (32 - bits))
}

fn relative(pc: usize, delta: i64) -> Option<usize> {
    usize::try_from(pc as i128 + i128::from(delta)).ok()
}

fn adrp_page(instruction: u32, pc: usize) -> Option<usize> {
    let immediate = ((instruction >> 5) & 0x7ffff) << 2 | ((instruction >> 29) & 3);
    relative(pc & !0xfff, signed(immediate, 21) << 12)
}

fn add_immediate(instruction: u32) -> usize {
    (((instruction >> 10) & 0xfff) as usize) << (12 * ((instruction >> 22) & 1))
}

fn branch19(instruction: u32, pc: usize) -> Option<usize> {
    relative(pc, signed((instruction >> 5) & 0x7ffff, 19) * 4)
}

fn encode_branch(pc: usize, target: usize) -> Result<u32> {
    let delta = target as i128 - pc as i128;
    if delta % 4 != 0 || !(-(1 << 27)..(1 << 27)).contains(&delta) {
        return Err(Error("cascadia-anchor branch target out of range"));
    }
    Ok(0x1400_0000 | ((delta / 4) as u32 & 0x03ff_ffff))
}

fn encode_adrp(pc: usize, target: usize) -> Result<u32> {
    let pages = ((target & !0xfff) as i128 - (pc & !0xfff) as i128) / 4096;
    if !(-(1 << 20)..(1 << 20)).contains(&pages) {
        return Err(Error("cascadia-anchor ADRP target out of range"));
    }
    let immediate = pages as u32 & 0x1f_ffff;
    Ok(0x9000_0002 | ((immediate & 3) << 29) | ((immediate >> 2) << 5))
}

fn unique<T>(mut candidates: Vec<T>, message: &'static str) -> Result<T> {
    if candidates.len() != 1 {
        return Err(Error(message));
    }
    Ok(candidates.remove(0))
}

/// Find the result move next to the stack-cookie guard. The guard stays intact.
fn validation_result(data: &[u8]) -> Result<usize> {
    let anchor = unique(
        data.windows(ANCHOR.len())
            .enumerate()
            .filter_map(|(at, bytes)| (bytes == ANCHOR).then_some(at))
            .collect(),
        "cascadia-anchor needs exactly one ASN1 string",
    )?;
    let mut candidates = Vec::new();
    for xref in (0..data.len().saturating_sub(8)).step_by(4) {
        let a = word(data, xref).unwrap();
        let b = word(data, xref + 4).unwrap();
        if a & 0x9f00_0000 != 0x9000_0000
            || b & 0x7f80_0000 != 0x1100_0000
            || a & 31 != (b >> 5) & 31
            || adrp_page(a, xref).and_then(|p| p.checked_add(add_immediate(b))) != Some(anchor)
        {
            continue;
        }
        let Some(target) = xref.checked_sub(8) else {
            continue;
        };
        for at in (xref.saturating_sub(0x800) & !3..xref).step_by(4) {
            if at < 4 {
                continue;
            }
            let branch = word(data, at).unwrap();
            if branch & 0xff00_001f != 0x5400_0001
                || branch19(branch, at) != Some(target)
                || word(data, at + 4) != Some(MOV_X0_X20)
            {
                continue;
            }
            let end = data.len().min(at + 0x84);
            if (at + 8..end)
                .step_by(4)
                .any(|p| word(data, p) == Some(FRAME_RESTORE))
            {
                candidates.push(at + 4);
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    unique(
        candidates,
        "cascadia-anchor needs exactly one validation-result site",
    )
}

/// The manifest authenticator's CBZ and its existing success continuation.
fn manifest_gate(data: &[u8]) -> Result<(usize, usize)> {
    let mut candidates = Vec::new();
    for call in (8..data.len().saturating_sub(24)).step_by(4) {
        let at = call + 4;
        let branch = word(data, at).unwrap();
        if word(data, call - 8) != Some(MOV_X0_X20)
            || word(data, call - 4) != Some(0xaa15_03e3)
            || word(data, call).unwrap() & 0xfc00_0000 != 0x9400_0000
            || branch & 0xff00_001f != 0x3400_0000
            || word(data, call + 8) != Some(0x5100_7b75)
            || word(data, call + 12).unwrap() & 0xfc00_0000 != 0x1400_0000
        {
            continue;
        }
        let Some(target) = branch19(branch, at) else {
            continue;
        };
        if target <= at {
            continue;
        }
        if word(data, target).is_some_and(|w| w & 0xffc0_03ff == 0x9100_03e8)
            && target
                .checked_add(4)
                .and_then(|p| word(data, p))
                .is_some_and(|w| w & 0xffc0_03ff == 0x9100_0119)
        {
            candidates.push((at, target));
        }
    }
    unique(
        candidates,
        "cascadia-anchor needs exactly one manifest-auth gate",
    )
}

fn bootargs_call(data: &[u8]) -> Result<usize> {
    let mut candidates = Vec::new();
    for at in (0..data.len().saturating_sub(20)).step_by(4) {
        let a = word(data, at).unwrap();
        let b = word(data, at + 4).unwrap();
        let destination = word(data, at + 8).unwrap() & 0xffc0_03ff;
        if a & 0x9f00_001f != 0x9000_0002
            || b & 0xffc0_03ff != 0x9100_0042
            || !matches!(destination, 0x9100_03e0 | 0xd100_03a0)
            || word(data, at + 12) != Some(0x5280_8001)
            || word(data, at + 16).unwrap() & 0xfc00_0000 != 0x9400_0000
        {
            continue;
        }
        let Some(format) = adrp_page(a, at).and_then(|p| p.checked_add(add_immediate(b))) else {
            continue;
        };
        if format >= 1
            && format
                .checked_add(3)
                .and_then(|end| data.get(format - 1..end))
                == Some(b"\0%s\0")
        {
            candidates.push(at);
        }
    }
    unique(
        candidates,
        "cascadia-anchor needs exactly one boot-args call",
    )
}

fn slot_in(run: &Range<usize>, length: usize) -> Option<usize> {
    let guarded = run.start.checked_add(8)?;
    for alignment in [16, 8, 4, 2, 1] {
        let at = guarded.checked_add(alignment - 1)? & !(alignment - 1);
        if at.checked_add(length)? <= run.end {
            return Some(at);
        }
    }
    None
}

/// Match the reference's allocation: unique largest page tail, otherwise the
/// smallest fitting alignment-padding run, with earlier offsets breaking ties.
fn literal_slot(data: &[u8], length: usize) -> Result<usize> {
    let mut runs = Vec::new();
    let mut start = None;
    for (at, byte) in data.iter().copied().chain(std::iter::once(1)).enumerate() {
        match (start, byte == 0) {
            (None, true) => start = Some(at),
            (Some(begin), false) => {
                runs.push(begin..at);
                start = None;
            }
            _ => {}
        }
    }
    let mut tails: Vec<_> = runs.iter().filter(|r| r.end % 4096 == 0).collect();
    tails.sort_by_key(|r| std::cmp::Reverse(r.len()));
    if tails.iter().any(|r| slot_in(r, length).is_some()) {
        if tails.len() > 1 && tails[0].len() == tails[1].len() {
            return Err(Error("cascadia-anchor has ambiguous page-tail space"));
        }
        return slot_in(tails[0], length)
            .ok_or(Error("cascadia-anchor page-tail slot is too small"));
    }
    let mut padding: Vec<_> = runs
        .iter()
        .filter(|r| {
            let alignment = 1usize << r.end.trailing_zeros();
            alignment >= 256 && r.len() < alignment
        })
        .collect();
    padding.sort_by_key(|r| r.len());
    padding
        .iter()
        .find_map(|r| slot_in(r, length))
        .ok_or(Error("cascadia-anchor has no space for boot arguments"))
}

struct Edit {
    at: usize,
    original: Vec<u8>,
    replacement: Vec<u8>,
}

impl Edit {
    fn instruction(data: &[u8], at: usize, replacement: u32) -> Result<Self> {
        let original = word(data, at).ok_or(Error("cascadia-anchor truncated instruction"))?;
        Ok(Self {
            at,
            original: original.to_le_bytes().to_vec(),
            replacement: replacement.to_le_bytes().to_vec(),
        })
    }
}

/// Patch one decoded ARM64 iBSS. All sites are resolved and checked against
/// the original image before constructing the result. `boot_args` is literal
/// recipe data, including any requested serial-console setting.
pub fn patch(data: &[u8], boot_args: &str) -> Result<Vec<u8>> {
    validate_args(boot_args)?;
    let validate = validation_result(data)?;
    let (auth, success) = manifest_gate(data)?;
    let args = bootargs_call(data)?;
    let mut literal = boot_args.as_bytes().to_vec();
    literal.push(0);
    let slot = literal_slot(data, literal.len())?;
    let mut edits = vec![
        Edit::instruction(data, validate, MOV_X0_ZERO)?,
        Edit::instruction(data, auth, encode_branch(auth, success)?)?,
        Edit::instruction(data, args, encode_adrp(args, slot)?)?,
        Edit::instruction(data, args + 4, 0x9100_0042 | ((slot as u32 & 0xfff) << 10))?,
        Edit {
            at: slot,
            original: vec![0; literal.len()],
            replacement: literal,
        },
    ];
    edits.sort_by_key(|e| e.at);
    let mut end = 0;
    for edit in &edits {
        if edit.at < end {
            return Err(Error("cascadia-anchor patch sites overlap"));
        }
        end = edit
            .at
            .checked_add(edit.original.len())
            .ok_or(Error("cascadia-anchor patch range overflow"))?;
        if data.get(edit.at..end) != Some(edit.original.as_slice()) {
            return Err(Error("cascadia-anchor original bytes differ"));
        }
    }
    let mut out = data.to_vec();
    for edit in edits {
        out[edit.at..edit.at + edit.replacement.len()].copy_from_slice(&edit.replacement);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

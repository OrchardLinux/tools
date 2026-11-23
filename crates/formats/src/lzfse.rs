//! LZFSE and LZVN decoding: raw, LZVN and FSE (v1 and v2) blocks.
//!
//! Work is linear in the input: every block consumes input, and output is
//! capped by the size the caller expects.
use crate::{Error, Result};

/// Decodes a framed LZFSE stream that must produce exactly `expected` bytes.
pub fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let out = stream(input, expected)?;
    if out.len() != expected {
        return Err(Error("LZFSE output has the wrong size"));
    }
    Ok(out)
}

/// Decodes a framed LZFSE stream of unknown size, refusing output over `limit`.
pub fn decode_limited(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    stream(input, limit)
}

/// Decodes a bare LZVN stream (no block header), as decmpfs stores it.
pub fn decode_lzvn(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    lzvn(input, &mut out, expected)?;
    if out.len() != expected {
        return Err(Error("LZVN output has the wrong size"));
    }
    Ok(out)
}

fn stream(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let magic = bytes(input, at, 4)?;
        if magic == b"bvx$" {
            if at + 4 != input.len() {
                return Err(Error("data after the LZFSE end marker"));
            }
            return Ok(out);
        }
        let target = out
            .len()
            .checked_add(le32(input, at + 4)? as usize)
            .ok_or(OVERFLOW)?;
        if target > limit {
            return Err(Error("LZFSE output exceeds the expected size"));
        }
        at = match magic {
            b"bvx-" => {
                let start = at + 8;
                let size = target - out.len();
                append(&mut out, bytes(input, start, size)?, target)?;
                start + size
            }
            b"bvxn" => {
                let size = le32(input, at + 8)? as usize;
                let start = at + 12;
                lzvn(bytes(input, start, size)?, &mut out, target)?;
                start + size
            }
            b"bvx1" | b"bvx2" => fse(input, at, &mut out, target)?,
            _ => return Err(Error("unknown LZFSE block type")),
        };
        if out.len() != target {
            return Err(Error("LZFSE block has the wrong size"));
        }
    }
}

const OVERFLOW: Error = Error("LZFSE size overflow");

fn bytes(data: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    data.get(at..at.checked_add(len).ok_or(OVERFLOW)?)
        .ok_or(Error("truncated LZFSE stream"))
}

fn le16(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(data, at, 2)?.try_into().unwrap()))
}

fn le32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, at, 4)?.try_into().unwrap()))
}

fn le64(data: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, at, 8)?.try_into().unwrap()))
}

fn append(out: &mut Vec<u8>, data: &[u8], target: usize) -> Result<()> {
    if out.len() + data.len() > target {
        return Err(Error("LZFSE block exceeds its declared size"));
    }
    out.extend_from_slice(data);
    Ok(())
}

fn copy_match(out: &mut Vec<u8>, distance: usize, count: usize, target: usize) -> Result<()> {
    if distance == 0 || distance > out.len() || out.len() + count > target {
        return Err(Error("bad LZFSE match"));
    }
    let start = out.len() - distance;
    let mut left = count;
    // An overlapping match repeats the bytes since `start`; copy in growing
    // pieces instead of byte by byte.
    while left > 0 {
        let n = left.min(out.len() - start);
        out.extend_from_within(start..start + n);
        left -= n;
    }
    Ok(())
}

fn lzvn(input: &[u8], out: &mut Vec<u8>, target: usize) -> Result<()> {
    let mut at = 0;
    let mut distance = 0;
    while at < input.len() {
        let op = input[at];
        match op {
            // The end marker is opcode 6 followed by seven zero bytes.
            6 => {
                if bytes(input, at, 8)? != [6, 0, 0, 0, 0, 0, 0, 0] || at + 8 != input.len() {
                    return Err(Error("bad LZVN end marker"));
                }
                return Ok(());
            }
            0x0e | 0x16 => at += 1,
            0xe0 => {
                let n = usize::from(bytes(input, at + 1, 1)?[0]) + 16;
                append(out, bytes(input, at + 2, n)?, target)?;
                at += 2 + n;
            }
            0xe1..=0xef => {
                let n = usize::from(op & 15);
                append(out, bytes(input, at + 1, n)?, target)?;
                at += 1 + n;
            }
            0xf0 => {
                copy_match(
                    out,
                    distance,
                    usize::from(bytes(input, at + 1, 1)?[0]) + 16,
                    target,
                )?;
                at += 2;
            }
            0xf1..=0xff => {
                copy_match(out, distance, usize::from(op & 15), target)?;
                at += 1;
            }
            0xa0..=0xbf => {
                let word = usize::from(le16(input, at + 1)?);
                at += 3;
                distance = word >> 2;
                let literals = usize::from(op >> 3 & 3);
                let matched = (usize::from(op & 7) << 2 | word & 3) + 3;
                append(out, bytes(input, at, literals)?, target)?;
                at += literals;
                copy_match(out, distance, matched, target)?;
            }
            0x70..=0x7f | 0xd0..=0xdf => return Err(Error("undefined LZVN opcode")),
            _ if op & 7 == 6 && op < 0x40 => return Err(Error("undefined LZVN opcode")),
            _ => {
                let literals = usize::from(op >> 6);
                let matched = usize::from(op >> 3 & 7) + 3;
                match op & 7 {
                    7 => {
                        distance = usize::from(le16(input, at + 1)?);
                        at += 3;
                    }
                    6 => at += 1,
                    low => {
                        distance = usize::from(low) << 8 | usize::from(bytes(input, at + 1, 1)?[0]);
                        at += 2;
                    }
                }
                append(out, bytes(input, at, literals)?, target)?;
                at += literals;
                copy_match(out, distance, matched, target)?;
            }
        }
    }
    Err(Error("LZVN stream has no end marker"))
}

/// Bits used by each 5-bit code in the v2 frequency table.
const NBITS: [u8; 16] = [2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14];
const VALUE: [u8; 32] = [
    0, 2, 1, 4, 0, 3, 1, 0, 0, 2, 1, 5, 0, 3, 1, 0, 0, 2, 1, 6, 0, 3, 1, 0, 0, 2, 1, 7, 0, 3, 1, 0,
];

/// Frequencies per table: 20 L, 20 M, 64 D, 256 literal symbols.
const SYMBOLS: usize = 360;
/// Size of a v1 header: 50 fixed bytes, the frequencies and 2 bytes padding.
const V1_HEADER: usize = 50 + 2 * SYMBOLS + 2;

struct Header {
    payload: usize,
    literal_bytes: usize,
    lmd_bytes: usize,
    literals: usize,
    matches: usize,
    literal_bits: i32,
    lmd_bits: i32,
    literal_states: [usize; 4],
    states: [usize; 3],
    frequencies: [u16; SYMBOLS],
}

fn bits(word: u64, offset: u32, width: u32) -> usize {
    (word >> offset & ((1 << width) - 1)) as usize
}

fn header(input: &[u8], at: usize) -> Result<Header> {
    let mut frequencies = [0; SYMBOLS];
    let h = if bytes(input, at, 4)? == b"bvx1" {
        bytes(input, at, V1_HEADER)?;
        for (i, f) in frequencies.iter_mut().enumerate() {
            *f = le16(input, at + 50 + 2 * i)?;
        }
        let u16_at = |p| le16(input, at + p).map(usize::from);
        let h = Header {
            payload: at + V1_HEADER,
            literal_bytes: le32(input, at + 20)? as usize,
            lmd_bytes: le32(input, at + 24)? as usize,
            literals: le32(input, at + 12)? as usize,
            matches: le32(input, at + 16)? as usize,
            literal_bits: le32(input, at + 28)? as i32,
            lmd_bits: le32(input, at + 40)? as i32,
            literal_states: [u16_at(32)?, u16_at(34)?, u16_at(36)?, u16_at(38)?],
            states: [u16_at(44)?, u16_at(46)?, u16_at(48)?],
            frequencies,
        };
        if le32(input, at + 8)? as usize != h.literal_bytes + h.lmd_bytes {
            return Err(Error("LZFSE v1 payload size mismatch"));
        }
        h
    } else {
        let (v0, v1, v2) = (
            le64(input, at + 8)?,
            le64(input, at + 16)?,
            le64(input, at + 24)?,
        );
        let len = bits(v2, 0, 32);
        if len < 32 {
            return Err(Error("LZFSE v2 header too short"));
        }
        let table = bytes(input, at + 32, len - 32)?;
        let (mut acc, mut have, mut p) = (0u32, 0u32, 0);
        for f in &mut frequencies {
            while p < table.len() && have <= 24 {
                acc |= u32::from(table[p]) << have;
                have += 8;
                p += 1;
            }
            let code = (acc & 31) as usize;
            let n = u32::from(NBITS[code & 15]);
            if n > have {
                return Err(Error("truncated LZFSE frequency table"));
            }
            *f = match n {
                8 => 8 + (acc >> 4 & 15) as u16,
                14 => 24 + (acc >> 4 & 1023) as u16,
                _ => u16::from(VALUE[code]),
            };
            acc >>= n;
            have -= n;
        }
        if p != table.len() || have >= 8 {
            return Err(Error("data after the LZFSE frequency table"));
        }
        Header {
            payload: at + len,
            literal_bytes: bits(v0, 20, 20),
            lmd_bytes: bits(v1, 40, 20),
            literals: bits(v0, 0, 20),
            matches: bits(v0, 40, 20),
            literal_bits: bits(v0, 60, 3) as i32 - 7,
            lmd_bits: bits(v1, 60, 3) as i32 - 7,
            literal_states: [
                bits(v1, 0, 10),
                bits(v1, 10, 10),
                bits(v1, 20, 10),
                bits(v1, 30, 10),
            ],
            states: [bits(v2, 32, 10), bits(v2, 42, 10), bits(v2, 52, 10)],
            frequencies,
        }
    };
    if h.literals > 40_000
        || h.matches > 10_000
        || h.literal_states.iter().any(|&s| s >= 1024)
        || h.states[0] >= 64
        || h.states[1] >= 64
        || h.states[2] >= 256
    {
        return Err(Error("LZFSE block header out of range"));
    }
    Ok(h)
}

/// FSE bit streams are read from the end towards the start.
struct Backward<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Backward<'a> {
    fn new(data: &'a [u8], initial: i32) -> Result<Backward<'a>> {
        if !(-7..=0).contains(&initial) {
            return Err(Error("bad FSE initial bit count"));
        }
        let position = (data.len() * 8)
            .checked_sub(initial.unsigned_abs() as usize)
            .ok_or(Error("truncated FSE stream"))?;
        Ok(Backward { data, position })
    }

    fn pull(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.position = self
            .position
            .checked_sub(n as usize)
            .ok_or(Error("FSE stream underflow"))?;
        let first = self.position / 8;
        let last = (self.position + n as usize - 1) / 8;
        let word = self.data[first..=last]
            .iter()
            .rev()
            .fold(0u64, |w, &b| w << 8 | u64::from(b));
        Ok((word >> (self.position & 7) & ((1 << n) - 1)) as u32)
    }
}

#[derive(Clone, Copy)]
struct State {
    bits: u32,
    delta: u32,
    symbol: usize,
}

fn table(states: u32, frequencies: &[u16]) -> Result<Vec<State>> {
    if frequencies.iter().map(|&f| u32::from(f)).sum::<u32>() != states {
        return Err(Error("bad FSE frequency sum"));
    }
    let mut table = Vec::with_capacity(states as usize);
    for (symbol, &f) in frequencies.iter().enumerate().filter(|(_, &f)| f != 0) {
        let f = u32::from(f);
        let k = f.leading_zeros() - states.leading_zeros();
        let j0 = ((2 * states) >> k) - f;
        for j in 0..f {
            let (bits, delta) = if j < j0 {
                (k, ((f + j) << k) - states)
            } else {
                let bits = k.checked_sub(1).ok_or(Error("bad FSE table"))?;
                (bits, (j - j0) << bits)
            };
            table.push(State {
                bits,
                delta,
                symbol,
            });
        }
    }
    Ok(table)
}

/// An L, M or D value table: extra bits and base value per symbol.
struct Values {
    states: Vec<State>,
    extra: Vec<u32>,
    base: Vec<u32>,
}

impl Values {
    fn new(states: u32, frequencies: &[u16], extra: Vec<u32>) -> Result<Values> {
        let mut base = Vec::with_capacity(extra.len());
        let mut next = 0;
        for bits in &extra {
            base.push(next);
            next += 1 << bits;
        }
        Ok(Values {
            states: table(states, frequencies)?,
            extra,
            base,
        })
    }

    fn next(&self, stream: &mut Backward<'_>, state: &mut usize) -> Result<usize> {
        let s = self
            .states
            .get(*state)
            .ok_or(Error("FSE state outside table"))?;
        let extra = self.extra[s.symbol];
        let read = stream.pull(s.bits + extra)?;
        *state = (s.delta + (read >> extra)) as usize;
        Ok((self.base[s.symbol] + (read & ((1 << extra) - 1))) as usize)
    }
}

fn fse(input: &[u8], at: usize, out: &mut Vec<u8>, target: usize) -> Result<usize> {
    let h = header(input, at)?;
    let lmd_at = h.payload + h.literal_bytes;
    let literal_input = bytes(input, h.payload, h.literal_bytes)?;
    let lmd_input = bytes(input, lmd_at, h.lmd_bytes)?;

    let literal_table = table(1024, &h.frequencies[104..])?;
    let mut stream = Backward::new(literal_input, h.literal_bits)?;
    let mut states = h.literal_states;
    let mut literals = Vec::with_capacity(h.literals + 4);
    for _ in 0..h.literals.div_ceil(4) {
        for state in &mut states {
            let s = literal_table
                .get(*state)
                .ok_or(Error("FSE state outside table"))?;
            literals.push(s.symbol as u8);
            *state = (s.delta + stream.pull(s.bits)?) as usize;
        }
    }

    let small = |tail: [u32; 4]| {
        (0..20)
            .map(|i| if i < 16 { 0 } else { tail[i - 16] })
            .collect()
    };
    let l = Values::new(64, &h.frequencies[..20], small([2, 3, 5, 8]))?;
    let m = Values::new(64, &h.frequencies[20..40], small([3, 5, 8, 11]))?;
    let d = Values::new(
        256,
        &h.frequencies[40..104],
        (0..64).map(|i| i / 4).collect(),
    )?;
    let mut stream = Backward::new(lmd_input, h.lmd_bits)?;
    let [mut ls, mut ms, mut ds] = h.states;
    let (mut used, mut distance) = (0, 0);
    for _ in 0..h.matches {
        let literal_count = l.next(&mut stream, &mut ls)?;
        let match_len = m.next(&mut stream, &mut ms)?;
        let new_distance = d.next(&mut stream, &mut ds)?;
        if new_distance != 0 {
            distance = new_distance;
        }
        let end = used + literal_count;
        if end > h.literals {
            return Err(Error("FSE match uses more literals than the block has"));
        }
        append(out, &literals[used..end], target)?;
        used = end;
        if match_len != 0 {
            copy_match(out, distance, match_len, target)?;
        }
    }
    Ok(lmd_at + h.lmd_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_block() {
        let mut s = b"bvx-".to_vec();
        s.extend(5u32.to_le_bytes());
        s.extend(b"hello");
        s.extend(b"bvx$");
        assert_eq!(decode(&s, 5).unwrap(), b"hello");
        assert!(decode(&s, 4).is_err());
        assert_eq!(decode_limited(&s, 5).unwrap(), b"hello");
    }

    #[test]
    fn lzvn_literal_and_overlapping_match() {
        // 0xe3: three literals "abc". 0x08: no literals, a match of 4 at the
        // distance in the next byte (3), overlapping its own output.
        let stream = [0xe3, b'a', b'b', b'c', 0x08, 0x03, 6, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_lzvn(&stream, 7).unwrap(), b"abcabca");
    }

    #[test]
    fn lzvn_needs_its_end_marker() {
        assert!(decode_lzvn(&[0xe1, b'x'], 1).is_err());
        assert!(decode_lzvn(&[0xe1, b'x', 6, 0, 0, 0, 0, 0, 0, 1], 1).is_err());
    }

    #[test]
    fn rejects_trailing_data_and_unknown_blocks() {
        assert!(decode(b"bvx$x", 0).is_err());
        assert!(decode(b"bvxq\0\0\0\0bvx$", 0).is_err());
    }
}

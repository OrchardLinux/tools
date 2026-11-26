//! SHA-512 crypt (`$6$`), the password hash `/etc/shadow` and orchard's PIN
//! file hold. The install system has the new root's own `chpasswd` and
//! `openssl passwd -6` make it; from iOS nothing of the root can run.
use sha2::{Digest, Sha512};

const ROUNDS: usize = 5000;
const ALPHABET: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `len` bytes of `block`, repeated.
fn cycled(block: &[u8], len: usize) -> Vec<u8> {
    block.iter().copied().cycle().take(len).collect()
}

/// The hash of `password` with `salt` (up to 16 characters of the crypt
/// alphabet), as `$6$salt$hash`.
pub fn sha512(password: &[u8], salt: &str) -> String {
    let salt = &salt.as_bytes()[..salt.len().min(16)];
    let b = Sha512::new().chain_update(password).chain_update(salt).chain_update(password).finalize();
    let mut a = Sha512::new().chain_update(password).chain_update(salt).chain_update(cycled(&b, password.len()));
    let mut bits = password.len();
    while bits > 0 {
        if bits & 1 == 1 {
            a.update(b);
        } else {
            a.update(password);
        }
        bits >>= 1;
    }
    let mut a = a.finalize();
    let mut dp = Sha512::new();
    for _ in 0..password.len() {
        dp.update(password);
    }
    let p = cycled(&dp.finalize(), password.len());
    let mut ds = Sha512::new();
    for _ in 0..16 + a[0] as usize {
        ds.update(salt);
    }
    let s = cycled(&ds.finalize(), salt.len());
    for round in 0..ROUNDS {
        let mut c = Sha512::new();
        if round % 2 == 1 {
            c.update(&p);
        } else {
            c.update(a);
        }
        if round % 3 != 0 {
            c.update(&s);
        }
        if round % 7 != 0 {
            c.update(&p);
        }
        if round % 2 == 1 {
            c.update(a);
        } else {
            c.update(&p);
        }
        a = c.finalize();
    }
    let mut out = format!("$6${}$", String::from_utf8_lossy(salt));
    let mut push = |b2: u8, b1: u8, b0: u8, count: usize| {
        let mut w = (b2 as u32) << 16 | (b1 as u32) << 8 | b0 as u32;
        for _ in 0..count {
            out.push(ALPHABET[(w & 0x3f) as usize] as char);
            w >>= 6;
        }
    };
    let (mut x, mut y, mut z) = (0usize, 21usize, 42usize);
    for _ in 0..21 {
        push(a[x], a[y], a[z], 4);
        (x, y, z) = ((y + 1) % 63, (z + 1) % 63, (x + 1) % 63);
    }
    push(0, 0, a[63], 2);
    out
}

/// A fresh 16-character salt.
pub fn salt() -> String {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).expect("system randomness");
    raw.iter().map(|b| ALPHABET[(b & 0x3f) as usize] as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specifications_vectors() {
        assert_eq!(
            sha512(b"Hello world!", "saltstring"),
            "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
        );
        // Salts are cut at 16 characters.
        assert!(sha512(b"x", "0123456789abcdefXYZ").starts_with("$6$0123456789abcdef$"));
        assert_eq!(salt().len(), 16);
    }
}

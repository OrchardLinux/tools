//! tar archives (ustar, with pax headers for long paths), as GitHub serves
//! repository snapshots.
use crate::{Error, Result};

const BLOCK: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive<'a> {
    /// The pax global `comment`, which GitHub sets to the commit id.
    pub comment: Option<String>,
    /// Regular files, in order.
    pub files: Vec<File<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File<'a> {
    pub path: String,
    /// The permission bits.
    pub mode: u32,
    /// When it was last changed, in seconds since 1970.
    pub mtime: u64,
    pub data: &'a [u8],
}

pub fn read(bytes: &[u8]) -> Result<Archive<'_>> {
    let mut archive = Archive {
        comment: None,
        files: Vec::new(),
    };
    let mut long_path: Option<String> = None;
    let mut at = 0;
    while at + BLOCK <= bytes.len() {
        let header = &bytes[at..at + BLOCK];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let checksum: u32 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u32::from(b)
                }
            })
            .sum();
        if octal(&header[148..156])? != u64::from(checksum) {
            return Err(Error("tar header checksum mismatch"));
        }
        let size =
            usize::try_from(octal(&header[124..136])?).map_err(|_| Error("tar entry too large"))?;
        let start = at + BLOCK;
        let data = bytes
            .get(start..start + size)
            .ok_or(Error("truncated tar entry"))?;
        at = start + size.div_ceil(BLOCK) * BLOCK;
        match header[156] {
            b'g' => {
                archive.comment = pax(data)?
                    .into_iter()
                    .find(|(k, _)| k == "comment")
                    .map(|(_, v)| v)
            }
            b'x' => {
                long_path = pax(data)?
                    .into_iter()
                    .find(|(k, _)| k == "path")
                    .map(|(_, v)| v)
            }
            kind => {
                let path = match long_path.take() {
                    Some(p) => p,
                    None => {
                        let name = text(&header[0..100])?;
                        // ustar keeps the first part of a long path in the prefix field.
                        match text(&header[345..500])? {
                            prefix if !prefix.is_empty() && &header[257..262] == b"ustar" => {
                                format!("{prefix}/{name}")
                            }
                            _ => name,
                        }
                    }
                };
                if matches!(kind, b'0' | 0) {
                    archive.files.push(File { path, mode: octal(&header[100..108]).unwrap_or(0o644) as u32 & 0o7777, mtime: octal(&header[136..148]).unwrap_or(0), data });
                }
            }
        }
    }
    Ok(archive)
}

/// A regular file to write, with its permission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub path: &'a str,
    pub mode: u32,
    pub data: &'a [u8],
}

/// A tar archive of `entries`, in order: ustar headers, owner root, time 0,
/// and a pax header for a path that doesn't fit in 100 bytes. The same
/// entries always give the same bytes.
pub fn write(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for entry in entries {
        if entry.path.is_empty() || entry.path.contains('\0') {
            return Err(Error("bad tar path"));
        }
        if entry.path.len() > 100 {
            let record = pax_record("path", entry.path);
            push(&mut out, "././@PaxHeader", 0o644, b'x', record.as_bytes());
        }
        let name: String = entry.path.chars().take(100).collect();
        push(&mut out, &name, entry.mode, b'0', entry.data);
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}

/// One pax record; its length counts itself.
fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut len = body.len() + 1;
    while len.to_string().len() + body.len() != len {
        len = len.to_string().len() + body.len();
    }
    format!("{len}{body}")
}

fn push(out: &mut Vec<u8>, name: &str, mode: u32, kind: u8, data: &[u8]) {
    let mut h = [0u8; BLOCK];
    h[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
    h[100..107].copy_from_slice(format!("{:07o}", mode & 0o7777).as_bytes());
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[265..269].copy_from_slice(b"root");
    h[297..301].copy_from_slice(b"root");
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(BLOCK), 0);
}

/// pax records: `<length> <key>=<value>\n`.
fn pax(mut data: &[u8]) -> Result<Vec<(String, String)>> {
    let mut records = Vec::new();
    while !data.is_empty() {
        let space = data
            .iter()
            .position(|&b| b == b' ')
            .ok_or(Error("bad pax record"))?;
        let len: usize = std::str::from_utf8(&data[..space])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(Error("bad pax record"))?;
        let record = data.get(space + 1..len).ok_or(Error("bad pax record"))?;
        let record = std::str::from_utf8(record.strip_suffix(b"\n").unwrap_or(record))
            .map_err(|_| Error("pax record is not UTF-8"))?;
        let (key, value) = record.split_once('=').ok_or(Error("bad pax record"))?;
        records.push((key.to_owned(), value.to_owned()));
        data = &data[len..];
    }
    Ok(records)
}

fn octal(field: &[u8]) -> Result<u64> {
    let s = text(field)?;
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| Error("bad octal number in tar header"))
}

fn text(field: &[u8]) -> Result<String> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8(field[..end].to_vec()).map_err(|_| Error("tar header text is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, kind: u8, size: usize, prefix: &str) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        h
    }

    fn entry(out: &mut Vec<u8>, name: &str, kind: u8, data: &[u8], prefix: &str) {
        out.extend(header(name, kind, data.len(), prefix));
        out.extend(data);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }

    /// A pax record; its length field counts its own digits.
    fn record(key: &str, value: &str) -> Vec<u8> {
        let body = format!(" {key}={value}\n");
        let mut len = body.len() + 1;
        while len != body.len() + len.to_string().len() {
            len = body.len() + len.to_string().len();
        }
        format!("{len}{body}").into_bytes()
    }

    #[test]
    fn reads_files_long_paths_and_comment() {
        let mut t = Vec::new();
        entry(
            &mut t,
            "pax_global_header",
            b'g',
            &record("comment", "abc123"),
            "",
        );
        entry(&mut t, "repo/", b'5', b"", "");
        entry(&mut t, "repo/a.txt", b'0', b"hello", "");
        entry(&mut t, "b.txt", b'0', b"prefixed", "repo/deep");
        let long = format!("repo/{}", "x".repeat(150));
        entry(&mut t, "PaxHeader", b'x', &record("path", &long), "");
        entry(&mut t, "truncated-name", b'0', b"long", "");
        t.extend([0; 2 * BLOCK]);
        let a = read(&t).unwrap();
        assert_eq!(a.comment.as_deref(), Some("abc123"));
        let paths: Vec<_> = a.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["repo/a.txt", "repo/deep/b.txt", long.as_str()]);
        assert_eq!(a.files[2].data, b"long");
    }

    #[test]
    fn rejects_damaged_headers() {
        let mut t = Vec::new();
        entry(&mut t, "a", b'0', b"x", "");
        t[0] = b'b';
        assert!(read(&t).is_err());
    }

    #[test]
    fn written_archives_read_back_with_long_paths() {
        let long = format!("lib/firmware/brcm/{}.txt", "x".repeat(120));
        let bytes = write(&[
            Entry { path: "mixin.json", mode: 0o600, data: b"{}" },
            Entry { path: &long, mode: 0o600, data: b"FW" },
        ])
        .unwrap();
        let archive = read(&bytes).unwrap();
        let files: Vec<(&str, &[u8])> = archive.files.iter().map(|f| (f.path.as_str(), f.data)).collect();
        assert_eq!(files, [("mixin.json", &b"{}"[..]), (long.as_str(), &b"FW"[..])]);
        assert_eq!(pax_record("path", "a"), "9 path=a\n");
    }
}

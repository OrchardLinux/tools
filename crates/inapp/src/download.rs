//! Pinned files fetched into the cache: resumed where an earlier run
//! stopped, paused between pieces while the storage is split, checked by
//! size and SHA-256 before they get their name. The cache has the tethered
//! installer's layout (`files/<sha256>`).
use orchard_boards::release::Receipt;
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::{Code, Error, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PIECE: usize = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where a pinned file is in the cache once it is complete.
pub fn place(cache: &Path, receipt: &Receipt) -> PathBuf {
    cache.join("files").join(&receipt.sha256)
}

fn partial(cache: &Path, receipt: &Receipt) -> PathBuf {
    cache.join("files").join(format!("{}.partial", receipt.sha256))
}

/// How much of a file the cache holds.
pub fn have(cache: &Path, receipt: &Receipt) -> u64 {
    let len = |path: PathBuf| std::fs::metadata(path).map(|m| m.len()).ok();
    match len(place(cache, receipt)) {
        Some(n) if n == receipt.bytes => n,
        _ => len(partial(cache, receipt)).unwrap_or(0).min(receipt.bytes),
    }
}

/// A local file given in place of a download (`file://`), checked like one.
fn local(path: &Path, receipt: &Receipt) -> Result<PathBuf> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::from(e).context(path.display()))?;
    let mut hash = Sha256::new();
    let copied = std::io::copy(&mut file, &mut hash)?;
    if copied != receipt.bytes || hex(&hash.finalize()) != receipt.sha256 {
        return Err(Error::new(Code::Format, "the file differs from what the release states").context(path.display()));
    }
    Ok(path.to_owned())
}

/// The file in the cache, fetching what is missing. `go` is asked before
/// every piece; `Ok(None)` when it said no. `got` hears each piece's size.
pub fn fetch(cache: &Path, receipt: &Receipt, go: &dyn Fn() -> bool, got: &dyn Fn(u64)) -> Result<Option<PathBuf>> {
    if let Some(path) = receipt.url.strip_prefix("file://") {
        return local(Path::new(path), receipt).map(Some);
    }
    let done = place(cache, receipt);
    if std::fs::metadata(&done).is_ok_and(|m| m.len() == receipt.bytes) {
        return Ok(Some(done));
    }
    std::fs::create_dir_all(cache.join("files"))?;
    let part = partial(cache, receipt);
    let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part)?;
    // What is there already counts once it has been hashed again.
    let mut hash = Sha256::new();
    let mut offset = 0u64;
    let mut buffer = vec![0u8; PIECE];
    let kept = file.metadata()?.len().min(receipt.bytes);
    while offset < kept {
        let n = file.read(&mut buffer[..PIECE.min((kept - offset) as usize)])?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        offset += n as u64;
    }
    file.set_len(offset)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut http = Http::new(&receipt.url, receipt.bytes, TIMEOUT)?;
    while offset < receipt.bytes {
        if !go() {
            file.sync_all()?;
            return Ok(None);
        }
        let piece = http.fetch(offset, PIECE.min((receipt.bytes - offset) as usize))?;
        hash.update(&piece);
        file.write_all(&piece)?;
        offset += piece.len() as u64;
        got(piece.len() as u64);
    }
    file.sync_all()?;
    drop(file);
    let actual = hex(&hash.finalize());
    if actual != receipt.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(Error::new(Code::Format, format!("SHA-256 is {actual}, the release expects {}", receipt.sha256)).context(&receipt.url));
    }
    std::fs::rename(&part, &done)?;
    Ok(Some(done))
}

/// A small file into memory (an index, a package database).
pub fn small(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).map_err(|e| Error::from(e).context(path));
    }
    let mut http = Http::probe(url, TIMEOUT)?;
    let size = http.size() as usize;
    http.fetch(0, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(path: &Path, data: &[u8]) -> Receipt {
        Receipt { url: format!("file://{}", path.display()), bytes: data.len() as u64, sha256: hex(&Sha256::digest(data)), expanded_bytes: None }
    }

    #[test]
    fn a_local_file_is_checked_like_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layer");
        std::fs::write(&path, b"layer").unwrap();
        let good = receipt(&path, b"layer");
        assert_eq!(fetch(dir.path(), &good, &|| true, &|_| {}).unwrap(), Some(path.clone()));
        let bad = Receipt { sha256: "0".repeat(64), ..good };
        assert!(fetch(dir.path(), &bad, &|| true, &|_| {}).is_err());
    }

    #[test]
    fn what_the_cache_holds_counts() {
        let dir = tempfile::tempdir().unwrap();
        let r = Receipt { url: "https://example/x".into(), bytes: 10, sha256: "ab".repeat(32), expanded_bytes: None };
        assert_eq!(have(dir.path(), &r), 0);
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(partial(dir.path(), &r), b"1234").unwrap();
        assert_eq!(have(dir.path(), &r), 4);
        std::fs::write(place(dir.path(), &r), b"1234567890").unwrap();
        assert_eq!(have(dir.path(), &r), 10);
    }
}

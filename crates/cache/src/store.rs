//! The cache: one directory of files, each named after its key. Every write
//! and every read checks the file's SHA-256.
use crate::{Code, Context, Error, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Store {
    files: PathBuf,
}

/// What a cached file is found by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// The file's own SHA-256, when it is known in advance.
    Sha256(String),
    /// A description of where the file comes from, when its hash is not known
    /// in advance. It must change whenever the content could.
    Origin(String),
}

/// One cached file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
}

impl Store {
    /// Opens the cache in `dir`, or in the platform's cache directory.
    pub fn open(dir: Option<&Path>) -> Result<Store> {
        let root = match dir {
            Some(dir) => dir.to_owned(),
            None => default_dir()?,
        };
        let files = root.join("files");
        fs::create_dir_all(&files).context(files.display())?;
        Ok(Store { files })
    }

    pub fn dir(&self) -> &Path {
        self.files.parent().unwrap_or(&self.files)
    }

    /// The cached file for `key`, if there is an intact one.
    pub fn get(&self, key: &Key) -> Result<Option<Vec<u8>>> {
        let Some(path) = self.find(key)? else {
            return Ok(None);
        };
        let bytes = fs::read(&path).context(path.display())?;
        let expected = match key {
            Key::Sha256(hex) => hex.to_ascii_lowercase(),
            Key::Origin(_) => path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .rsplit('-')
                .next()
                .unwrap_or("")
                .to_owned(),
        };
        if sha256(&bytes) != expected {
            // A damaged file is dropped and fetched again.
            fs::remove_file(&path).context(path.display())?;
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    /// Stores `bytes` under `key`. A `Sha256` key must match the bytes.
    pub fn put(&self, key: &Key, bytes: &[u8]) -> Result<()> {
        let actual = sha256(bytes);
        let name = match key {
            Key::Sha256(hex) if !hex.eq_ignore_ascii_case(&actual) => {
                return Err(Error::new(
                    Code::Format,
                    format!("SHA-256 is {actual}, expected {hex}"),
                ));
            }
            Key::Sha256(hex) => format!("sha256-{}", hex.to_ascii_lowercase()),
            Key::Origin(origin) => format!("key-{}-{actual}", sha256(origin.as_bytes())),
        };
        // Write beside the target and rename, so a reader never sees half a file.
        let temp = self.files.join(format!(".{name}.{}", std::process::id()));
        let target = self.files.join(&name);
        fs::write(&temp, bytes)
            .and_then(|_| fs::rename(&temp, &target))
            .map_err(|e| {
                let _ = fs::remove_file(&temp);
                Error::from(e).context(target.display())
            })?;
        // An origin has one file: what was stored for it before is replaced.
        if let Key::Origin(origin) = key {
            let prefix = format!("key-{}-", sha256(origin.as_bytes()));
            for old in self.file_entries()? {
                if old.name.starts_with(&prefix) && old.name != name {
                    let _ = fs::remove_file(self.files.join(old.name));
                }
            }
        }
        Ok(())
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut entries = self.file_entries()?;
        let keys = self.dir().join("firmware-keys");
        match fs::symlink_metadata(&keys) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(meta) => {
                private(&meta, true)?;
                for item in fs::read_dir(keys)? {
                    let item = item?;
                    let name = item.file_name().to_string_lossy().into_owned();
                    if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                        continue;
                    }
                    let meta = fs::symlink_metadata(item.path())?;
                    private(&meta, false)?;
                    entries.push(Entry {
                        name: format!("firmware-keys/{name}"),
                        size: meta.len(),
                    });
                }
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn file_entries(&self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.files).context(self.files.display())? {
            let item = item?;
            let name = item.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') {
                entries.push(Entry {
                    name,
                    size: item.metadata()?.len(),
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Deletes every cached file and returns what was deleted.
    pub fn clear(&self) -> Result<Vec<Entry>> {
        let entries = self.entries()?;
        for e in &entries {
            let path = if e.name.starts_with("firmware-keys/") {
                self.dir().join(&e.name)
            } else {
                self.files.join(&e.name)
            };
            fs::remove_file(&path).context(path.display())?;
        }
        Ok(entries)
    }

    fn find(&self, key: &Key) -> Result<Option<PathBuf>> {
        match key {
            Key::Sha256(hex) => {
                let path = self
                    .files
                    .join(format!("sha256-{}", hex.to_ascii_lowercase()));
                Ok(path.is_file().then_some(path))
            }
            Key::Origin(origin) => {
                let prefix = format!("key-{}-", sha256(origin.as_bytes()));
                Ok(self
                    .file_entries()?
                    .into_iter()
                    .find(|e| e.name.starts_with(&prefix))
                    .map(|e| self.files.join(e.name)))
            }
        }
    }
}

/// `~/Library/Caches/orchard2` on macOS; `$XDG_CACHE_HOME/orchard2` or
/// `~/.cache/orchard2` elsewhere.
pub fn default_dir() -> Result<PathBuf> {
    let home = || {
        std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            Error::new(Code::InvalidInput, "HOME is not set").hint("set --cache or ORCHARD_CACHE")
        })
    };
    let base = if cfg!(target_os = "macos") {
        home()?.join("Library/Caches")
    } else {
        match std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
            Some(dir) if dir.is_absolute() => dir,
            _ => home()?.join(".cache"),
        }
    };
    Ok(base.join("orchard2"))
}

/// Checks that a firmware-key cache directory or file is a real one and
/// private to this user.
pub fn private(meta: &std::fs::Metadata, directory: bool) -> Result<()> {
    if (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
        return Err(Error::new(
            Code::InvalidInput,
            "firmware key cache must contain regular files in a real directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(Error::new(
                Code::InvalidInput,
                "firmware key cache must be owner-private",
            ));
        }
    }
    Ok(())
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        (dir, store)
    }

    #[test]
    fn stores_by_hash_and_by_origin() {
        let (_dir, store) = store();
        let hashed = Key::Sha256(sha256(b"one"));
        let origin = Key::Origin("ipsw abc, member Firmware/x".into());
        assert_eq!(store.get(&hashed).unwrap(), None);
        store.put(&hashed, b"one").unwrap();
        store.put(&origin, b"two").unwrap();
        assert_eq!(store.get(&hashed).unwrap().unwrap(), b"one");
        assert_eq!(store.get(&origin).unwrap().unwrap(), b"two");
        assert_eq!(store.get(&Key::Origin("other".into())).unwrap(), None);
        assert_eq!(store.entries().unwrap().len(), 2);
        // New content for an origin takes the place of the old.
        store.put(&origin, b"three").unwrap();
        assert_eq!(store.get(&origin).unwrap().unwrap(), b"three");
        assert_eq!(store.entries().unwrap().len(), 2);
        assert_eq!(store.clear().unwrap().len(), 2);
        assert!(store.entries().unwrap().is_empty());
    }

    #[test]
    fn refuses_wrong_content_and_drops_damaged_files() {
        let (_dir, store) = store();
        let key = Key::Sha256(sha256(b"good"));
        assert_eq!(store.put(&key, b"bad").unwrap_err().code, Code::Format);
        store.put(&key, b"good").unwrap();
        let path = store.files.join(&store.entries().unwrap()[0].name);
        fs::write(&path, b"damaged").unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
        assert!(store.entries().unwrap().is_empty());

        let origin = Key::Origin("x".into());
        store.put(&origin, b"data").unwrap();
        let path = store.files.join(&store.entries().unwrap()[0].name);
        fs::write(&path, b"damaged").unwrap();
        assert_eq!(store.get(&origin).unwrap(), None);
    }
}

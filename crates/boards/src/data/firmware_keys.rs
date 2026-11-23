//! Automatic private key cache, indexed by IPSW, member and image kind.
//! The schema-1 JSON inventory is only an explicitly selected override.
use orchard_cache::store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::crypto::{self, Key};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Document {
    schema: u32,
    ipsws: BTreeMap<String, BTreeMap<String, Entry>>,
}

#[derive(Deserialize)]
struct Entry {
    kind: String,
    key: String,
    #[serde(default)]
    iv: String,
}

pub struct Keys {
    path: PathBuf,
    document: Option<Document>,
    explicit: bool,
    cache: PathBuf,
    staged: BTreeMap<PathBuf, Vec<u8>>,
}

impl Keys {
    pub fn new(path: Option<&Path>, cache: &Path) -> Self {
        let path = path
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("ORCHARD_FIRMWARE_KEYS").map(PathBuf::from));
        Self {
            explicit: path.is_some(),
            path: path.unwrap_or_default(),
            cache: cache.join("firmware-keys"),
            document: None,
            staged: BTreeMap::new(),
        }
    }

    pub fn get(&mut self, ipsw: &str, member: &str, kind: &str) -> Result<Key> {
        self.lookup(ipsw, member, kind)?.ok_or_else(|| {
            Error::new(Code::NotFound, "no firmware key")
                .context(member)
                .hint("use a compatible device to acquire keys, or supply --keys FILE")
        })
    }

    fn location(&self, ipsw: &str, member: &str, kind: &str) -> PathBuf {
        let identity = serde_json::to_vec(&(ipsw, member, kind)).expect("strings serialize");
        self.cache.join(store::sha256(&identity))
    }

    /// An explicit file is authoritative. Automatic misses are returned to the workflow.
    pub fn lookup(&mut self, ipsw: &str, member: &str, kind: &str) -> Result<Option<Key>> {
        if !self.explicit {
            let path = self.location(ipsw, member, kind);
            if let Some(bytes) = self.staged.get(&path) {
                return decode(bytes).map(Some);
            }
            match std::fs::symlink_metadata(&self.cache) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
                Ok(meta) => store::private(&meta, true)?,
            }
            return match read_private(&path, 64) {
                Ok(bytes) => decode(&bytes).map(Some),
                Err(e) if e.code == Code::NotFound => Ok(None),
                Err(e) => Err(e),
            };
        }
        if self.document.is_none() {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options
                .open(&self.path)
                .context(self.path.display())
                .map_err(|e| e.hint("set --keys or ORCHARD_FIRMWARE_KEYS to firmware-keys.json"))?;
            let meta = file.metadata().context(self.path.display())?;
            if !meta.is_file() || meta.len() > 1 << 20 {
                return Err(Error::new(
                    Code::InvalidInput,
                    "expected a regular key file under 1 MiB",
                )
                .context(self.path.display()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
                    return Err(Error::new(
                        Code::InvalidInput,
                        "key file must be owned by this user with permissions 0600",
                    )
                    .context(self.path.display()));
                }
            }
            let bytes = crate::firmware_build::read_capped(file, 1 << 20, &self.path)?;
            let document: Document = serde_json::from_slice(&bytes).map_err(|_| {
                Error::new(
                    Code::Format,
                    "invalid firmware key file (values suppressed)",
                )
                .context(self.path.display())
            })?;
            if document.schema != 1 {
                return Err(Error::new(Code::Format, "unsupported firmware key schema")
                    .context(self.path.display()));
            }
            self.document = Some(document);
        }
        let entry = self
            .document
            .as_ref()
            .unwrap()
            .ipsws
            .get(ipsw)
            .and_then(|e| e.get(member))
            .ok_or_else(|| Error::new(Code::NotFound, "no firmware key").context(member))?;
        let key = crypto::hex(&entry.key);
        let iv = crypto::hex(&entry.iv);
        match (key, iv) {
            (Ok(key), Ok(iv))
                if entry.kind == kind && matches!(key.len(), 16 | 24 | 32) && iv.len() == 16 =>
            {
                Ok(Some(Key {
                    key,
                    iv: iv.try_into().unwrap(),
                }))
            }
            _ => Err(Error::new(Code::Format, "invalid firmware key").context(member)),
        }
    }
}

impl Keys {
    /// Hold acquired keys in memory until all output checks pass.
    pub fn stage(&mut self, ipsw: &str, member: &str, kind: &str, key: &Key) -> Result<()> {
        let mut bytes = vec![1, key.key.len() as u8];
        bytes.extend(key.iv);
        bytes.extend(&key.key);
        decode(&bytes)?;
        self.staged.insert(self.location(ipsw, member, kind), bytes);
        Ok(())
    }

    /// One immutable, owner-private file per key. Concurrent identical writers
    /// agree; different keys for the same input are refused, never overwritten.
    pub fn commit(&mut self) -> Result<()> {
        if self.staged.is_empty() {
            return Ok(());
        }
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&self.cache) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        store::private(&std::fs::symlink_metadata(&self.cache)?, true)?;
        for (path, bytes) in &self.staged {
            use std::io::Write;
            let mut temp = tempfile::NamedTempFile::new_in(&self.cache)?;
            temp.write_all(bytes)?;
            temp.as_file().sync_all()?;
            if let Err(error) = temp.persist_noclobber(path) {
                if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.error.into());
                }
                if read_private(path, 64)? != *bytes {
                    return Err(Error::new(
                        Code::Format,
                        "conflicting cached firmware key (values suppressed)",
                    ));
                }
            }
        }
        self.staged.clear();
        Ok(())
    }
}

fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new(Code::NotFound, "firmware key is not cached")
        } else {
            Error::from(e)
        }
    })?;
    store::private(&file.metadata()?, false)?;
    crate::firmware_build::read_capped(file, limit, path)
}

fn decode(bytes: &[u8]) -> Result<Key> {
    if bytes.len() < 2
        || bytes[0] != 1
        || !matches!(bytes[1], 16 | 24 | 32)
        || bytes.len() != 18 + usize::from(bytes[1])
    {
        return Err(Error::new(
            Code::Format,
            "invalid cached firmware key (values suppressed)",
        ));
    }
    Ok(Key {
        iv: bytes[2..18].try_into().unwrap(),
        key: bytes[18..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f";
    const IV: &str = "0f0e0d0c0b0a09080706050403020100";

    /// Writes a key file at `path` with the given JSON and 0600 permissions.
    fn write_keys(path: &Path, json: &str) {
        std::fs::write(path, json).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn document() -> String {
        format!(
            r#"{{"schema":1,"ipsws":{{"abc":{{"iBSS":{{"kind":"img3","key":"{KEY}","iv":"{IV}"}}}}}}}}"#
        )
    }

    #[test]
    fn automatic_cache_is_staged_private_and_bound_to_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let key = Key {
            iv: [7; 16],
            key: vec![3; 32],
        };
        let mut keys = Keys::new(None, dir.path());
        assert!(keys.lookup("a", "b", "img3").unwrap().is_none());
        keys.stage("a", "b", "img3", &key).unwrap();
        assert!(!dir.path().join("firmware-keys").exists());
        assert_eq!(keys.get("a", "b", "img3").unwrap().key, key.key);
        keys.commit().unwrap();
        let mut other = Keys::new(None, dir.path());
        assert_eq!(other.get("a", "b", "img3").unwrap().iv, key.iv);
        for (ipsw, member, kind) in [("c", "b", "img3"), ("a", "c", "img3"), ("a", "b", "im4p")] {
            assert!(other.lookup(ipsw, member, kind).unwrap().is_none());
        }
        other.stage("a", "b", "img3", &key).unwrap();
        other.commit().unwrap();
        other
            .stage(
                "a",
                "b",
                "img3",
                &Key {
                    iv: [8; 16],
                    key: vec![4; 32],
                },
            )
            .unwrap();
        assert!(other.commit().unwrap_err().message.contains("conflicting"));
    }

    #[test]
    fn concurrent_private_writers_preserve_unrelated_keys_and_clear_lists_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = store::Store::open(Some(dir.path())).unwrap();
        std::thread::scope(|s| {
            for member in ["one", "two", "one"] {
                let root = dir.path();
                s.spawn(move || {
                    let mut keys = Keys::new(None, root);
                    keys.stage(
                        "fw",
                        member,
                        "im4p",
                        &Key {
                            iv: [8; 16],
                            key: vec![9; 32],
                        },
                    )
                    .unwrap();
                    keys.commit().unwrap();
                });
            }
        });
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.name.starts_with("firmware-keys/")));
        assert_eq!(store.clear().unwrap(), entries);
        assert!(Keys::new(None, dir.path())
            .lookup("fw", "one", "im4p")
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn automatic_cache_refuses_links_permissions_and_corruption() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let mut keys = Keys::new(None, dir.path());
        keys.stage(
            "fw",
            "one",
            "im4p",
            &Key {
                iv: [0; 16],
                key: vec![1; 32],
            },
        )
        .unwrap();
        keys.commit().unwrap();
        let path = keys.location("fw", "one", "im4p");
        let check = || {
            Keys::new(None, dir.path())
                .lookup("fw", "one", "im4p")
                .is_err()
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"secret malformed content").unwrap();
        let error = Keys::new(None, dir.path())
            .lookup("fw", "one", "im4p")
            .err()
            .unwrap();
        assert!(!error.message.contains("secret malformed"));
        std::fs::remove_file(&path).unwrap();
        symlink("/dev/null", &path).unwrap();
        assert!(check());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(dir.path().join("firmware-keys")).unwrap();
        symlink(dir.path(), dir.path().join("firmware-keys")).unwrap();
        assert!(check());
    }

    #[test]
    fn reads_a_key_by_ipsw_and_member() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware-keys.json");
        write_keys(&path, &document());
        let mut keys = Keys::new(Some(&path), dir.path());
        let key = keys.get("abc", "iBSS", "img3").unwrap();
        assert_eq!(key.key, crypto::hex(KEY).unwrap());
        assert_eq!(key.iv, crypto::hex(IV).unwrap().as_slice());
        // A member or IPSW that is not present is NotFound.
        assert_eq!(
            keys.get("abc", "iBEC", "img3")
                .map(|_| ())
                .unwrap_err()
                .code,
            Code::NotFound
        );
        assert_eq!(
            keys.get("xyz", "iBSS", "img3")
                .map(|_| ())
                .unwrap_err()
                .code,
            Code::NotFound
        );
        // The wrong kind for the entry is a Format error.
        assert_eq!(
            keys.get("abc", "iBSS", "im4p")
                .map(|_| ())
                .unwrap_err()
                .code,
            Code::Format
        );
    }

    #[test]
    fn rejects_an_unsupported_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware-keys.json");
        write_keys(&path, r#"{"schema":2,"ipsws":{}}"#);
        let e = Keys::new(Some(&path), dir.path())
            .get("abc", "iBSS", "img3")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(e.code, Code::Format);
    }

    #[test]
    fn reports_a_parse_error_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware-keys.json");
        write_keys(&path, "not json");
        let e = Keys::new(Some(&path), dir.path())
            .get("abc", "iBSS", "img3")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(e.code, Code::Format);
        assert!(e.message.contains("invalid firmware key file"));
    }

    #[test]
    fn missing_file_points_at_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.json");
        let e = Keys::new(Some(&path), dir.path())
            .get("abc", "iBSS", "img3")
            .map(|_| ())
            .unwrap_err();
        assert!(e.hint.unwrap().contains("ORCHARD_FIRMWARE_KEYS"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware-keys.json");
        std::fs::write(&path, document()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = Keys::new(Some(&path), dir.path())
            .get("abc", "iBSS", "img3")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
    }
}

//! orchard's loaders (m1n1, Pongo and Cascadia's) as its builds publish
//! them.
//!
//! Every orchard build publishes the loaders with a key each: a hash of the
//! files the loader is built from. The key is the loader's version. A new
//! build of unchanged sources carries the same key, so a file that is here
//! for a key is kept, whatever build names it next. A loader can be several
//! files under its one key: m1n1 comes as the plain binary Pongo starts and
//! as the Mach-O a patched iBoot starts, with the monitor stub.
//!
//! Which keys are current is a small record in the cache directory
//! (`data/loaders.json`). Like the board data it is fetched when there is
//! none and refreshed by `data update`; using a device never asks for a
//! newer one. A loader's file is fetched when the record names a key that is
//! not in the cache yet.
use orchard_cache::http;
use orchard_cache::store::{self, Key, Store};
use orchard_cache::{Code, Context, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASES: &str = "https://github.com/OrchardLinux/orchard/releases/download";
/// The release whose `latest.json` names the newest build.
const POINTER: &str = "nightly";
const SUMS: &str = "SHA256SUMS";
const KEYS: &str = "component-keys.json";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TEXT: u64 = 1 << 20;
const MAX_LOADER: u64 = 64 << 20;

/// One published loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loader {
    /// The hash of what the loader is built from: its version.
    pub key: String,
    /// The loader's files, all built from that key.
    pub files: Vec<File>,
}

/// One file of a loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    /// The file's name in the build that published it.
    pub asset: String,
    pub sha256: String,
}

/// The loaders orchard2 takes from orchard's builds.
const LOADERS: [&str; 3] = ["m1n1", "pongo", "cascadia"];

/// Which loaders are current, and the build that names them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The orchard build the record was read from, such as `nightly-…`.
    pub release: String,
    /// When the record was last fetched, in Unix seconds.
    pub checked: u64,
    /// By name: `m1n1`, `pongo`, `cascadia`.
    pub loaders: BTreeMap<String, Loader>,
}

#[derive(Deserialize)]
struct Pointer {
    name: String,
}

#[derive(Deserialize)]
struct KeysFile {
    schema: u32,
    #[serde(default)]
    shared: BTreeMap<String, Component>,
}

#[derive(Deserialize)]
struct Component {
    key: String,
    asset: String,
    /// All files of the key, where there are more than `asset`.
    #[serde(default)]
    files: Vec<String>,
}

/// A name that is safe as one part of a URL and of a path.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// The record a build's `SHA256SUMS` and `component-keys.json` give. The
/// keys file counts only if the checksums list it with its hash.
pub fn record(release: &str, sums: &str, keys: &[u8], checked: u64) -> Result<Record> {
    let listed = |file: &str| {
        sums.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            let (hash, name) = (parts.next()?, parts.next()?);
            (name.trim_start_matches("./") == file && hash.len() == 64).then_some(hash)
        })
    };
    if listed(KEYS) != Some(store::sha256(keys).as_str()) {
        return Err(Error::new(
            Code::Format,
            format!("{KEYS} does not match the build's {SUMS}"),
        ));
    }
    let file: KeysFile =
        serde_json::from_slice(keys).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    crate::catalog::check_schema(file.schema, 1)?;
    let mut loaders = BTreeMap::new();
    for name in LOADERS {
        let Some(component) = file.shared.get(name) else {
            continue;
        };
        let assets = match component.files.is_empty() {
            true => std::slice::from_ref(&component.asset),
            false => component.files.as_slice(),
        };
        let mut files = Vec::new();
        for asset in assets {
            let Some(sha256) = listed(asset).filter(|_| plain(asset)) else {
                return Err(Error::new(
                    Code::Format,
                    format!("the build's {SUMS} does not list {name}'s file {asset}"),
                ));
            };
            files.push(File {
                asset: asset.clone(),
                sha256: sha256.to_owned(),
            });
        }
        loaders.insert(
            name.to_owned(),
            Loader {
                key: component.key.clone(),
                files,
            },
        );
    }
    Ok(Record {
        release: release.to_owned(),
        checked,
        loaders,
    })
}

/// The record in the cache directory, if there is one.
pub fn existing(store: &Store) -> Option<Record> {
    let text = fs::read_to_string(path(store)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The record to use: the one that is here, however old, or the newest
/// build's when there is none yet.
pub fn resolve(store: &Store) -> Result<Record> {
    match existing(store) {
        Some(record) => Ok(record),
        None => update(store),
    }
}

/// Reads the newest build's record now.
pub fn update(store: &Store) -> Result<Record> {
    let pointer = get(&format!("{RELEASES}/{POINTER}/latest.json"), MAX_TEXT)?;
    let pointer: Pointer = serde_json::from_slice(&pointer)
        .map_err(|e| Error::new(Code::Format, e.to_string()))
        .context("latest.json")?;
    if !plain(&pointer.name) {
        return Err(Error::new(Code::Format, "latest.json names no build"));
    }
    let base = format!("{RELEASES}/{}", pointer.name);
    let sums = get(&format!("{base}/{SUMS}"), MAX_TEXT)?;
    let keys = get(&format!("{base}/{KEYS}"), MAX_TEXT)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let record = record(&pointer.name, &String::from_utf8_lossy(&sums), &keys, now)
        .context(&pointer.name)?;
    let path = path(store);
    let temp = path.with_extension(format!("json.{}", std::process::id()));
    fs::create_dir_all(path.parent().expect("the record has a directory"))
        .and_then(|()| {
            fs::write(
                &temp,
                serde_json::to_vec(&record).expect("record serializes"),
            )
        })
        .and_then(|()| fs::rename(&temp, &path))
        .context(path.display())?;
    Ok(record)
}

/// The file `asset` of the loader `name`: from the cache when it is there
/// for the loader's key, else fetched from the build the record names and
/// checked against it.
pub fn file(store: &Store, record: &Record, name: &str, asset: &str) -> Result<Vec<u8>> {
    file_with(store, record, name, asset, |url| get(url, 256 << 20))
}

fn file_with(
    store: &Store,
    record: &Record,
    name: &str,
    asset: &str,
    mut fetch: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let loader = record.loaders.get(name).ok_or_else(|| {
        Error::new(
            Code::NotFound,
            format!("orchard's build {} publishes no {name}", record.release),
        )
        .hint("`orchard2 data update` reads the newest build")
    })?;
    let key = Key::Origin(format!("loader {name} {} {asset}", loader.key));
    if let Some(bytes) = store.get(&key)? {
        return Ok(bytes);
    }
    let base = format!("{RELEASES}/{}", record.release);
    let bytes = if let Some(file) = loader.files.iter().find(|f| f.asset == asset) {
        let url = format!("{base}/{}", file.asset);
        let bytes = fetch(&url)?;
        if store::sha256(&bytes) != file.sha256 {
            return Err(
                Error::new(Code::Format, "the file does not match the build's checksum")
                    .context(&url),
            );
        }
        bytes
    } else if name == "m1n1" && matches!(asset, "m1n1-idevice.macho" | "monitor-stub.macho") {
        // These outputs were built into the shared tools archive before CI
        // also exposed them as standalone assets. Use that same build.
        let sums = fetch(&format!("{base}/{SUMS}"))?;
        let sums = std::str::from_utf8(&sums)
            .map_err(|_| Error::new(Code::Format, "SHA256SUMS is not UTF-8"))?;
        let expected = sums
            .lines()
            .find_map(|line| {
                let mut parts = line.split_whitespace();
                let (hash, path) = (parts.next()?, parts.next()?);
                (path.trim_start_matches("./") == "tool-components.tar.gz").then_some(hash)
            })
            .ok_or_else(|| Error::new(Code::NotFound, "the build has no shared tools archive"))?;
        let archive = fetch(&format!("{base}/tool-components.tar.gz"))?;
        if store::sha256(&archive) != expected {
            return Err(Error::new(
                Code::Format,
                "the shared tools archive does not match the build's checksum",
            ));
        }
        bundled_loader(&archive, asset)?
    } else {
        return Err(Error::new(
            Code::NotFound,
            format!(
                "orchard's build {} publishes no {asset} for {name}",
                record.release
            ),
        ));
    };
    if bytes.len() as u64 > MAX_LOADER {
        return Err(Error::new(Code::Format, "published loader exceeds 64 MiB"));
    }
    store.put(&key, &bytes)?;
    Ok(bytes)
}

fn bundled_loader(compressed: &[u8], asset: &str) -> Result<Vec<u8>> {
    const LIMIT: u64 = 512 << 20;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(compressed)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::new(
            Code::Format,
            "shared tools archive exceeds 512 MiB",
        ));
    }
    let archive = orchard_formats::tar::read(&bytes)?;
    let path = format!("components/quiet/{asset}");
    let files: Vec<_> = archive.files.iter().filter(|f| f.path == path).collect();
    match files.as_slice() {
        [file] => Ok(file.data.to_vec()),
        _ => Err(Error::new(
            Code::Format,
            format!("shared tools archive needs exactly one {path}"),
        )),
    }
}

fn path(store: &Store) -> std::path::PathBuf {
    store.dir().join("data/loaders.json")
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    let network = |e: ureq::Error| Error::new(Code::Network, e.to_string()).context(url);
    let mut response = http::agent(url, TIMEOUT)?
        .get(url)
        .call()
        .map_err(network)?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(network)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools_archive() -> Vec<u8> {
        use std::io::Write;
        let mut tar = Vec::new();
        for (name, data) in [
            ("components/quiet/m1n1-idevice.macho", b"MACHO".as_slice()),
            ("components/quiet/monitor-stub.macho", b"STUB".as_slice()),
        ] {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            header[148..156].fill(b' ');
            header[156] = b'0';
            let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
            tar.extend(header);
            tar.extend(data);
            tar.resize(tar.len().next_multiple_of(512), 0);
        }
        tar.resize(tar.len() + 1024, 0);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn companion_from_the_same_published_archive_is_verified_and_cached() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temp.path())).unwrap();
        let mut record = sample();
        record
            .loaders
            .get_mut("m1n1")
            .unwrap()
            .files
            .retain(|f| f.asset == "m1n1.bin");
        let archive = tools_archive();
        let sums = format!("{}  ./tool-components.tar.gz\n", store::sha256(&archive));
        let mut requested = Vec::new();
        let got = file_with(&store, &record, "m1n1", "m1n1-idevice.macho", |url| {
            requested.push(url.to_owned());
            Ok(if url.ends_with(SUMS) {
                sums.as_bytes().to_vec()
            } else {
                archive.clone()
            })
        })
        .unwrap();
        assert_eq!(got, b"MACHO");
        assert_eq!(requested.len(), 2);
        assert!(requested.iter().all(|url| url.contains("/nightly-1/")));
        assert_eq!(
            file_with(&store, &record, "m1n1", "m1n1-idevice.macho", |_| panic!(
                "cached"
            ))
            .unwrap(),
            b"MACHO"
        );
        let error = file_with(&store, &record, "m1n1", "monitor-stub.macho", |url| {
            Ok(if url.ends_with(SUMS) {
                sums.as_bytes().to_vec()
            } else {
                b"changed".to_vec()
            })
        })
        .unwrap_err();
        assert_eq!(error.code, Code::Format);
        assert!(bundled_loader(&archive, "missing.macho").is_err());
    }

    const KEYS_FILE: &str = r#"{"schema": 1, "platforms": {}, "shared": {
        "m1n1": {"key": "aa11", "asset": "m1n1.bin",
                 "files": ["m1n1.bin", "m1n1-idevice.macho", "monitor-stub.macho"]},
        "pongo": {"key": "bb22", "asset": "pongo.bin"},
        "cascadia": {"key": "dd44", "asset": "cascadia-x1.bin"},
        "tools": {"key": "cc33", "asset": "tool-components.tar.gz"}}}"#;

    fn sums(m1n1: &[u8], pongo: &[u8], keys: &[u8]) -> String {
        format!(
            "{}  ./m1n1.bin\n{}  ./pongo.bin\n{}  ./component-keys.json\n\
             {}  ./m1n1-idevice.macho\n{}  ./monitor-stub.macho\n{}  ./cascadia-x1.bin\n",
            store::sha256(m1n1),
            store::sha256(pongo),
            store::sha256(keys),
            store::sha256(b"MACHO"),
            store::sha256(b"STUB"),
            store::sha256(b"CASCADIA"),
        )
    }

    fn sample() -> Record {
        let keys = KEYS_FILE.as_bytes();
        record("nightly-1", &sums(b"M1N1", b"PONGO", keys), keys, 7).unwrap()
    }

    #[test]
    fn reads_the_loaders_with_key_files_and_checksums() {
        let record = sample();
        assert_eq!(record.release, "nightly-1");
        let names: Vec<&str> = record.loaders.keys().map(String::as_str).collect();
        assert_eq!(names, ["cascadia", "m1n1", "pongo"]);
        // m1n1 is three files under one key.
        let m1n1 = &record.loaders["m1n1"];
        assert_eq!(m1n1.key, "aa11");
        let assets: Vec<&str> = m1n1.files.iter().map(|f| f.asset.as_str()).collect();
        assert_eq!(
            assets,
            ["m1n1.bin", "m1n1-idevice.macho", "monitor-stub.macho"]
        );
        assert_eq!(m1n1.files[0].sha256, store::sha256(b"M1N1"));
        assert_eq!(m1n1.files[2].sha256, store::sha256(b"STUB"));
        // A loader with one file names it as its asset.
        let pongo = &record.loaders["pongo"];
        assert_eq!(pongo.files.len(), 1);
        assert_eq!(pongo.files[0].sha256, store::sha256(b"PONGO"));
        assert_eq!(record.loaders["cascadia"].files[0].asset, "cascadia-x1.bin");
    }

    #[test]
    fn each_file_of_a_loader_is_cached_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        let record = sample();
        let mut asked = Vec::new();
        for (asset, bytes) in [
            ("m1n1-idevice.macho", b"MACHO".as_slice()),
            ("monitor-stub.macho", b"STUB"),
        ] {
            let got = file_with(&store, &record, "m1n1", asset, |url| {
                asked.push(url.rsplit('/').next().unwrap().to_owned());
                Ok(bytes.to_vec())
            });
            assert_eq!(got.unwrap(), bytes);
        }
        assert_eq!(asked, ["m1n1-idevice.macho", "monitor-stub.macho"]);
        // Both are here now; the plain binary of the same key is not.
        let again = file_with(
            &store,
            &record,
            "m1n1",
            "monitor-stub.macho",
            |_| unreachable!(),
        );
        assert_eq!(again.unwrap(), b"STUB");
        let plain = file_with(
            &store,
            &record,
            "m1n1",
            "m1n1.bin",
            |_| Ok(b"M1N1".to_vec()),
        );
        assert_eq!(plain.unwrap(), b"M1N1");

        // A file the loader does not have.
        let e = file_with(&store, &record, "pongo", "pongo.macho", |_| unreachable!()).unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert_eq!(
            e.message,
            "orchard's build nightly-1 publishes no pongo.macho for pongo"
        );
    }

    #[test]
    fn a_build_without_a_loaders_key_has_no_such_loader() {
        let keys = br#"{"schema": 1, "shared": {"m1n1": {"key": "aa11", "asset": "m1n1.bin"}}}"#;
        let record = record("nightly-1", &sums(b"M1N1", b"PONGO", keys), keys, 0).unwrap();
        assert!(!record.loaders.contains_key("pongo"));
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        let e = file_with(&store, &record, "pongo", "pongo.bin", |_| unreachable!()).unwrap_err();
        assert_eq!(e.code, Code::NotFound);
    }

    #[test]
    fn keys_the_checksums_do_not_cover_are_refused() {
        let keys = KEYS_FILE.as_bytes();
        let other = sums(b"M1N1", b"PONGO", b"other keys");
        assert_eq!(record("n", &other, keys, 0).unwrap_err().code, Code::Format);
        // A loader file the checksums do not list, or one outside the build.
        let only = format!("{}  ./component-keys.json\n", store::sha256(keys));
        assert_eq!(record("n", &only, keys, 0).unwrap_err().code, Code::Format);
        let keys = br#"{"schema": 1, "shared": {"m1n1": {"key": "k", "asset": "../m1n1.bin"}}}"#;
        let sums = format!(
            "{}  ./component-keys.json\n{}  ../m1n1.bin\n",
            store::sha256(keys),
            store::sha256(b"M1N1")
        );
        assert_eq!(record("n", &sums, keys, 0).unwrap_err().code, Code::Format);
    }

    #[test]
    fn a_file_is_fetched_once_per_key_whatever_build_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        let first = sample();
        let mut asked = Vec::new();
        let bytes = file_with(&store, &first, "m1n1", "m1n1.bin", |url| {
            asked.push(url.to_owned());
            Ok(b"M1N1".to_vec())
        });
        assert_eq!(bytes.unwrap(), b"M1N1");
        assert_eq!(
            asked,
            ["https://github.com/OrchardLinux/orchard/releases/download/nightly-1/m1n1.bin"]
        );

        // A later build with the same key, even with other bytes: nothing is fetched.
        let mut later = first.clone();
        later.release = "nightly-2".into();
        later.loaders.get_mut("m1n1").unwrap().files[0].sha256 = store::sha256(b"REBUILT");
        let bytes = file_with(&store, &later, "m1n1", "m1n1.bin", |_| {
            unreachable!("the key is cached")
        });
        assert_eq!(bytes.unwrap(), b"M1N1");

        // Another key is another version.
        later.loaders.get_mut("m1n1").unwrap().key = "dd44".into();
        let bytes = file_with(&store, &later, "m1n1", "m1n1.bin", |_| {
            Ok(b"REBUILT".to_vec())
        });
        assert_eq!(bytes.unwrap(), b"REBUILT");
    }

    #[test]
    fn a_file_that_does_not_match_its_checksum_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        let record = sample();
        let e = file_with(&store, &record, "pongo", "pongo.bin", |_| {
            Ok(b"OTHER".to_vec())
        })
        .unwrap_err();
        assert_eq!(e.code, Code::Format);
        // The next try fetches again.
        let bytes = file_with(&store, &record, "pongo", "pongo.bin", |_| {
            Ok(b"PONGO".to_vec())
        });
        assert_eq!(bytes.unwrap(), b"PONGO");
    }
}

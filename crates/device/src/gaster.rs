//! The outside tool the device crate runs itself: gaster's worker, which
//! enters the boot ROM and unwraps firmware keys on the device. Which
//! build is run is pinned here by size and hash.
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::store::{Key, Store};
use orchard_cache::{Code, Context, Error, Result};
use std::path::PathBuf;
use std::time::Duration;

/// A release file of the worker for one host, as this version accepts it.
struct Pin {
    os: &'static str,
    arch: &'static str,
    asset: &'static str,
    size: usize,
    hash: &'static str,
}

/// Orchard's external worker for entry and firmware key acquisition,
/// orchard-worker-v0.1.3: sizes and checksums from the release's
/// `SHA256SUMS`.
const RELEASE: &str = "https://github.com/OrchardLinux/gaster/releases/download/orchard-worker-v0.1.3";
const PINS: &[Pin] = &[
    Pin {
        os: "macos",
        arch: "aarch64",
        asset: "gaster-worker-macos-aarch64",
        size: 90432,
        hash: "2c81aa131656cbbe322e97d90aa72630678fbe7935d2cffc2b6b42ba606eac5f",
    },
    Pin {
        os: "macos",
        arch: "x86_64",
        asset: "gaster-worker-macos-x86_64",
        size: 57408,
        hash: "726e74adda52f837a8cf1362ce80307a146eb4e78f21908635f434c9cebec7e2",
    },
    Pin {
        os: "linux",
        arch: "aarch64",
        asset: "gaster-worker-linux-aarch64",
        size: 35648,
        hash: "d7d807b32c5ec7fc99e1b6dfb7eba46347f105388b3e6e6f455db30ac64b1e80",
    },
    Pin {
        os: "linux",
        arch: "x86_64",
        asset: "gaster-worker-linux-x86_64",
        size: 38600,
        hash: "7f5fb3acac5faeeb8a6d33b6299cf767909eaa1f1ea7b8464d935781d8161b3b",
    },
];


/// A pinned tool, ready to run.
pub struct Executable {
    pub bytes: Vec<u8>,
    /// Where the tool is written out to run. It is inside the cache, since
    /// the system's temporary directory can be mounted without the right to
    /// run files.
    pub dir: PathBuf,
}

/// How long the download may stall.
const FETCH: Duration = Duration::from_secs(30);

fn pin(os: &str, arch: &str) -> Result<&'static Pin> {
    PINS.iter()
        .find(|pin| pin.os == os && pin.arch == arch)
        .ok_or_else(|| Error::new(Code::Unsupported, format!("gaster: no pinned worker release for {os} {arch}")))
}

/// The pinned file from the cache, or from `fetch` when the cache lacks it.
/// Only bytes with the pin's size and hash are kept and returned.
fn cached(store: &Store, pin: &Pin, fetch: impl FnOnce() -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    let key = Key::Sha256(pin.hash.into());
    let (bytes, fetched) = match store.get(&key)? {
        Some(bytes) => (bytes, false),
        None => (fetch()?, true),
    };
    if bytes.len() != pin.size {
        return Err(Error::new(Code::Format, "gaster: wrong file size"));
    }
    if fetched {
        store.put(&key, &bytes).context("gaster")?;
    }
    Ok(bytes)
}

/// gaster's worker for this host, from the cache or its release.
pub fn worker(store: &Store) -> Result<Executable> {
    let pin = pin(std::env::consts::OS, std::env::consts::ARCH)?;
    let bytes = cached(store, pin, || {
        let url = format!("{RELEASE}/{}", pin.asset);
        Http::new(&url, pin.size as u64, FETCH)?.fetch(0, pin.size)
    })?;
    let dir = store.dir().join("run");
    std::fs::create_dir_all(&dir).context(dir.display())?;
    Ok(Executable { bytes, dir })
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
    fn each_host_gets_its_own_build_and_another_is_named() {
        for (os, arch) in [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "aarch64"), ("linux", "x86_64")] {
            assert_eq!(pin(os, arch).unwrap().asset, format!("gaster-worker-{os}-{arch}"));
        }
        assert!(pin("windows", "x86_64").is_err_and(|e| e.code == Code::Unsupported));
    }

    #[test]
    fn a_download_is_kept_only_with_the_pinned_size_and_hash() {
        let (_dir, store) = store();
        let pin = pin("macos", "aarch64").unwrap();
        // The right size and the wrong content: the store refuses the hash.
        assert!(cached(&store, pin, || Ok(vec![0; pin.size])).is_err());
        assert!(cached(&store, pin, || Ok(vec![0; 3])).is_err());
        assert!(store.get(&Key::Sha256(pin.hash.into())).unwrap().is_none());
    }
}

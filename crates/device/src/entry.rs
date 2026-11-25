//! Entries: the outside tools that take a device from a state anyone can
//! reach to one that accepts our code. Which build of a tool is run is
//! pinned here by size and hash.
use crate::host::{Connect, Tool, ToolArg};
use crate::steps::{Outcome, Run};
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::store::{Key, Store};
use orchard_cache::{Code, Context, Error, Result};
use std::path::PathBuf;
use std::time::Duration;

/// A release file of a tool for one host, as this version of orchard2
/// accepts it.
struct Pin {
    os: &'static str,
    arch: &'static str,
    asset: &'static str,
    size: usize,
    hash: &'static str,
}

/// How an entry is obtained and run.
enum How {
    /// A tool downloaded and pinned per host, run through the session to
    /// upload Pongo over the boot ROM exploit. `args`' `File(0)` is Pongo.
    Pongo {
        /// Where the tool's release files are.
        release: &'static str,
        pins: &'static [Pin],
        args: &'static [ToolArg<'static>],
    },
    /// The external gaster worker, run in its own session-owned process.
    Gaster {
        release: &'static str,
        pins: &'static [Pin],
    },
}

/// An entry's tool and how it is run. Which states it links is orchard's
/// data; the method table says which of them this version runs.
pub struct Entry {
    pub name: &'static str,
    how: How,
}

/// palera1n v2.4: the release and checksums of orchard's `tools.py`. macOS
/// has one build for both architectures.
pub const PALERA1N: Entry = Entry {
    name: "palera1n",
    how: How::Pongo {
        release: "https://github.com/palera1n/palera1n/releases/download/v2.4",
        pins: &[
            Pin {
                os: "macos",
                arch: "aarch64",
                asset: "palera1n-macos-universal",
                size: 9757024,
                hash: "dcf2f827e89a12215685c55f0462cd7d0cbc0551e3acd340bf0216c274c2e83e",
            },
            Pin {
                os: "macos",
                arch: "x86_64",
                asset: "palera1n-macos-universal",
                size: 9757024,
                hash: "dcf2f827e89a12215685c55f0462cd7d0cbc0551e3acd340bf0216c274c2e83e",
            },
            Pin {
                os: "linux",
                arch: "aarch64",
                asset: "palera1n-linux-arm64",
                size: 5059168,
                hash: "0ddcc3e703709f9cf9a85eb7c598f7500e2028fc4c60f3085eecb0b2bcbb138b",
            },
            Pin {
                os: "linux",
                arch: "x86_64",
                asset: "palera1n-linux-x86_64",
                size: 5042624,
                hash: "ea531df933c0c8edab25ecb8f43faedd010177484482bb3744eaa4cf1cd48658",
            },
        ],
        args: &[ToolArg::Text("-p"), ToolArg::Text("-k"), ToolArg::File(0)],
    },
};

/// checkra1n 0.1337.3, the official build: the release and checksums of
/// orchard's `tools.py`. macOS has one build for both architectures. The
/// arguments are those of the one recorded boot with it (orchard#59):
/// stop after Pongo's upload (`-E`), a clean Pongo shell (`-p`), and our
/// Pongo in place of the built-in one (`-k`).
pub const CHECKRA1N: Entry = Entry {
    name: "checkra1n",
    how: How::Pongo {
        release: "https://assets.checkra.in/downloads/preview/0.1337.3",
        pins: &[
            Pin {
                os: "macos",
                arch: "aarch64",
                asset: "checkra1n-macos",
                size: 1161528,
                hash: "3bc1a0247dd9782b18de8945e3bb3b2dc740208b8c78f78634b80a7804a9f9be",
            },
            Pin {
                os: "macos",
                arch: "x86_64",
                asset: "checkra1n-macos",
                size: 1161528,
                hash: "3bc1a0247dd9782b18de8945e3bb3b2dc740208b8c78f78634b80a7804a9f9be",
            },
            Pin {
                os: "linux",
                arch: "aarch64",
                asset: "checkra1n-linux-arm64",
                size: 550664,
                hash: "078053c11e9a301f0e71b87b6b854e6c8a69c87972325dd0c90deeeba1c3cb70",
            },
            Pin {
                os: "linux",
                arch: "x86_64",
                asset: "checkra1n-linux-x86_64",
                size: 554232,
                hash: "53154d87594cf67c56659f24547f1b5bb5336f84ea58c2d42f25a7b2df81a964",
            },
        ],
        args: &[
            ToolArg::Text("-E"),
            ToolArg::Text("-p"),
            ToolArg::Text("-v"),
            ToolArg::Text("-n"),
            ToolArg::Text("-k"),
            ToolArg::File(0),
        ],
    },
};

/// Orchard's external worker for entry and firmware key acquisition.
pub const GASTER: Entry = Entry {
    name: "gaster",
    how: How::Gaster {
        release: "https://github.com/OrchardLinux/gaster/releases/download/orchard-worker-v0.1.0",
        pins: &[],
    },
};

/// The entries this version runs.
pub const ENTRIES: &[&Entry] = &[&PALERA1N, &CHECKRA1N, &GASTER];

/// The entry called `name`, when this version runs it.
pub fn named(name: &str) -> Option<&'static Entry> {
    ENTRIES.iter().copied().find(|entry| entry.name == name)
}

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

/// A tool's whole run: the exploit, then Pongo's upload. The Python CLI's
/// limit.
const RUN: Duration = Duration::from_secs(120);

/// How long Pongo may take to show up on USB once the tool has ended.
const ARRIVE: Duration = Duration::from_secs(60);

/// Shown when the tool starts: after the exploit some hosts only see the
/// device again once it is plugged in anew.
const CABLE_HINT: &str = "If the Apple logo stays, reconnect the cable at the device.";

impl Entry {
    /// Whether the entry can be planned for this host without fetching its tool.
    pub fn available(&self) -> Result<()> {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        if matches!(self.how, How::Gaster { pins: [], .. })
            && matches!(os, "macos" | "linux")
            && matches!(arch, "aarch64" | "x86_64")
        {
            return Ok(());
        }
        self.pin(os, arch).map(|_| ())
    }

    /// The tool's release file for a host.
    fn pin(&self, os: &str, arch: &str) -> Result<&'static Pin> {
        let pins = match self.how {
            How::Pongo { pins, .. } | How::Gaster { pins, .. } => pins,
        };
        pins.iter()
            .find(|pin| pin.os == os && pin.arch == arch)
            .ok_or_else(|| {
                if matches!(self.how, How::Gaster { .. }) {
                    Error::new(
                        Code::Unsupported,
                        format!("gaster: no pinned worker release for {os} {arch}"),
                    )
                } else {
                    Error::new(
                        Code::Unsupported,
                        format!("{}: no build for {os} {arch}", self.name),
                    )
                }
            })
    }

    /// The pinned file from the cache, or from `fetch` when the cache lacks
    /// it. Only bytes with the pin's size and hash are kept and returned.
    fn cached(
        &self,
        store: &Store,
        pin: &Pin,
        fetch: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let key = Key::Sha256(pin.hash.into());
        let (bytes, fetched) = match store.get(&key)? {
            Some(bytes) => (bytes, false),
            None => (fetch()?, true),
        };
        if bytes.len() != pin.size {
            return Err(Error::new(
                Code::Format,
                format!("{}: wrong file size", self.name),
            ));
        }
        if fetched {
            store.put(&key, &bytes).context(self.name)?;
        }
        Ok(bytes)
    }

    /// Prepares the tool for this host before device work starts.
    pub fn tool(&self, store: &Store) -> Result<Executable> {
        let release = match self.how {
            How::Pongo { release, .. } | How::Gaster { release, .. } => release,
        };
        let pin = self.pin(std::env::consts::OS, std::env::consts::ARCH)?;
        let bytes = self.cached(store, pin, || {
            let url = format!("{release}/{}", pin.asset);
            Http::new(&url, pin.size as u64, FETCH)?.fetch(0, pin.size)
        })?;
        let dir = store.dir().join("run");
        std::fs::create_dir_all(&dir).context(dir.display())?;
        Ok(Executable { bytes, dir })
    }

    /// Runs the entry under the session's lock: a pinned tool exploits the
    /// boot ROM and uploads `pongo`, then Pongo shows up; the gaster worker
    /// leaves the device in pwned DFU. `tool` is there in a real run.
    pub fn run<D: Connect>(
        &self,
        r: &mut Run<D>,
        tool: Option<&Executable>,
        pongo: &[u8],
    ) -> Result<()> {
        let tool = match tool {
            Some(tool) => tool,
            None if r.planning() => &Executable {
                bytes: Vec::new(),
                dir: PathBuf::new(),
            },
            None => {
                return Err(Error::new(
                    Code::NotFound,
                    format!("{}: the tool was not fetched", self.name),
                ))
            }
        };
        let How::Pongo { args, .. } = &self.how else {
            return r.step("run gaster; verify pwned DFU", |s| {
                s.gaster(None, tool)?;
                Ok((Outcome::Done, ()))
            });
        };
        let mut line = format!("run {}", self.name);
        for arg in *args {
            line.push(' ');
            line.push_str(match arg {
                ToolArg::Text(text) => text,
                ToolArg::File(_) => "pongo.bin",
            });
        }
        r.step_with_hint(line, Some(CABLE_HINT), |s| {
            // Neither tool has an option that names a device (palera1n's
            // manual for v2.4, checkra1n's option list): each takes a
            // device in DFU that it finds.
            s.require_single_dfu(self.name)?;
            let invocation = Tool {
                name: self.name,
                executable: &tool.bytes,
                args,
                files: &[pongo],
                dir: &tool.dir,
            };
            s.entry_tool(&invocation, RUN)?;
            Ok((Outcome::Done, ()))
        })?;
        r.step("wait pongo", |s| {
            s.wait_for(orchard_usb::Mode::Pongo, ARRIVE)?;
            Ok((Outcome::Done, ()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pin for the bytes `tool`.
    const PIN: Pin = Pin {
        os: "test",
        arch: "test",
        asset: "test",
        size: 4,
        hash: "7c9bbe5ec9b3fb774e8fa0f54247e93c34ddf8e5d16fe3073420de0ae81a262d",
    };

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(dir.path())).unwrap();
        (dir, store)
    }

    #[test]
    fn each_host_gets_its_own_build() {
        let asset = |entry: &Entry, os, arch| entry.pin(os, arch).unwrap().asset;
        assert_eq!(
            asset(&PALERA1N, "macos", "aarch64"),
            "palera1n-macos-universal"
        );
        assert_eq!(
            asset(&PALERA1N, "macos", "x86_64"),
            "palera1n-macos-universal"
        );
        assert_eq!(asset(&PALERA1N, "linux", "aarch64"), "palera1n-linux-arm64");
        assert_eq!(asset(&PALERA1N, "linux", "x86_64"), "palera1n-linux-x86_64");
        assert_eq!(asset(&CHECKRA1N, "macos", "aarch64"), "checkra1n-macos");
        assert_eq!(asset(&CHECKRA1N, "macos", "x86_64"), "checkra1n-macos");
        assert_eq!(
            asset(&CHECKRA1N, "linux", "aarch64"),
            "checkra1n-linux-arm64"
        );
        assert_eq!(
            asset(&CHECKRA1N, "linux", "x86_64"),
            "checkra1n-linux-x86_64"
        );
    }

    #[test]
    fn a_host_without_a_build_is_named() {
        for entry in [&PALERA1N, &CHECKRA1N] {
            let error = entry.pin("linux", "arm").err().unwrap();
            assert_eq!(error.code, Code::Unsupported);
            assert_eq!(
                error.message,
                format!("{}: no build for linux arm", entry.name)
            );
        }
    }

    #[test]
    fn an_entry_is_found_by_its_name() {
        assert_eq!(named("checkra1n").unwrap().name, "checkra1n");
        assert_eq!(named("gaster").unwrap().name, "gaster");
        assert!(named("pico-checkm8").is_none());
    }

    #[test]
    fn an_unpinned_worker_is_refused_before_fetching_or_staging() {
        let (_dir, store) = store();
        let worker = Entry {
            name: "gaster",
            how: How::Gaster {
                release: "https://example.invalid",
                pins: &[],
            },
        };
        let error = worker.tool(&store).err().unwrap();
        assert_eq!(error.code, Code::Unsupported);
        assert!(error.message.contains("no pinned worker release"));
        assert!(store.entries().unwrap().is_empty());
        assert!(!store.dir().join("run").exists());
    }

    #[test]
    fn a_download_is_kept_only_with_the_pinned_size_and_hash() {
        let (_dir, store) = store();
        let short = CHECKRA1N
            .cached(&store, &PIN, || Ok(b"bad".to_vec()))
            .unwrap_err();
        assert_eq!(short.message, "checkra1n: wrong file size");
        // The right size, other bytes: the store refuses the hash.
        let wrong_hash = GASTER
            .cached(&store, &PIN, || Ok(b"nope".to_vec()))
            .unwrap_err();
        assert_eq!(wrong_hash.code, Code::Format);
        assert!(wrong_hash.message.contains("SHA-256"));
        assert!(store.entries().unwrap().is_empty());
    }

    #[test]
    fn a_cached_tool_is_fetched_once() {
        let (_dir, store) = store();
        let first = GASTER
            .cached(&store, &PIN, || Ok(b"tool".to_vec()))
            .unwrap();
        let again = GASTER
            .cached(&store, &PIN, || panic!("fetched again"))
            .unwrap();
        assert_eq!((first, again), (b"tool".to_vec(), b"tool".to_vec()));
    }

    #[test]
    fn a_damaged_cached_tool_is_not_returned() {
        let (_dir, store) = store();
        GASTER.cached(&store, &PIN, || Ok(b"tool".to_vec())).unwrap();
        let entry = store.entries().unwrap().pop().unwrap();
        std::fs::write(store.dir().join("files").join(entry.name), b"nope").unwrap();
        let bytes = GASTER.cached(&store, &PIN, || Ok(b"tool".to_vec())).unwrap();
        assert_eq!(bytes, b"tool");
    }
}

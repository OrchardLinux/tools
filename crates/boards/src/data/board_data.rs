//! orchard's board data, fetched from the main branch on GitHub and kept in
//! the cache directory. Used when no local data directory is given.
use orchard_cache::http;
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::tar;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const URL: &str = "https://codeload.github.com/OrchardLinux/orchard/tar.gz/refs/heads/main";
const TIMEOUT: Duration = Duration::from_secs(30);
/// Archives larger than this are refused, compressed or not.
const MAX_ARCHIVE: u64 = 256 << 20;
/// The parts of the repository that are board data.
const KEEP: &[&str] = &[
    "devices/",
    "chips/",
    "platforms/",
    "features.toml",
    "boot-paths.toml",
    "routes.toml",
];

/// Where the board data in use came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Copy {
    /// The commit of main the copy was made from.
    pub commit: Option<String>,
    etag: Option<String>,
    /// When the copy was last fetched or confirmed unchanged, in Unix seconds.
    pub checked: u64,
}

/// The board data directory, and the copy it holds if it came from GitHub.
pub struct Data {
    pub dir: PathBuf,
    pub copy: Option<Copy>,
}

/// The data to use: `explicit` if given, otherwise the copy from main. A
/// copy that is there is used as it is, however old: using a device never
/// waits for a download. Only when there is none yet is main fetched.
/// [`update`] is the way to a newer copy.
pub fn resolve(explicit: Option<&Path>, store: &Store) -> Result<Data> {
    if let Some(dir) = explicit {
        return Ok(Data {
            dir: dir.to_owned(),
            copy: None,
        });
    }
    match read_copy(store) {
        Some(copy) => Ok(Data {
            dir: current(store),
            copy: Some(copy),
        }),
        None => update(store),
    }
}

/// The data to use without fetching anything: `explicit`, or the copy from
/// main if there is one.
pub fn existing(explicit: Option<&Path>, store: &Store) -> Option<PathBuf> {
    match explicit {
        Some(dir) => Some(dir.to_owned()),
        None => read_copy(store).map(|_| current(store)),
    }
}

/// Fetches main now, unless it is unchanged since the last copy.
pub fn update(store: &Store) -> Result<Data> {
    let old = read_copy(store);
    let agent = http::agent(URL, TIMEOUT)?;
    let mut request = agent.get(URL);
    if let Some(etag) = old.as_ref().and_then(|c| c.etag.as_deref()) {
        request = request.header("If-None-Match", etag);
    }
    let mut response = request
        .call()
        .map_err(|e| Error::new(Code::Network, e.to_string()).context(URL))?;
    // Not modified: main is unchanged since the copy was made.
    if response.status().as_u16() == 304 {
        let old = old.ok_or_else(|| {
            Error::new(
                Code::Network,
                "server answered not modified to a first fetch",
            )
            .context(URL)
        })?;
        let copy = Copy {
            checked: now(),
            ..old
        };
        write_copy(store, &copy)?;
        return Ok(Data {
            dir: current(store),
            copy: Some(copy),
        });
    }
    let etag = response
        .headers()
        .get("ETag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let compressed = response
        .body_mut()
        .with_config()
        .limit(MAX_ARCHIVE)
        .read_to_vec()
        .map_err(|e| Error::new(Code::Network, e.to_string()).context(URL))?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&compressed[..])
        .take(MAX_ARCHIVE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(Code::Format, e.to_string()).context(URL))?;
    if bytes.len() as u64 > MAX_ARCHIVE {
        return Err(Error::new(Code::TooLarge, "archive is too large").context(URL));
    }
    let archive = tar::read(&bytes).context(URL)?;
    install(store, &archive)?;
    let copy = Copy {
        commit: archive.comment,
        etag,
        checked: now(),
    };
    write_copy(store, &copy)?;
    Ok(Data {
        dir: current(store),
        copy: Some(copy),
    })
}

/// Writes the board data from `archive` beside the current copy, then swaps
/// it in.
fn install(store: &Store, archive: &tar::Archive) -> Result<()> {
    let root = store.dir().join("data");
    let fresh = root.join(format!(".new-{}", std::process::id()));
    let _ = fs::remove_dir_all(&fresh);
    let mut kept = 0;
    for file in &archive.files {
        // Paths start with the snapshot's top directory, such as `orchard-main/`.
        let Some((_, path)) = file.path.split_once('/') else {
            continue;
        };
        if !KEEP.iter().any(|k| path.starts_with(k))
            || path.split('/').any(|c| c == ".." || c.is_empty())
        {
            continue;
        }
        let target = fresh.join(path);
        fs::create_dir_all(target.parent().unwrap()).context(target.display())?;
        fs::write(&target, file.data).context(target.display())?;
        kept += 1;
    }
    if kept == 0 || !fresh.join("devices").is_dir() {
        let _ = fs::remove_dir_all(&fresh);
        return Err(Error::new(Code::Format, "archive holds no board data").context(URL));
    }
    let old = root.join(format!(".old-{}", std::process::id()));
    let current = current(store);
    if current.exists() {
        fs::rename(&current, &old).context(current.display())?;
    }
    fs::rename(&fresh, &current).context(current.display())?;
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

fn current(store: &Store) -> PathBuf {
    store.dir().join("data/current")
}

fn read_copy(store: &Store) -> Option<Copy> {
    let text = fs::read_to_string(store.dir().join("data/source.json")).ok()?;
    let copy: Copy = serde_json::from_str(&text).ok()?;
    current(store).join("devices").is_dir().then_some(copy)
}

fn write_copy(store: &Store, copy: &Copy) -> Result<()> {
    let path = store.dir().join("data/source.json");
    let temp = path.with_extension(format!("json.{}", std::process::id()));
    fs::write(&temp, serde_json::to_vec(copy).expect("copy serializes"))
        .and_then(|_| fs::rename(&temp, &path))
        .context(path.display())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

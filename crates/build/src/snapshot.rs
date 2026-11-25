//! orchard's recipes for a build: the data directory the person names, or a
//! copy of orchard's newest nightly kept in the cache.
//!
//! The nightly's source is the state its published kernels, m1n1 and tools
//! were built from, so a build from it finds them by key instead of
//! compiling. A copy that is there is used as it is; [`update`] fetches the
//! newest.
use crate::{published, read_text, write_text, RELEASES};
use orchard_cache::store::Store;
use orchard_cache::{Code, Context, Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Inside a copy: the nightly and commit it is.
pub const RECORD: &str = ".orchard-snapshot";
const SOURCE: &str = "https://codeload.github.com/OrchardLinux/orchard/tar.gz/refs/tags";
const LIMIT: u64 = 256 << 20;

fn directory(store: &Store) -> PathBuf {
    store.dir().join("recipes")
}

/// The copy in use, if there is one.
pub fn existing(store: &Store) -> Option<PathBuf> {
    let directory = directory(store);
    let tag = read_text(&directory.join("current")).ok()?;
    let path = directory.join(tag.trim());
    path.join("kernel/build").is_file().then_some(path)
}

/// The recipes to build from: `explicit`, else the copy in the cache,
/// fetched when there is none yet.
pub fn resolve(explicit: Option<&Path>, store: &Store, say: &dyn Fn(&str)) -> Result<PathBuf> {
    match (explicit, existing(store)) {
        (Some(path), _) => Ok(path.to_owned()),
        (None, Some(path)) => Ok(path),
        (None, None) => update(store, say),
    }
}

/// What a copy says it is: `{tag, commit}`.
pub fn record(root: &Path) -> Option<Value> {
    serde_json::from_str(&read_text(&root.join(RECORD)).ok()?).ok()
}

/// Makes the newest nightly's source the copy in use.
pub fn update(store: &Store, say: &dyn Fn(&str)) -> Result<PathBuf> {
    let pointer =
        published::fetch(&format!("{RELEASES}/nightly/latest.json"), 64 << 10).map_err(|e| {
            e.context("orchard's newest nightly")
                .hint("connect to the network once, or point --data at an orchard checkout")
        })?;
    let pointer: Value =
        serde_json::from_slice(&pointer).map_err(|e| Error::new(Code::Format, e.to_string()))?;
    let tag = pointer["name"].as_str().unwrap_or("");
    let plain = !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        && !tag.starts_with('.');
    if !plain {
        return Err(Error::new(
            Code::Format,
            "the nightly pointer names no build",
        ));
    }
    let directory = directory(store);
    let target = directory.join(tag);
    if !target.join("kernel/build").is_file() {
        say(&format!("Fetching orchard's recipes ({tag})"));
        std::fs::create_dir_all(&directory).context(directory.display())?;
        let temporary = tempfile::Builder::new()
            .prefix(".fetch-")
            .tempdir_in(&directory)
            .context(directory.display())?;
        let archive = temporary.path().join("source.tar.gz");
        download(&format!("{SOURCE}/{tag}"), &archive)?;
        let unpacked = temporary.path().join("unpacked");
        published::extract(&archive, "", &unpacked)?;
        // The archive holds one directory, named after the repository and the tag.
        let mut inner = std::fs::read_dir(&unpacked)
            .context(unpacked.display())?
            .flatten()
            .map(|i| i.path());
        let top = match (inner.next(), inner.next()) {
            (Some(top), None) if top.join("kernel/build").is_file() => top,
            _ => {
                return Err(Error::new(
                    Code::Format,
                    format!("the source of {tag} is not orchard's"),
                ))
            }
        };
        write_text(
            &top.join(RECORD),
            &crate::json::pretty(&json!({"tag": tag, "commit": pointer["commit"]})),
        )?;
        if target.exists() {
            std::fs::remove_dir_all(&target).context(target.display())?;
        }
        std::fs::rename(&top, &target).context(target.display())?;
    }
    write_text(&directory.join("current"), &format!("{tag}\n"))?;
    // Older copies are not used again.
    for item in std::fs::read_dir(&directory)
        .context(directory.display())?
        .flatten()
    {
        let path = item.path();
        if path.is_dir() && path != target && !item.file_name().to_string_lossy().starts_with('.') {
            let _ = std::fs::remove_dir_all(path);
        }
    }
    Ok(target)
}

fn download(url: &str, to: &Path) -> Result<()> {
    let agent = orchard_cache::http::agent(url, Duration::from_secs(60))?;
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| Error::new(Code::Network, e.to_string()).context(url))?;
    let mut file = std::fs::File::create(to).context(to.display())?;
    let mut body = std::io::Read::take(response.body_mut().as_reader(), LIMIT + 1);
    let size = std::io::copy(&mut body, &mut file)
        .map_err(|e| Error::new(Code::Network, e.to_string()).context(url))?;
    if size > LIMIT {
        return Err(Error::new(
            Code::TooLarge,
            format!("{url} is larger than expected"),
        ));
    }
    Ok(())
}

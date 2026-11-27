//! Build components: directories made once from a set of inputs and never
//! written again. Each lives at `components/NAME/KEY/`, where the key is a
//! hash of everything that decides its content. Every file is recorded with
//! its SHA-256 (`component.json`) and checked before the entry is used.
//!
//! Beside them the cache keeps what producers build in: work directories
//! (`work/RECIPE/KEY`), kernel trees (`kernel-work/`), git objects (`git/`)
//! and the compiler cache (`ccache/`).
use crate::{Code, Context, Error, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The file inside an entry that lists its files and their hashes.
pub const RECORDS: &str = "component.json";
/// The file inside an entry that holds the inputs its key was made from.
pub const INPUTS: &str = "key-inputs.json";
/// One copy of each file content and mode, hard-linked into every entry
/// that has it.
const SHARED: &str = ".objects";

/// What an entry is found by: the hash, and the inputs it was made from
/// when the caller can name them, so a later miss can say what changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub hash: String,
    pub inputs: Option<serde_json::Value>,
}

impl Key {
    pub fn plain(hash: impl Into<String>) -> Key {
        Key {
            hash: hash.into(),
            inputs: None,
        }
    }
}

/// An entry that was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    /// Whether it was already there.
    pub reused: bool,
}

/// One directory under `components/NAME/` or `work/RECIPE/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub key: String,
    pub path: PathBuf,
    /// When it was last used.
    pub used: SystemTime,
}

/// The build part of the cache.
#[derive(Debug, Clone)]
pub struct Components {
    root: PathBuf,
}

/// A held lock on the build cache or on one entry; dropping it releases it.
pub struct Lock {
    _file: File,
}

impl Components {
    /// The build cache inside the cache directory `root`.
    pub fn open(root: &Path) -> Result<Components> {
        let components = Components {
            root: root.to_owned(),
        };
        fs::create_dir_all(components.dir()).context(components.dir().display())?;
        Ok(components)
    }

    /// The cache directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join("components")
    }

    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    pub fn kernel_work(&self) -> PathBuf {
        self.root.join("kernel-work")
    }

    pub fn git(&self) -> PathBuf {
        self.root.join("git")
    }

    pub fn ccache(&self) -> PathBuf {
        self.root.join("ccache")
    }

    /// Use of the build cache. Builds share it, so they run side by side;
    /// what removes entries takes it exclusively and waits until no build
    /// uses the cache. `say` is told when the lock has to be waited for.
    pub fn lock(&self, exclusive: bool, say: &dyn Fn(&str)) -> Result<Lock> {
        let path = self.root.join("build.lock");
        let waiting = if exclusive {
            format!(
                "Waiting for running builds to finish with {} ...",
                self.root.display()
            )
        } else {
            format!(
                "Waiting for another process to finish changing {} ...",
                self.root.display()
            )
        };
        lock(&path, exclusive, &|| say(&waiting))
    }

    /// The build cache's lock, taken exclusively only when nobody holds it.
    pub fn try_lock(&self) -> Result<Option<Lock>> {
        try_lock(&self.root.join("build.lock"))
    }

    /// The work directory of one recipe input set. Recipes keep sources and
    /// incremental output there; using it refreshes its time, which pruning
    /// goes by.
    pub fn recipe_work(&self, recipe: &str, key: &str) -> Result<PathBuf> {
        let work = self.work().join(recipe).join(key);
        if work.exists() {
            touch(&work)?;
        } else if let Some(parent) = work.parent() {
            fs::create_dir_all(parent).context(parent.display())?;
        }
        Ok(work)
    }

    /// The entry `name` for `key`, made by `produce` when it is not there.
    ///
    /// `key` is asked again after `produce`: inputs that changed during the
    /// build mean the result is not what the key says, and it is dropped.
    /// One process builds an entry at a time; another that wants the same
    /// entry waits for it and reuses it.
    pub fn cached(
        &self,
        name: &str,
        key: &mut dyn FnMut() -> Result<Key>,
        produce: &mut dyn FnMut(&Path) -> Result<()>,
        say: &dyn Fn(&str),
    ) -> Result<Found> {
        let parent = self.dir().join(name);
        fs::create_dir_all(&parent).context(parent.display())?;
        let first = key()?;
        let target = parent.join(&first.hash);
        if target.exists() {
            return reuse(name, &target, say);
        }
        let locks = self.dir().join(".locks");
        fs::create_dir_all(&locks).context(locks.display())?;
        let _entry = lock(&locks.join(format!("{name}-{}", first.hash)), true, &|| {
            say(&format!("Waiting for another build of {name} ..."))
        })?;
        if target.exists() {
            return reuse(name, &target, say);
        }
        say(&format!(
            "Building {name} (cache miss): {}",
            target.display()
        ));
        if let Some(inputs) = &first.inputs {
            if let Some((previous, changed)) = explain_miss(&parent, inputs) {
                let more = match changed.len() {
                    0..=8 => String::new(),
                    n => format!(" and {} more", n - 8),
                };
                let shown: Vec<&str> = changed.iter().take(8).map(String::as_str).collect();
                let short = &previous[..previous.len().min(12)];
                say(&format!(
                    "  differs from newest cached {short} in: {}{more}",
                    shown.join(", ")
                ));
            }
        }
        let temporary = tempfile::Builder::new()
            .prefix("build-")
            .tempdir_in(&parent)
            .context(parent.display())?;
        let stage = temporary.path().join("component");
        fs::create_dir(&stage).context(stage.display())?;
        produce(&stage)?;
        if let Some(inputs) = &first.inputs {
            write_json(&stage.join(INPUTS), inputs)?;
        }
        let records = record(&stage)?;
        write_json(&stage.join(RECORDS), &serde_json::json!(records))?;
        if key()?.hash != first.hash {
            return Err(Error::new(
                Code::InvalidInput,
                format!("{name} inputs changed during build; result was not cached"),
            ));
        }
        if target.exists() {
            return Err(Error::new(
                Code::Io,
                format!(
                    "component appeared during build: {}; inspect before rebuilding",
                    target.display()
                ),
            ));
        }
        self.share(&stage, &records);
        fs::rename(&stage, &target).context(target.display())?;
        Ok(Found {
            path: target,
            reused: false,
        })
    }

    /// Replaces files under `root` by hard links to the cache's single copy
    /// of their content. Entries are never written after they are made, so
    /// sharing is invisible to readers. A shared copy that no longer matches
    /// its name is replaced. Filesystems without hard links keep private
    /// copies.
    pub fn share(&self, root: &Path, records: &BTreeMap<String, String>) {
        let store = self.dir().join(SHARED);
        if fs::create_dir_all(&store).is_err() {
            return;
        }
        for (relative, checksum) in records {
            let path = root.join(relative);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let shared = store.join(format!("{checksum}-{:o}", mode(&meta) & 0o7777));
            let _ = share_one(&path, &shared, checksum, &meta);
        }
    }

    /// Deletes shared copies no entry links to any more.
    pub fn remove_unshared(&self) {
        let Ok(items) = fs::read_dir(self.dir().join(SHARED)) else {
            return;
        };
        for item in items.flatten() {
            if fs::symlink_metadata(item.path()).is_ok_and(|m| links(&m) == 1) {
                let _ = fs::remove_file(item.path());
            }
        }
    }

    /// Entries and leftover build directories, by name, oldest use first
    /// within each name.
    pub fn entries(&self) -> Vec<Entry> {
        keyed(&self.dir())
    }

    /// The recipes' work directories, by recipe, oldest use first.
    pub fn work_entries(&self) -> Vec<Entry> {
        keyed(&self.work())
    }

    /// Removes the entry locks of finished builds. Only while the cache is
    /// held exclusively.
    pub fn clear_entry_locks(&self) {
        if let Ok(items) = fs::read_dir(self.dir().join(".locks")) {
            for item in items.flatten() {
                let _ = fs::remove_file(item.path());
            }
        }
    }
}

/// The records of an entry: relative path to SHA-256.
pub fn records(entry: &Path) -> Result<BTreeMap<String, String>> {
    let path = entry.join(RECORDS);
    let text = fs::read(&path).context(path.display())?;
    serde_json::from_slice(&text)
        .map_err(|e| Error::new(Code::Format, e.to_string()).context(path.display()))
}

/// An entry that is there, checked file by file before use.
fn reuse(name: &str, target: &Path, say: &dyn Fn(&str)) -> Result<Found> {
    let recorded = records(target)?;
    let mut actual = BTreeSet::new();
    for file in files(target)? {
        if file != Path::new(RECORDS) {
            actual.insert(file.to_string_lossy().into_owned());
        }
    }
    if actual != recorded.keys().cloned().collect() {
        return Err(Error::new(
            Code::Format,
            format!("cached component inventory changed: {}", target.display()),
        )
        .hint("remove it (orchard cache rm) and rebuild"));
    }
    for (relative, expected) in &recorded {
        let path = target.join(relative);
        let linked = fs::symlink_metadata(&path)
            .context(path.display())?
            .is_symlink();
        if linked || &digest(&path)? != expected {
            return Err(Error::new(
                Code::Format,
                format!("cached component changed: {}", path.display()),
            )
            .hint("remove that component (orchard cache rm) and rebuild"));
        }
    }
    touch(target)?;
    say(&format!("Reusing {name}: {}", target.display()));
    Ok(Found {
        path: target.to_owned(),
        reused: true,
    })
}

/// The inputs that differ from the newest recorded entry of a component.
fn explain_miss(parent: &Path, inputs: &serde_json::Value) -> Option<(String, Vec<String>)> {
    let newest = keyed_in(parent, "")
        .into_iter()
        .filter(|e| !e.key.starts_with("build-") && e.path.join(INPUTS).is_file())
        .max_by_key(|e| {
            fs::metadata(e.path.join(INPUTS))
                .and_then(|m| m.modified())
                .ok()
        })?;
    let previous: serde_json::Value =
        serde_json::from_slice(&fs::read(newest.path.join(INPUTS)).ok()?).ok()?;
    let (mut old, mut new) = (BTreeMap::new(), BTreeMap::new());
    flatten(&previous, String::new(), &mut old);
    flatten(inputs, String::new(), &mut new);
    let names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let changed = names
        .into_iter()
        .filter(|name| old.get(*name) != new.get(*name))
        .cloned()
        .collect();
    Some((newest.key, changed))
}

fn flatten<'a>(
    value: &'a serde_json::Value,
    prefix: String,
    flat: &mut BTreeMap<String, &'a serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, item) in map {
                let name = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                flatten(item, name, flat);
            }
        }
        other => {
            flat.insert(prefix, other);
        }
    }
}

fn share_one(path: &Path, shared: &Path, checksum: &str, meta: &fs::Metadata) -> Result<()> {
    if let Ok(existing) = fs::metadata(shared) {
        if same_file(&existing, meta) {
            return Ok(());
        }
        if digest(shared)? != checksum {
            fs::remove_file(shared)?;
        }
    }
    if !shared.exists() {
        fs::hard_link(path, shared)?;
        return Ok(());
    }
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".orchard-share");
    let replacement = path.with_file_name(name);
    fs::hard_link(shared, &replacement)?;
    fs::rename(&replacement, path)?;
    Ok(())
}

/// Every file under `root`, as sorted relative paths. A link to a file
/// counts as a file, as it would be read.
pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, root: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
        for item in fs::read_dir(dir).context(dir.display())? {
            let path = item?.path();
            let meta = fs::symlink_metadata(&path).context(path.display())?;
            if meta.is_dir() {
                walk(&path, root, found)?;
            } else if path.is_file() {
                found.push(path.strip_prefix(root).expect("under root").to_owned());
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(root, root, &mut found)?;
    found.sort();
    Ok(found)
}

fn record(stage: &Path) -> Result<BTreeMap<String, String>> {
    let mut records = BTreeMap::new();
    for file in files(stage)? {
        records.insert(
            file.to_string_lossy().into_owned(),
            digest(&stage.join(&file))?,
        );
    }
    Ok(records)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value).expect("JSON value serializes");
    text.push('\n');
    fs::write(path, text).context(path.display())
}

fn keyed(directory: &Path) -> Vec<Entry> {
    let Ok(items) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut parents: Vec<PathBuf> = items
        .flatten()
        .map(|i| i.path())
        .filter(|p| p.is_dir() && !hidden(p))
        .collect();
    parents.sort();
    parents
        .iter()
        .flat_map(|parent| keyed_in(parent, &name_of(parent)))
        .collect()
}

fn keyed_in(parent: &Path, name: &str) -> Vec<Entry> {
    let Ok(items) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = items
        .flatten()
        .map(|i| i.path())
        .filter(|p| p.is_dir())
        .map(|path| Entry {
            name: name.to_owned(),
            key: name_of(&path),
            used: fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH),
            path,
        })
        .collect();
    entries.sort_by(|a, b| a.used.cmp(&b.used).then_with(|| a.key.cmp(&b.key)));
    entries
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn hidden(path: &Path) -> bool {
    name_of(path).starts_with('.')
}

/// The SHA-256 of a file, read in blocks.
pub fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).context(path.display())?;
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut block).context(path.display())?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Marks a directory as used now.
pub fn touch(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.set_modified(SystemTime::now()))
        .context(path.display())
}

/// Bytes under `path` as they take space: a file with several hard links is
/// counted once.
pub fn on_disk(path: &Path) -> u64 {
    fn walk(path: &Path, seen: &mut HashSet<(u64, u64)>, total: &mut u64) {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return;
        };
        if meta.is_dir() {
            if let Ok(items) = fs::read_dir(path) {
                for item in items.flatten() {
                    walk(&item.path(), seen, total);
                }
            }
        } else if seen.insert(inode(&meta)) {
            *total += meta.len();
        }
    }
    let (mut seen, mut total) = (HashSet::new(), 0);
    walk(path, &mut seen, &mut total);
    total
}

/// Free bytes on the filesystem that holds `path`, or of its nearest
/// existing parent.
pub fn free_space(path: &Path) -> Result<u64> {
    let mut at = path;
    while !at.exists() {
        match at.parent() {
            Some(parent) => at = parent,
            None => break,
        }
    }
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(at.as_os_str().as_bytes())
        .map_err(|_| Error::new(Code::InvalidInput, "path contains a NUL byte"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `name` is a valid C string and `stat` is writable; statvfs
    // fills it when it returns 0.
    let stat = unsafe {
        if libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) != 0 {
            return Err(Error::from(std::io::Error::last_os_error()).context(at.display()));
        }
        stat.assume_init()
    };
    #[allow(clippy::unnecessary_cast)]
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// Holds `path`'s lock, waiting for it; `waiting` is called once if it is
/// not free at once.
pub fn lock(path: &Path, exclusive: bool, waiting: &dyn Fn()) -> Result<Lock> {
    use std::os::fd::AsRawFd;
    let how = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    let mut told = false;
    loop {
        let file = open_lock(path)?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), how | libc::LOCK_NB) } != 0 {
            if !told {
                waiting();
                told = true;
            }
            loop {
                // SAFETY: as above.
                if unsafe { libc::flock(file.as_raw_fd(), how) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(Error::from(error).context(path.display()));
                }
            }
        }
        if still_there(&file, path) {
            return Ok(Lock { _file: file });
        }
    }
}

fn open_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context(parent.display())?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context(path.display())
}

/// Whether the lock that was taken is still the file at `path`: what was
/// waited for can have been deleted with its directory, and its lock then
/// protects nothing.
fn still_there(file: &File, path: &Path) -> bool {
    match (file.metadata(), fs::metadata(path)) {
        (Ok(held), Ok(now)) => same_file(&held, &now),
        _ => false,
    }
}

/// Holds `path`'s lock exclusively, or `None` when another process has it.
pub fn try_lock(path: &Path) -> Result<Option<Lock>> {
    use std::os::fd::AsRawFd;
    let file = open_lock(path)?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    Ok(still_there(&file, path).then_some(Lock { _file: file }))
}

fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode()
}

fn links(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

fn inode(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    inode(a) == inode(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn cache() -> (tempfile::TempDir, Components) {
        let dir = tempfile::tempdir().unwrap();
        let components = Components::open(dir.path()).unwrap();
        (dir, components)
    }

    fn quiet(_: &str) {}

    fn make(components: &Components, name: &str, key: &str, content: &str) -> Found {
        components
            .cached(
                name,
                &mut || Ok(Key::plain(key)),
                &mut |stage| Ok(fs::write(stage.join("file"), content)?),
                &quiet,
            )
            .unwrap()
    }

    #[test]
    fn an_entry_is_built_once_and_then_reused() {
        let (_dir, components) = cache();
        let built = Cell::new(0);
        let get = || {
            components
                .cached(
                    "tools",
                    &mut || Ok(Key::plain("abc")),
                    &mut |stage| {
                        built.set(built.get() + 1);
                        fs::create_dir(stage.join("sub"))?;
                        Ok(fs::write(stage.join("sub/file"), "one")?)
                    },
                    &quiet,
                )
                .unwrap()
        };
        let first = get();
        let second = get();
        assert!(!first.reused && second.reused);
        assert_eq!(first.path, second.path);
        assert_eq!(built.get(), 1);
        assert_eq!(
            records(&first.path).unwrap().keys().collect::<Vec<_>>(),
            ["sub/file"]
        );
        // Nothing of the build is left beside the entry.
        assert_eq!(components.entries().len(), 1);
    }

    #[test]
    fn a_changed_or_extra_file_is_refused() {
        let (_dir, components) = cache();
        let found = make(&components, "tools", "abc", "one");
        fs::write(found.path.join("extra"), "x").unwrap();
        let error = components
            .cached(
                "tools",
                &mut || Ok(Key::plain("abc")),
                &mut |_| Ok(()),
                &quiet,
            )
            .unwrap_err();
        assert!(error.message.contains("inventory changed"), "{error}");
        fs::remove_file(found.path.join("extra")).unwrap();
        // The file is shared, so replace it instead of writing through the link.
        fs::remove_file(found.path.join("file")).unwrap();
        fs::write(found.path.join("file"), "two").unwrap();
        let error = components
            .cached(
                "tools",
                &mut || Ok(Key::plain("abc")),
                &mut |_| Ok(()),
                &quiet,
            )
            .unwrap_err();
        assert!(error.message.contains("component changed"), "{error}");
    }

    #[test]
    fn inputs_that_change_during_the_build_are_not_cached() {
        let (_dir, components) = cache();
        let asked = Cell::new(0);
        let error = components
            .cached(
                "kernel",
                &mut || {
                    asked.set(asked.get() + 1);
                    Ok(Key::plain(format!("key{}", asked.get())))
                },
                &mut |stage| Ok(fs::write(stage.join("file"), "x")?),
                &quiet,
            )
            .unwrap_err();
        assert!(
            error.message.contains("inputs changed during build"),
            "{error}"
        );
        assert!(components.entries().is_empty());
    }

    #[test]
    fn a_failed_build_leaves_no_entry() {
        let (_dir, components) = cache();
        let error = components
            .cached(
                "kernel",
                &mut || Ok(Key::plain("abc")),
                &mut |_| Err(Error::new(Code::Io, "the producer failed")),
                &quiet,
            )
            .unwrap_err();
        assert_eq!(error.message, "the producer failed");
        assert!(components.entries().is_empty());
    }

    #[test]
    fn a_miss_names_the_inputs_that_changed() {
        let (_dir, components) = cache();
        let said = std::cell::RefCell::new(Vec::new());
        for (hash, tree) in [("one", "aaa"), ("two", "bbb")] {
            let inputs = serde_json::json!({"tree": tree, "build": {"arch": "arm64"}});
            components
                .cached(
                    "kernel",
                    &mut || {
                        Ok(Key {
                            hash: hash.into(),
                            inputs: Some(inputs.clone()),
                        })
                    },
                    &mut |stage| Ok(fs::write(stage.join("file"), tree)?),
                    &|line| said.borrow_mut().push(line.to_owned()),
                )
                .unwrap();
        }
        let said = said.into_inner();
        assert!(
            said.iter()
                .any(|l| l == "  differs from newest cached one in: tree"),
            "{said:?}"
        );
    }

    #[test]
    fn identical_files_take_space_once() {
        let (_dir, components) = cache();
        let content = "x".repeat(4096);
        make(&components, "a", "one", &content);
        make(&components, "b", "two", &content);
        // Two entries and the shared copy hold the file; it is stored once.
        let size = on_disk(&components.dir());
        assert!(size < 2 * 4096, "{size}");
        // A shared copy is kept only while an entry links to it.
        for entry in components.entries() {
            fs::remove_dir_all(entry.path).unwrap();
        }
        components.remove_unshared();
        assert_eq!(on_disk(&components.dir().join(SHARED)), 0);
    }

    #[test]
    fn entries_list_oldest_use_first() {
        let (_dir, components) = cache();
        let old = make(&components, "kernel", "old", "1");
        let new = make(&components, "kernel", "new", "2");
        let earlier = SystemTime::now() - std::time::Duration::from_secs(3600);
        File::open(&new.path)
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        let keys: Vec<String> = components.entries().into_iter().map(|e| e.key).collect();
        assert_eq!(keys, ["new", "old"]);
        // Using an entry makes it the newest.
        make(&components, "kernel", "new", "2");
        let keys: Vec<String> = components.entries().into_iter().map(|e| e.key).collect();
        assert_eq!(keys, ["old", "new"]);
        assert!(old.path.is_dir());
    }

    #[test]
    fn a_lock_is_exclusive_until_dropped() {
        let (dir, _components) = cache();
        let path = dir.path().join("x.lock");
        let held = lock(&path, true, &|| {}).unwrap();
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
    }
}

//! Looking after the build cache: what is in it, removing entries, and
//! keeping it within its size limit.
//!
//! Before a build the least recently used entries are deleted until the
//! build cache fits its limit. Only what can be made again goes: older
//! component entries and work directories, and kernel trees without local
//! edits. The newest entry of each component and work recipe and the newest
//! kernel tree always stay.
use crate::{invalid, sources};
use orchard_cache::component::{free_space, on_disk, records, try_lock, Components, Entry, INPUTS};
use orchard_cache::{Code, Context, Error, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// GiB the build cache may use; `ORCHARD_CACHE_BUDGET_GB` changes it, 0
/// turns the limit off.
const BUDGET_GB: f64 = 20.0;
/// GB a build needs free; `ORCHARD_MIN_FREE_GB` changes it, 0 turns the
/// check off.
const MIN_FREE_GB: f64 = 10.0;

fn setting(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Bytes the build cache may use.
pub fn budget() -> u64 {
    (setting("ORCHARD_CACHE_BUDGET_GB", BUDGET_GB) * (1u64 << 30) as f64) as u64
}

/// A size as people read it.
pub fn human(size: u64) -> String {
    let mut value = size as f64;
    for unit in ["B", "K", "M"] {
        if value < 1024.0 {
            return if unit == "B" {
                format!("{value:.0}B")
            } else {
                format!("{value:.1}{unit}")
            };
        }
        value /= 1024.0;
    }
    format!("{value:.1}G")
}

/// Refuses to start a build when the disk of one of `paths` has less than
/// the needed space free: a build that runs out halfway leaves broken work
/// directories and stops other builds on the host.
pub fn check_free(paths: &[&Path]) -> Result<()> {
    let needed = setting("ORCHARD_MIN_FREE_GB", MIN_FREE_GB);
    if needed <= 0.0 {
        return Ok(());
    }
    for path in paths {
        let free = free_space(path)? as f64 / 1e9;
        if free < needed {
            return Err(Error::new(
                Code::Io,
                format!("only {free:.1} GB free on the disk of {}; a build needs {needed} GB", path.display()),
            )
            .hint("free space first (orchard2 cache prune, your own out/ builds), or set ORCHARD_MIN_FREE_GB to change the limit"));
        }
    }
    Ok(())
}

/// A kernel tree in the cache.
#[derive(Debug, Clone)]
pub struct KernelTree {
    pub path: PathBuf,
    pub used: SystemTime,
    /// The source is exactly what orchard prepared: no local edits, so the
    /// tree can be made again.
    pub clean: bool,
}

/// The cache's kernel trees, oldest use first.
pub fn kernel_trees(cache: &Components) -> Vec<KernelTree> {
    let Ok(items) = std::fs::read_dir(cache.kernel_work()) else {
        return Vec::new();
    };
    let modified = |path: PathBuf| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut trees: Vec<KernelTree> = items
        .flatten()
        .map(|item| item.path())
        .filter(|path| path.is_dir() && !name_of(path).starts_with('.'))
        .map(|path| {
            let marks = ["prepared-tree", "applied-patches"]
                .iter()
                .filter_map(|name| modified(path.join(name)))
                .max();
            let used = marks
                .or_else(|| modified(path.clone()))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let clean = is_clean(&path);
            KernelTree { path, used, clean }
        })
        .collect();
    trees.sort_by_key(|tree| tree.used);
    trees
}

/// Whether a kernel tree holds nothing that can't be made again: its content
/// is what orchard prepared, and its git has no work of its own (staged
/// content, stashes, branches, tags, or commits made in it).
fn is_clean(tree: &Path) -> bool {
    let source = tree.join("source");
    let Ok(baseline) = std::fs::read_to_string(tree.join("prepared-tree")) else {
        return false;
    };
    let git = |args: &[&str]| sources::git(&source, args, &[]).ok();
    source.join(".git").exists()
        && sources::working_tree(&source, true).is_ok_and(|tree| tree == baseline.trim())
        && git(&["for-each-ref", "--format=%(refname)"]).is_some_and(|refs| refs.is_empty())
        && sources::git_ok(&source, &["diff", "--cached", "--quiet"])
        && git(&["rev-parse", "HEAD"]).is_some_and(|head| {
            // Commits made on the detached HEAD are only in its reflog.
            git(&["reflog", "show", "--format=%H", "HEAD"])
                .is_some_and(|log| log.lines().all(|commit| commit == head))
        })
}

/// Deletes a kernel tree that nobody uses and that can be made again. It is
/// moved away under its lock first, so what waits for the lock finds no
/// tree instead of half of one.
fn remove_tree(path: &Path) -> bool {
    let Some(_held) = try_lock(&path.join(".lock")).ok().flatten() else {
        return false;
    };
    if !is_clean(path) {
        return false;
    }
    let aside = path.with_file_name(format!(
        "{REMOVING}{}-{}",
        name_of(path),
        std::process::id()
    ));
    if std::fs::rename(path, &aside).is_err() {
        return false;
    }
    let _ = std::fs::remove_dir_all(&aside);
    true
}

/// What a kernel tree is called while it is deleted.
const REMOVING: &str = ".removing-";

/// Deletes what interrupted builds left behind: unfinished entries and
/// their staging directories, and trees that were being deleted. Only while
/// no build uses the cache.
fn sweep(cache: &Components) {
    for entry in cache.entries() {
        if entry.key.starts_with("build-") {
            let _ = std::fs::remove_dir_all(&entry.path);
        }
    }
    let within = |directory: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(directory)
            .map(|items| items.flatten().map(|item| item.path()).collect())
            .unwrap_or_default()
    };
    for path in within(&cache.kernel_work()) {
        if name_of(&path).starts_with(REMOVING) {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        if !path.is_dir() || name_of(&path).starts_with('.') {
            continue;
        }
        let Some(_held) = try_lock(&path.join(".lock")).ok().flatten() else {
            continue;
        };
        for left in within(&path) {
            let name = name_of(&left);
            if ["base-", "modules-", "source-"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                let _ = std::fs::remove_dir_all(&left);
            }
        }
    }
    for left in within(&cache.root().join("recipes")) {
        if name_of(&left).starts_with(".fetch-") {
            let _ = std::fs::remove_dir_all(&left);
        }
    }
}

/// Bytes the build cache takes on disk.
pub fn usage(cache: &Components) -> u64 {
    [
        cache.dir(),
        cache.work(),
        cache.kernel_work(),
        cache.git(),
        cache.ccache(),
    ]
    .iter()
    .map(|p| on_disk(p))
    .sum()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn short(key: &str) -> &str {
    &key[..key.len().min(12)]
}

/// What an entry was made from, in a few words.
pub fn summary(entry: &Entry) -> String {
    if entry.key.starts_with("build-") {
        return "unfinished build".into();
    }
    let inputs = std::fs::read(entry.path.join(INPUTS))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    match inputs {
        None => "inputs not recorded".into(),
        Some(inputs) => match (inputs["tree"].as_str(), inputs["base"].as_str()) {
            (Some(tree), _) => format!("tree {}", short(tree)),
            (None, Some(base)) => format!(
                "base {} dtb {}",
                short(base),
                inputs["dtb"].as_str().unwrap_or("?")
            ),
            _ => String::new(),
        },
    }
}

/// Deletes the entries named `COMPONENT/KEY-PREFIX` or `COMPONENT` (all of
/// its entries) and returns what was deleted. Waits until no build uses
/// the cache.
pub fn remove(cache: &Components, selectors: &[String], say: &dyn Fn(&str)) -> Result<Vec<String>> {
    let rows = cache.entries();
    let mut chosen: Vec<&Entry> = Vec::new();
    for selector in selectors {
        let (component, prefix) = selector.split_once('/').unwrap_or((selector, ""));
        let matches: Vec<&Entry> = rows
            .iter()
            .filter(|e| e.name == component && e.key.starts_with(prefix))
            .collect();
        if matches.is_empty() {
            return Err(Error::new(
                Code::NotFound,
                format!("no cache entry matches {selector}"),
            ));
        }
        if !prefix.is_empty() && matches.len() > 1 {
            return Err(Error::new(
                Code::Ambiguous,
                format!("{selector} is ambiguous; give more of the key"),
            ));
        }
        chosen.extend(
            matches
                .into_iter()
                .filter(|m| !chosen.iter().any(|c| c.path == m.path))
                .collect::<Vec<_>>(),
        );
    }
    // The lock keeps removal away from a running build and its sources.
    let _held = cache.lock(true, say)?;
    let mut removed = Vec::new();
    for entry in chosen {
        std::fs::remove_dir_all(&entry.path).context(entry.path.display())?;
        removed.push(format!("{}/{}", entry.name, entry.key));
    }
    cache.remove_unshared();
    Ok(removed)
}

/// Keeps the `keep` most recently used entries of each component and work
/// directories of each recipe, and deletes the rest and builds that never
/// finished. Kernel trees are kept: they hold editable source. Returns the
/// bytes freed.
pub fn prune(cache: &Components, keep: usize, say: &dyn Fn(&str)) -> Result<u64> {
    if keep < 1 {
        return Err(invalid("--keep must be at least 1"));
    }
    let before;
    {
        let _held = cache.lock(true, say)?;
        before = on_disk(&cache.dir()) + on_disk(&cache.work());
        let mut kept = std::collections::BTreeMap::new();
        for entry in cache.entries().iter().rev() {
            let count = kept.entry(entry.name.clone()).or_insert(0usize);
            if entry.key.starts_with("build-") || *count >= keep {
                std::fs::remove_dir_all(&entry.path).context(entry.path.display())?;
                say(&format!("Removed {}/{}", entry.name, entry.key));
                continue;
            }
            *count += 1;
            // Entries made before another had the same file share it now.
            if let Ok(recorded) = records(&entry.path) {
                cache.share(&entry.path, &recorded);
            }
        }
        cache.remove_unshared();
        cache.clear_entry_locks();
        let mut used = std::collections::BTreeMap::new();
        for entry in cache.work_entries().iter().rev() {
            let count = used.entry(entry.name.clone()).or_insert(0usize);
            *count += 1;
            if *count > keep {
                std::fs::remove_dir_all(&entry.path).context(entry.path.display())?;
                say(&format!("Removed work {}/{}", entry.name, entry.key));
            }
        }
    }
    let after = on_disk(&cache.dir()) + on_disk(&cache.work());
    say(&format!(
        "Freed {}; {} on disk in {}",
        human(before.saturating_sub(after)),
        human(after),
        cache.root().display()
    ));
    fit(cache, budget(), say);
    trim_vm(say);
    Ok(before.saturating_sub(after))
}

/// Deletes least recently used entries until the build cache fits in
/// `limit` bytes; 0 is no limit. Never waits for a running build: what it
/// uses is skipped. Returns the bytes still in use.
pub fn fit(cache: &Components, limit: u64, say: &dyn Fn(&str)) -> u64 {
    let idle = cache.try_lock().ok().flatten();
    if idle.is_some() {
        sweep(cache);
    }
    let start = usage(cache);
    if limit == 0 || start <= limit {
        return start;
    }
    let mut used = start;
    // (last use, path, label, whether it is a kernel tree)
    let mut candidates: Vec<(SystemTime, PathBuf, String, bool)> = Vec::new();
    if idle.is_some() {
        // Component entries and work directories, all but the newest of each.
        for (rows, kind) in [(cache.entries(), ""), (cache.work_entries(), "work ")] {
            for (index, entry) in rows.iter().enumerate() {
                let newest = rows
                    .get(index + 1)
                    .is_none_or(|next| next.name != entry.name);
                if !newest {
                    candidates.push((
                        entry.used,
                        entry.path.clone(),
                        format!("{kind}{}/{}", entry.name, short(&entry.key)),
                        false,
                    ));
                }
            }
        }
    }
    let trees = kernel_trees(cache);
    for tree in trees
        .iter()
        .take(trees.len().saturating_sub(1))
        .filter(|t| t.clean)
    {
        candidates.push((
            tree.used,
            tree.path.clone(),
            format!("kernel tree {}", name_of(&tree.path)),
            true,
        ));
    }
    candidates.sort_by_key(|candidate| candidate.0);
    for (_, path, label, tree) in candidates {
        if used <= limit {
            break;
        }
        let size = on_disk(&path);
        // A kernel tree in use by a build is locked; it is skipped.
        let removed = match tree {
            true => remove_tree(&path),
            false => std::fs::remove_dir_all(&path).is_ok(),
        };
        if removed {
            used = used.saturating_sub(size);
            say(&format!(
                "Removed {label} ({}, least recently used)",
                human(size)
            ));
        }
    }
    if idle.is_some() {
        cache.remove_unshared();
    }
    let used = usage(cache);
    if used < start {
        say(&format!(
            "Freed {}; the build cache uses {} of its {} limit",
            human(start - used),
            human(used),
            human(limit)
        ));
    }
    if used > limit {
        let edited: Vec<String> = kernel_trees(cache)
            .iter()
            .filter(|t| !t.clean)
            .map(|t| name_of(&t.path))
            .collect();
        let note = match edited.is_empty() {
            true => String::new(),
            false => format!(
                "; kernel trees with local edits are kept: {}",
                edited.join(", ")
            ),
        };
        say(&format!(
            "The build cache uses {}, over its {} limit{note}. See orchard2 cache ls; ORCHARD_CACHE_BUDGET_GB changes the limit",
            human(used),
            human(limit)
        ));
    }
    used
}

/// On macOS, gives the Podman VM's freed space back to macOS; deletes
/// nothing. What is deleted inside the VM stays allocated in its disk file
/// until the VM trims it.
pub fn trim_vm(say: &dyn Fn(&str)) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let podman = |args: &[&str]| {
        std::process::Command::new("podman")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let Ok(state) = podman(&["machine", "inspect", "--format", "{{.State}}"]) else {
        return;
    };
    if !state.status.success() || String::from_utf8_lossy(&state.stdout).trim() != "running" {
        say("The Podman VM is not running; nothing to trim");
        return;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let before = free_space(&home).unwrap_or(0);
    match podman(&["machine", "ssh", "sudo", "fstrim", "-a"]) {
        Ok(trimmed) if trimmed.status.success() => {
            let given = free_space(&home).unwrap_or(0).saturating_sub(before);
            say(&format!(
                "Trimmed the Podman VM: {} given back to macOS",
                human(given)
            ));
        }
        _ => say("Could not trim the Podman VM"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_cache::component::Key;
    use std::cell::RefCell;
    use std::time::Duration;

    fn quiet(_: &str) {}

    fn cache() -> (tempfile::TempDir, Components) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Components::open(dir.path()).unwrap();
        (dir, cache)
    }

    /// An entry of `size` bytes last used `age` hours ago.
    fn entry(cache: &Components, name: &str, key: &str, size: usize, age: u64) -> PathBuf {
        let content = format!("{name}{key}").repeat(size).into_bytes();
        let found = cache
            .cached(
                name,
                &mut || Ok(Key::plain(key)),
                &mut |stage| Ok(std::fs::write(stage.join("f"), &content[..size])?),
                &quiet,
            )
            .unwrap();
        let at = SystemTime::now() - Duration::from_secs(age * 3600);
        std::fs::File::open(&found.path)
            .unwrap()
            .set_modified(at)
            .unwrap();
        found.path
    }

    /// Fits until `done`: another test's child holds a copy of a released
    /// lock for the moment between its fork and its exec.
    fn fit_until(cache: &Components, limit: u64, done: &dyn Fn() -> bool) {
        for _ in 0..200 {
            fit(cache, limit, &quiet);
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the cache never became free");
    }

    #[test]
    fn sizes_read_as_people_expect() {
        assert_eq!(human(512), "512B");
        assert_eq!(human(1536), "1.5K");
        assert_eq!(human(5 << 20), "5.0M");
        assert_eq!(human(3 << 30), "3.0G");
        assert_eq!(human(2048u64 << 30), "2048.0G");
    }

    #[test]
    fn fitting_removes_the_least_recently_used_but_never_the_newest() {
        let (_dir, cache) = cache();
        let old = entry(&cache, "kernel", "old", 4000, 3);
        let mid = entry(&cache, "kernel", "mid", 4000, 2);
        let new = entry(&cache, "kernel", "new", 4000, 1);
        let only = entry(&cache, "tools", "only", 4000, 9);
        let said = RefCell::new(Vec::new());
        let used = fit(&cache, 9000, &|line| {
            said.borrow_mut().push(line.to_owned())
        });
        assert!(
            !old.exists() && !mid.exists() && new.exists() && only.exists(),
            "{:?}",
            said.borrow()
        );
        assert!(used <= 9000, "{used}");
        // Nothing more can go: the newest of each component stays.
        let used = fit(&cache, 1000, &|line| {
            said.borrow_mut().push(line.to_owned())
        });
        assert!(new.exists() && only.exists() && used > 1000);
        assert!(
            said.borrow().iter().any(|l| l.contains("over its")),
            "{:?}",
            said.borrow()
        );
        // No limit removes nothing.
        entry(&cache, "kernel", "more", 4000, 5);
        fit(&cache, 0, &quiet);
        assert_eq!(cache.entries().len(), 3);
    }

    #[test]
    fn a_running_build_keeps_its_entries() {
        let (_dir, cache) = cache();
        let old = entry(&cache, "kernel", "old", 4000, 3);
        entry(&cache, "kernel", "new", 4000, 1);
        let building = cache.lock(false, &quiet).unwrap();
        fit(&cache, 5000, &quiet);
        assert!(old.exists());
        drop(building);
        fit_until(&cache, 5000, &|| !old.exists());
    }

    /// A kernel tree as a build leaves it, last used `age` hours ago.
    fn tree(cache: &Components, name: &str, age: u64) -> PathBuf {
        let path = cache.kernel_work().join(name);
        let source = path.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("file.c"), "int a;\n").unwrap();
        let git = |args: &[&str]| sources::git(&source, args, &[]).unwrap();
        let user = ["-c", "user.name=t", "-c", "user.email=t@example.org"];
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&[&user[..], &["commit", "-q", "-m", "base"]].concat());
        // A build's tree is a detached checkout without branches or history.
        git(&["checkout", "-q", "--detach"]);
        git(&["branch", "-q", "-D", "main"]);
        std::fs::write(source.join("patched.c"), "int b;\n").unwrap();
        let prepared = sources::working_tree(&source, true).unwrap();
        std::fs::write(path.join("prepared-tree"), format!("{prepared}\n")).unwrap();
        let at = SystemTime::now() - Duration::from_secs(age * 3600);
        std::fs::File::open(path.join("prepared-tree"))
            .unwrap()
            .set_modified(at)
            .unwrap();
        path
    }

    #[test]
    fn an_idle_kernel_tree_goes_but_never_one_with_work_in_it() {
        let user = ["-c", "user.name=t", "-c", "user.email=t@example.org"];
        type Work = fn(&Path, &[&str]);
        let works: [(&str, Work); 5] = [
            ("edited", |source, _| {
                std::fs::write(source.join("file.c"), "int c;\n").unwrap()
            }),
            ("staged", |source, _| {
                std::fs::write(source.join("file.c"), "int c;\n").unwrap();
                sources::git(source, &["add", "file.c"], &[]).unwrap();
                std::fs::write(source.join("file.c"), "int a;\n").unwrap();
            }),
            ("stashed", |source, user| {
                std::fs::write(source.join("file.c"), "int c;\n").unwrap();
                sources::git(
                    source,
                    &[user, &["stash", "push", "-q", "file.c"]].concat(),
                    &[],
                )
                .unwrap();
            }),
            ("branch", |source, user| {
                sources::git(source, &["checkout", "-q", "-b", "mine"], &[]).unwrap();
                sources::git(
                    source,
                    &[user, &["commit", "-q", "--allow-empty", "-m", "work"]].concat(),
                    &[],
                )
                .unwrap();
                sources::git(source, &["checkout", "-q", "--detach", "HEAD^"], &[]).unwrap();
            }),
            ("detached commit", |source, user| {
                sources::git(
                    source,
                    &[user, &["commit", "-q", "--allow-empty", "-m", "work"]].concat(),
                    &[],
                )
                .unwrap();
                sources::git(source, &["checkout", "-q", "--detach", "HEAD^"], &[]).unwrap();
            }),
        ];
        for (what, work) in works {
            let (_dir, cache) = cache();
            let old = tree(&cache, "apple-old", 5);
            tree(&cache, "apple-new", 1);
            assert!(is_clean(&old), "{what}");
            work(&old.join("source"), &user);
            assert!(!is_clean(&old), "{what}");
            fit(&cache, 1, &quiet);
            assert!(old.join("source/file.c").exists(), "{what}");
        }
        let (_dir, cache) = cache();
        let old = tree(&cache, "apple-old", 5);
        let new = tree(&cache, "apple-new", 1);
        let building = try_lock(&old.join(".lock")).unwrap().unwrap();
        fit(&cache, 1, &quiet);
        assert!(old.exists(), "in use");
        drop(building);
        fit_until(&cache, 1, &|| !old.exists());
        assert!(new.exists());
        let left: Vec<_> = std::fs::read_dir(cache.kernel_work())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn what_an_interrupted_build_left_goes_and_never_counts_as_newest() {
        let (_dir, cache) = cache();
        let good = entry(&cache, "kernel", "good", 4000, 2);
        let left = cache.dir().join("kernel/build-abc");
        std::fs::create_dir_all(left.join("component")).unwrap();
        let tree = tree(&cache, "apple-new", 1);
        for name in ["base-x", "modules-x", "source-x"] {
            std::fs::create_dir_all(tree.join(name)).unwrap();
        }
        let fetch = cache.root().join("recipes/.fetch-x");
        std::fs::create_dir_all(&fetch).unwrap();
        // A running build keeps its own staging directories.
        let building = cache.lock(false, &quiet).unwrap();
        fit(&cache, 1, &quiet);
        assert!(left.exists() && tree.join("base-x").exists() && fetch.exists());
        drop(building);
        fit_until(&cache, 1, &|| !left.exists() && !fetch.exists());
        assert!(good.exists());
        assert!(tree.join("source").exists());
        for name in ["base-x", "modules-x", "source-x"] {
            assert!(!tree.join(name).exists());
        }
    }

    #[test]
    fn pruning_keeps_the_newest_of_each_and_drops_unfinished_builds() {
        let (_dir, cache) = cache();
        std::env::set_var("ORCHARD_CACHE_BUDGET_GB", "0");
        let old = entry(&cache, "kernel", "old", 100, 3);
        let new = entry(&cache, "kernel", "new", 100, 1);
        let other = entry(&cache, "tools", "only", 100, 9);
        let unfinished = cache.dir().join("kernel/build-abc");
        std::fs::create_dir_all(&unfinished).unwrap();
        for (key, age) in [("w1", 2), ("w2", 1)] {
            let work = cache.recipe_work("m1n1", key).unwrap();
            std::fs::create_dir_all(&work).unwrap();
            let at = SystemTime::now() - Duration::from_secs(age * 3600);
            std::fs::File::open(&work)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        assert!(prune(&cache, 0, &quiet).is_err());
        prune(&cache, 1, &quiet).unwrap();
        assert!(!old.exists() && new.exists() && other.exists() && !unfinished.exists());
        let work: Vec<String> = cache.work_entries().into_iter().map(|e| e.key).collect();
        assert_eq!(work, ["w2"]);
    }

    #[test]
    fn entries_are_removed_by_component_or_key_prefix() {
        let (_dir, cache) = cache();
        entry(&cache, "kernel", "aaa111", 10, 2);
        entry(&cache, "kernel", "aab222", 10, 1);
        entry(&cache, "tools", "ccc", 10, 1);
        let names = |selectors: &[&str]| {
            let selectors: Vec<String> = selectors.iter().map(|s| (*s).to_owned()).collect();
            remove(&cache, &selectors, &quiet)
        };
        assert_eq!(names(&["kernel/aa"]).unwrap_err().code, Code::Ambiguous);
        assert_eq!(names(&["nope"]).unwrap_err().code, Code::NotFound);
        assert_eq!(names(&["kernel/aab"]).unwrap(), ["kernel/aab222"]);
        assert_eq!(
            names(&["kernel", "tools/c"]).unwrap(),
            ["kernel/aaa111", "tools/ccc"]
        );
        assert!(cache.entries().is_empty());
    }
}

use crate::args::{CacheCommand, Global};
use crate::output::{Output, Report};
use orchard_build::manage::{self, human};
use orchard_cache::component::{on_disk, Components};
use orchard_cache::store::{Entry, Store};
use orchard_cache::Result;
use serde::Serialize;
use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
struct Cache {
    dir: String,
    files: Vec<File>,
    size: u64,
    /// Whether the files were just deleted.
    cleared: bool,
    /// Build components, oldest use first within each name.
    components: Vec<Component>,
    /// The recipes' work directories.
    work: Vec<Component>,
    kernel_trees: Vec<KernelTree>,
    /// Bytes the build cache takes on disk, and its limit (0: none).
    build_size: u64,
    build_limit: u64,
    /// What a `rm` or `prune` deleted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    removed: Vec<String>,
}

#[derive(Serialize)]
struct File {
    name: String,
    size: u64,
}

#[derive(Serialize)]
struct Component {
    name: String,
    key: String,
    /// Bytes on disk; files shared with other entries count in each.
    size: u64,
    /// Last use, in Unix seconds.
    used: u64,
    /// What it was made from, in a few words.
    summary: String,
}

#[derive(Serialize)]
struct KernelTree {
    name: String,
    size: u64,
    used: u64,
    /// No local edits: it can be made again.
    clean: bool,
}

fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Hours or days since `time`, for a listing.
fn ago(time: u64) -> String {
    let passed = seconds(SystemTime::now()).saturating_sub(time);
    match passed {
        0..=3599 => format!("{}m ago", passed / 60),
        3600..=172_799 => format!("{}h ago", passed / 3600),
        _ => format!("{}d ago", passed / 86400),
    }
}

impl Report for Cache {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        for removed in &self.removed {
            let _ = writeln!(text, "Removed {removed}");
        }
        if !self.cleared && self.removed.is_empty() {
            for f in &self.files {
                let _ = writeln!(text, "{:>12}  {}", f.size, f.name);
            }
        }
        let verb = if self.cleared { "deleted" } else { "cached" };
        let _ = writeln!(
            text,
            "{} files, {} bytes {verb} in {}",
            self.files.len(),
            self.size,
            self.dir
        );
        if self.cleared {
            return text.into_bytes();
        }
        for c in &self.components {
            let key = &c.key[..c.key.len().min(12)];
            let _ = writeln!(
                text,
                "{:28} {key:12} {:>7}  {:>8}  {}",
                c.name,
                human(c.size),
                ago(c.used),
                c.summary
            );
        }
        for w in &self.work {
            let key = &w.key[..w.key.len().min(12)];
            let _ = writeln!(
                text,
                "work {:23} {key:12} {:>7}  {:>8}",
                w.name,
                human(w.size),
                ago(w.used)
            );
        }
        for tree in &self.kernel_trees {
            let state = if tree.clean {
                "no local edits"
            } else {
                "local edits, kept"
            };
            let _ = writeln!(
                text,
                "kernel tree {:29} {:>7}  {:>8}  {state}",
                tree.name,
                human(tree.size),
                ago(tree.used)
            );
        }
        let limit = match self.build_limit {
            0 => "no limit".to_owned(),
            limit => format!("limit {}", human(limit)),
        };
        let _ = writeln!(
            text,
            "{} build components, {} on disk with work directories, kernel trees and git objects ({limit}; \
             ORCHARD_CACHE_BUDGET_GB)",
            self.components.len(),
            human(self.build_size)
        );
        text.into_bytes()
    }
}

fn say(text: &str) {
    eprintln!("{text}");
}

pub fn run(global: &Global, command: &CacheCommand) -> Result<Output> {
    let store = Store::open(global.cache.as_deref())?;
    let build = Components::open(store.dir())?;
    let mut removed = Vec::new();
    let (entries, cleared) = match command {
        CacheCommand::Ls => (store.entries()?, false),
        CacheCommand::Clear => (store.clear()?, true),
        CacheCommand::Rm { entries } => {
            removed = manage::remove(&build, entries, &say)?;
            (store.entries()?, false)
        }
        CacheCommand::Prune { keep } => {
            manage::prune(&build, *keep, &say)?;
            (store.entries()?, false)
        }
    };
    let size = entries.iter().map(|e| e.size).sum();
    let files = entries
        .into_iter()
        .map(|Entry { name, size }| File { name, size })
        .collect();
    let row = |entry: orchard_cache::component::Entry| Component {
        summary: manage::summary(&entry),
        size: on_disk(&entry.path),
        used: seconds(entry.used),
        name: entry.name,
        key: entry.key,
    };
    Ok(Cache {
        dir: store.dir().display().to_string(),
        files,
        size,
        cleared,
        components: build.entries().into_iter().map(row).collect(),
        work: build.work_entries().into_iter().map(row).collect(),
        kernel_trees: manage::kernel_trees(&build)
            .into_iter()
            .map(|tree| KernelTree {
                name: tree
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                size: on_disk(&tree.path),
                used: seconds(tree.used),
                clean: tree.clean,
            })
            .collect(),
        build_size: manage::usage(&build),
        build_limit: manage::budget(),
        removed,
    }
    .into())
}

use crate::args::{CacheCommand, Global};
use crate::output::{Output, Report};
use orchard_cache::store::{Entry, Store};
use orchard_cache::Result;
use serde::Serialize;
use std::fmt::Write;

#[derive(Serialize)]
struct Cache {
    dir: String,
    files: Vec<File>,
    size: u64,
    /// Whether the files were just deleted.
    cleared: bool,
}

#[derive(Serialize)]
struct File {
    name: String,
    size: u64,
}

impl Report for Cache {
    fn human(&self) -> Vec<u8> {
        let mut text = String::new();
        if !self.cleared {
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
        text.into_bytes()
    }
}

pub fn run(global: &Global, command: &CacheCommand) -> Result<Output> {
    let store = Store::open(global.cache.as_deref())?;
    let (entries, cleared) = match command {
        CacheCommand::Ls => (store.entries()?, false),
        CacheCommand::Clear => (store.clear()?, true),
    };
    let size = entries.iter().map(|e| e.size).sum();
    let files = entries
        .into_iter()
        .map(|Entry { name, size }| File { name, size })
        .collect();
    Ok(Cache {
        dir: store.dir().display().to_string(),
        files,
        size,
        cleared,
    }
    .into())
}

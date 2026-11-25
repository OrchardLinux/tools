//! Hashing recipe files: what a component's key is made from. Only recipes
//! are hashed, never build output or experiment files.
use crate::json;
use orchard_cache::component::{digest, Key};
use orchard_cache::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Directories that hold build output or tooling state, not recipes.
pub const IGNORE: &[&str] = &[".work", ".git", "__pycache__"];

/// `(relative path, "mode:sha256")` for the files under `paths`, in the
/// order they are hashed: the paths as given, a directory's own files by
/// name and then its directories by name.
pub fn entries(root: &Path, paths: &[&str], ignore: &[&str]) -> Result<Vec<(String, String)>> {
    let mut found: Vec<(String, String)> = Vec::new();
    for relative in paths {
        let start = root.join(relative);
        let mut files = Vec::new();
        if start.is_file() {
            files.push(start);
        } else if start.is_dir() {
            walk(&start, ignore, &mut files)?;
        }
        for path in files {
            // Git records only the executable bit, so the key does too:
            // builders with different umasks hash a checkout the same way.
            use std::os::unix::fs::PermissionsExt;
            let executable = std::fs::metadata(&path)
                .context(path.display())?
                .permissions()
                .mode()
                & 0o100
                != 0;
            let mode = if executable { 0o755 } else { 0o644 };
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let entry = format!("{mode}:{}", digest(&path)?);
            match found.iter_mut().find(|(known, _)| *known == name) {
                Some(slot) => slot.1 = entry,
                None => found.push((name, entry)),
            }
        }
    }
    Ok(found)
}

fn walk(directory: &Path, ignore: &[&str], files: &mut Vec<PathBuf>) -> Result<()> {
    let (mut names, mut directories) = (Vec::new(), Vec::new());
    for item in std::fs::read_dir(directory).context(directory.display())? {
        let item = item?;
        let name = item.file_name();
        if item.file_type()?.is_dir() {
            if !ignore.iter().any(|i| name == *i) {
                directories.push(name);
            }
        } else if item.path().is_dir() {
            // A link to a directory is not followed.
        } else {
            names.push(name);
        }
    }
    names.sort();
    directories.sort();
    files.extend(names.iter().map(|name| directory.join(name)));
    for name in directories {
        walk(&directory.join(name), ignore, files)?;
    }
    Ok(())
}

/// The entries as the JSON object key inputs hold.
pub fn object(root: &Path, paths: &[&str]) -> Result<Value> {
    Ok(Value::Object(
        entries(root, paths, IGNORE)?
            .into_iter()
            .map(|(name, entry)| (name, Value::String(entry)))
            .collect(),
    ))
}

/// One hash over the files under `paths`.
pub fn key(root: &Path, paths: &[&str], ignore: &[&str]) -> Result<String> {
    let mut hash = Sha256::new();
    for (relative, entry) in entries(root, paths, ignore)? {
        let (mode, checksum) = entry.split_once(':').expect("mode:sha256");
        hash.update(relative.as_bytes());
        hash.update([0]);
        hash.update(mode.as_bytes());
        hash.update([0]);
        hash.update(checksum.as_bytes());
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The key of a set of inputs, which it keeps for the cache record.
pub fn keyed(inputs: Value) -> Key {
    Key {
        hash: json::hash(&inputs),
        inputs: Some(inputs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn files_hash_in_orchards_order_with_only_the_executable_bit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, text) in [
            ("r/b", "b"),
            ("r/a/x", "x"),
            ("r/.work/skip", "s"),
            ("r/z", "z"),
            ("one", "1"),
        ] {
            std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            std::fs::write(root.join(name), text).unwrap();
        }
        std::fs::set_permissions(root.join("r/z"), std::fs::Permissions::from_mode(0o775)).unwrap();
        std::fs::set_permissions(root.join("r/b"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let found = entries(root, &["one", "r", "missing"], IGNORE).unwrap();
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["one", "r/b", "r/z", "r/a/x"]);
        assert!(found[1].1.starts_with("420:"), "{}", found[1].1);
        assert!(found[2].1.starts_with("493:"), "{}", found[2].1);
        // Nothing ignored: the experiment file counts.
        assert_eq!(entries(root, &["r"], &[]).unwrap().len(), 4);
        assert_ne!(
            key(root, &["r"], IGNORE).unwrap(),
            key(root, &["r"], &[]).unwrap()
        );
    }
}

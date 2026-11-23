//! Packs the companion built for iOS (companion/out, or the directory named
//! by ORCHARD_COMPANION, e.g. a macOS build's copy on a Linux builder) into
//! the installer. Without one the installer still builds; building the iPadOS
//! overlay then says the companion is missing.
use std::path::{Path, PathBuf};

fn files(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            files(&path, root, out);
        } else if let Ok(data) = std::fs::read(&path) {
            out.push((path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"), data));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=ORCHARD_COMPANION");
    let dir = std::env::var_os("ORCHARD_COMPANION")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../companion/out"));
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut list = Vec::new();
    if dir.join("orchard-companion").is_file() {
        files(&dir, &dir, &mut list);
    }
    list.sort();
    let entries: Vec<orchard_formats::tar::Entry> =
        list.iter().map(|(p, d)| orchard_formats::tar::Entry { path: p, mode: 0o644, data: d }).collect();
    let tar = if entries.is_empty() { Vec::new() } else { orchard_formats::tar::write(&entries).expect("companion archive") };
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("companion.tar");
    std::fs::write(out, tar).unwrap();
}

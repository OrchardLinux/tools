//! cpio archives in the "newc" format, as Linux unpacks them for an initramfs.
//!
//! Only writing is supported, and only directories and regular files. Every
//! entry belongs to root and has time 0, so the same files always give the
//! same bytes.
//!
//! Linux accepts several archives back to back, each one compressed or not,
//! and unpacks them over each other in order. Zero bytes may separate them,
//! but each uncompressed archive has to start at a multiple of four bytes
//! from the start of the whole initramfs. [`append`] adds a layer that way.
use crate::{Error, Result};
use std::collections::BTreeSet;

const MAGIC: &[u8] = b"070701";
const TRAILER: &str = "TRAILER!!!";
const DIRECTORY: u32 = 0o040755;
const REGULAR: u32 = 0o100644;

/// A regular file and its path inside the archive, such as
/// `lib/firmware/brcm/brcmfmac4355-pcie.bin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File<'a> {
    pub path: &'a str,
    pub data: &'a [u8],
}

/// An archive of `files`. Every directory above a file gets an entry of its
/// own, before any file, so the archive unpacks into an empty root. Files keep
/// the order they are given in.
pub fn write(files: &[File]) -> Result<Vec<u8>> {
    let mut paths = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for file in files {
        check_path(file.path)?;
        if !paths.insert(file.path) {
            return Err(Error("a cpio path appears twice"));
        }
        let mut parent = file.path;
        while let Some((directory, _)) = parent.rsplit_once('/') {
            directories.insert(directory);
            parent = directory;
        }
    }
    if directories
        .iter()
        .any(|directory| paths.contains(directory))
    {
        return Err(Error("a cpio path is both a file and a directory"));
    }

    let mut archive = Vec::new();
    let mut inode = 0u32;
    for directory in directories {
        inode += 1;
        entry(&mut archive, inode, DIRECTORY, 2, directory, &[])?;
    }
    for file in files {
        inode = inode
            .checked_add(1)
            .ok_or(Error("too many files for one cpio archive"))?;
        entry(&mut archive, inode, REGULAR, 1, file.path, file.data)?;
    }
    entry(&mut archive, 0, 0, 1, TRAILER, &[])?;
    Ok(archive)
}

/// An entry with its permission bits, for archives that need more than the
/// defaults of [`write`] (files 0644, directories 0755).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node<'a> {
    Directory { path: &'a str, mode: u32 },
    File { path: &'a str, mode: u32, data: &'a [u8] },
}

/// An archive of `nodes`, with their modes. Directories above a file that are
/// not listed get 0755. Listed directories keep their order, before any file.
pub fn write_nodes(nodes: &[Node]) -> Result<Vec<u8>> {
    let mut listed = std::collections::BTreeMap::new();
    let mut files = Vec::new();
    for node in nodes {
        match *node {
            Node::Directory { path, mode } => {
                check_path(path)?;
                if listed.insert(path, mode & 0o7777).is_some() {
                    return Err(Error("a cpio path appears twice"));
                }
            }
            Node::File { path, mode, data } => files.push((File { path, data }, mode & 0o7777)),
        }
    }
    let mut paths = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for (file, _) in &files {
        check_path(file.path)?;
        if !paths.insert(file.path) || listed.contains_key(file.path) {
            return Err(Error("a cpio path appears twice"));
        }
        let mut parent = file.path;
        while let Some((directory, _)) = parent.rsplit_once('/') {
            directories.insert(directory);
            parent = directory;
        }
    }
    directories.extend(listed.keys().copied());
    if directories.iter().any(|directory| paths.contains(directory)) {
        return Err(Error("a cpio path is both a file and a directory"));
    }
    let mut archive = Vec::new();
    let mut inode = 0u32;
    for directory in directories {
        inode += 1;
        let mode = 0o040000 | listed.get(directory).copied().unwrap_or(0o755);
        entry(&mut archive, inode, mode, 2, directory, &[])?;
    }
    for (file, mode) in files {
        inode = inode.checked_add(1).ok_or(Error("too many files for one cpio archive"))?;
        entry(&mut archive, inode, 0o100000 | mode, 1, file.path, file.data)?;
    }
    entry(&mut archive, 0, 0, 1, TRAILER, &[])?;
    Ok(archive)
}

/// Adds `files` to `initramfs` as one more archive, which Linux unpacks over
/// what is already there. `initramfs` may be compressed, uncompressed or
/// several archives already.
pub fn append(initramfs: &mut Vec<u8>, files: &[File]) -> Result<()> {
    let layer = write(files)?;
    initramfs.resize(initramfs.len().next_multiple_of(4), 0);
    initramfs.extend_from_slice(&layer);
    Ok(())
}

/// A path is relative, uses `/` between non-empty names, and has no `.` or
/// `..` names. Linux reads the name up to a zero byte, so none may be inside.
fn check_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\0')
        || path
            .split('/')
            .any(|name| name.is_empty() || name == "." || name == "..")
    {
        return Err(Error(
            "a cpio path is not a relative path of plain names joined by /",
        ));
    }
    if path == TRAILER {
        return Err(Error("a cpio path is the archive's end marker"));
    }
    Ok(())
}

/// One header, the name with its zero byte, and the data, each padded to
/// four bytes.
fn entry(
    archive: &mut Vec<u8>,
    inode: u32,
    mode: u32,
    links: u32,
    name: &str,
    data: &[u8],
) -> Result<()> {
    let size = u32::try_from(data.len()).map_err(|_| Error("a cpio file is over 4 GiB"))?;
    let name_size =
        u32::try_from(name.len() + 1).map_err(|_| Error("a cpio path is over 4 GiB"))?;
    // inode, mode, uid, gid, links, mtime, size, device major and minor,
    // special-file device major and minor, name size, checksum (unused).
    let fields = [inode, mode, 0, 0, links, 0, size, 0, 0, 0, 0, name_size, 0];
    archive.extend_from_slice(MAGIC);
    for field in fields {
        archive.extend_from_slice(format!("{field:08x}").as_bytes());
    }
    archive.extend_from_slice(name.as_bytes());
    archive.push(0);
    archive.resize(archive.len().next_multiple_of(4), 0);
    archive.extend_from_slice(data);
    archive.resize(archive.len().next_multiple_of(4), 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Entry {
        inode: u32,
        mode: u32,
        links: u32,
        name: String,
        data: Vec<u8>,
    }

    /// Reads entries the way Linux does, checking alignment and padding,
    /// until the trailer, which must end the archive.
    fn read(archive: &[u8]) -> Vec<Entry> {
        assert_eq!(archive.len() % 4, 0);
        let mut entries = Vec::new();
        let mut at = 0;
        loop {
            let header = &archive[at..at + 110];
            assert_eq!(&header[..6], MAGIC);
            let field = |n: usize| {
                let hex = std::str::from_utf8(&header[6 + n * 8..14 + n * 8]).unwrap();
                u32::from_str_radix(hex, 16).unwrap()
            };
            for unused in [2, 3, 5, 7, 8, 9, 10, 12] {
                assert_eq!(field(unused), 0);
            }
            let (size, name_size) = (field(6) as usize, field(11) as usize);
            let name_end = at + 110 + name_size;
            assert_eq!(archive[name_end - 1], 0);
            let name = std::str::from_utf8(&archive[at + 110..name_end - 1]).unwrap();
            let data_at = name_end.next_multiple_of(4);
            assert!(archive[name_end..data_at].iter().all(|&b| b == 0));
            let next = (data_at + size).next_multiple_of(4);
            assert!(archive[data_at + size..next].iter().all(|&b| b == 0));
            if name == TRAILER {
                assert_eq!(next, archive.len());
                return entries;
            }
            entries.push(Entry {
                inode: field(0),
                mode: field(1),
                links: field(4),
                name: name.to_owned(),
                data: archive[data_at..data_at + size].to_vec(),
            });
            at = next;
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn directories_first_then_files_in_order() {
        let files = [
            File {
                path: "lib/firmware/brcm/b.bin",
                data: b"bee",
            },
            File {
                path: "lib/firmware/a.txt",
                data: b"",
            },
            File {
                path: "init",
                data: b"#!/bin/sh\n",
            },
        ];
        let entries = read(&write(&files).unwrap());
        assert_eq!(
            names(&entries),
            [
                "lib",
                "lib/firmware",
                "lib/firmware/brcm",
                "lib/firmware/brcm/b.bin",
                "lib/firmware/a.txt",
                "init",
            ]
        );
        for directory in &entries[..3] {
            assert_eq!((directory.mode, directory.links), (DIRECTORY, 2));
            assert!(directory.data.is_empty());
        }
        for (entry, file) in entries[3..].iter().zip(&files) {
            assert_eq!((entry.mode, entry.links), (REGULAR, 1));
            assert_eq!(entry.data, file.data);
        }
        let inodes: BTreeSet<_> = entries.iter().map(|e| e.inode).collect();
        assert_eq!(inodes.len(), entries.len());
        assert!(!inodes.contains(&0));
    }

    #[test]
    fn exact_bytes_of_one_file() {
        let archive = write(&[File {
            path: "a",
            data: b"xyz",
        }])
        .unwrap();
        let header =
            |fields: [u32; 13]| -> String { fields.iter().map(|f| format!("{f:08x}")).collect() };
        let expected = [
            b"070701".as_slice(),
            header([1, REGULAR, 0, 0, 1, 0, 3, 0, 0, 0, 0, 2, 0]).as_bytes(),
            b"a\0", // 110 + 2 bytes: already a multiple of 4
            b"xyz\0",
            b"070701",
            header([0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 11, 0]).as_bytes(),
            b"TRAILER!!!\0\0\0\0", // 110 + 11 bytes, padded to 124
        ]
        .concat();
        assert_eq!(archive, expected);
    }

    #[test]
    fn no_files_is_just_the_trailer() {
        assert!(read(&write(&[]).unwrap()).is_empty());
    }

    #[test]
    fn same_files_same_bytes() {
        let files = [File {
            path: "etc/hostname",
            data: b"orchard\n",
        }];
        assert_eq!(write(&files).unwrap(), write(&files).unwrap());
    }

    #[test]
    fn append_starts_the_layer_on_four_bytes() {
        for base_size in 0..8 {
            let mut initramfs = vec![0x1f; base_size];
            append(
                &mut initramfs,
                &[File {
                    path: "f",
                    data: b"1",
                }],
            )
            .unwrap();
            let at = base_size.next_multiple_of(4);
            assert_eq!(&initramfs[..base_size], vec![0x1f; base_size]);
            assert!(initramfs[base_size..at].iter().all(|&b| b == 0));
            assert_eq!(names(&read(&initramfs[at..])), ["f"]);
        }
    }

    #[test]
    fn paths_that_cannot_be_written() {
        let fails = |path: &str| write(&[File { path, data: b"" }]).is_err();
        for path in [
            "",
            "/etc/passwd",
            "etc/",
            "etc//passwd",
            "./init",
            "lib/../init",
            "..",
            "nul\0byte",
            TRAILER,
        ] {
            assert!(fails(path), "{path:?}");
        }
        assert!(!fails("lib/firmware/x.bin"));
    }

    #[test]
    fn conflicting_paths() {
        let file = |path| File { path, data: b"" };
        assert!(write(&[file("init"), file("init")]).is_err());
        assert!(write(&[file("lib"), file("lib/x")]).is_err());
        assert!(write(&[file("lib/x"), file("lib")]).is_err());
        assert!(write(&[file("lib/x"), file("lib/y")]).is_ok());
    }

    #[test]
    fn append_leaves_the_initramfs_alone_on_error() {
        let mut initramfs = b"abc".to_vec();
        assert!(append(
            &mut initramfs,
            &[File {
                path: "/x",
                data: b""
            }]
        )
        .is_err());
        assert_eq!(initramfs, b"abc");
    }

    #[test]
    fn nodes_keep_their_modes() {
        let archive = write_nodes(&[
            Node::Directory { path: "root", mode: 0o700 },
            Node::File { path: "install/dropbear", mode: 0o755, data: b"x" },
            Node::File { path: "install/host_key", mode: 0o600, data: b"k" },
        ])
        .unwrap();
        let modes: Vec<(String, u32)> = read(&archive).into_iter().map(|e| (e.name, e.mode)).collect();
        assert_eq!(
            modes,
            [
                ("install".into(), 0o040755),
                ("root".into(), 0o040700),
                ("install/dropbear".into(), 0o100755),
                ("install/host_key".into(), 0o100600),
            ]
        );
        assert!(write_nodes(&[Node::Directory { path: "a", mode: 0o755 }, Node::File { path: "a", mode: 0o644, data: b"" }]).is_err());
    }
}

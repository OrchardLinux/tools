//! Writes a new HFSX volume (case-sensitive HFS+) from a list of entries,
//! after Apple's TN1150. Made for small read-only images such as palera1n's
//! overlay: every file is one contiguous extent, there is no journal, and
//! the extents and attributes B-trees are empty. HFSX compares names as
//! plain UTF-16 code units, so no Unicode case-folding is involved.
//!
//! The same entries and options always give the same bytes.
use crate::hfs::{Entry, Kind, Meta};
use crate::{Error, Result};
use std::collections::BTreeMap;

const HEADER_OFFSET: usize = 1024;
const ROOT_PARENT: u32 = 1;
const ROOT: u32 = 2;
const FIRST_USER: u32 = 16;
const NODE: usize = 4096;
const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

/// The volume as a whole.
#[derive(Debug, Clone)]
pub struct Options {
    pub name: String,
    pub block_size: u32,
    /// The volume's dates (seconds since 1904).
    pub created: u32,
    pub modified: u32,
    /// The volume header's Finder info; bytes 24..32 are the volume's
    /// 64-bit identifier.
    pub finder: [u8; 32],
    /// The smallest image to make, in bytes; it grows to fit the entries.
    pub min_size: u64,
    /// Free space to leave beyond the entries, in bytes.
    pub slack: u64,
}

struct Node {
    cnid: u32,
    parent: u32,
    name: Vec<u16>,
    entry: Entry,
    /// First block and block count of the contents.
    extent: (u32, u32),
    valence: u32,
    /// Subfolders, which HFSX keeps per folder (kHFSHasFolderCountMask).
    folders: u32,
}

fn be16(out: &mut [u8], at: usize, v: u16) {
    out[at..at + 2].copy_from_slice(&v.to_be_bytes());
}
fn be32(out: &mut [u8], at: usize, v: u32) {
    out[at..at + 4].copy_from_slice(&v.to_be_bytes());
}
fn be64(out: &mut [u8], at: usize, v: u64) {
    out[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

fn data_of(entry: &Entry) -> &[u8] {
    match &entry.kind {
        Kind::Folder => &[],
        Kind::File(d) | Kind::Symlink(d) => d,
    }
}

/// A catalog key: parent CNID, then the name as length and UTF-16BE units.
fn catalog_key(parent: u32, name: &[u16]) -> Vec<u8> {
    let mut key = Vec::with_capacity(6 + 2 * name.len());
    key.extend(parent.to_be_bytes());
    key.extend((name.len() as u16).to_be_bytes());
    for unit in name {
        key.extend(unit.to_be_bytes());
    }
    key
}

/// An 80-byte fork record with one extent.
fn fork(out: &mut [u8], at: usize, size: u64, clump: u32, extent: (u32, u32)) {
    be64(out, at, size);
    be32(out, at + 8, clump);
    be32(out, at + 12, extent.1);
    if extent.1 > 0 {
        be32(out, at + 16, extent.0);
        be32(out, at + 20, extent.1);
    }
}

/// The shared first 80 bytes of folder and file records.
fn common(out: &mut [u8], kind: u16, flags: u16, cnid: u32, meta: &Meta) {
    be16(out, 0, kind);
    be16(out, 2, flags);
    be32(out, 8, cnid);
    be32(out, 12, meta.created);
    be32(out, 16, meta.modified);
    be32(out, 20, meta.attributes_modified);
    be32(out, 24, meta.accessed);
    be32(out, 28, meta.backed_up);
    be32(out, 32, meta.owner);
    be32(out, 36, meta.group);
    out[40] = meta.admin_flags;
    out[41] = meta.owner_flags;
    be16(out, 42, meta.mode);
    out[48..80].copy_from_slice(&meta.finder);
}

fn catalog_records(nodes: &BTreeMap<u32, Node>, block_size: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut records = Vec::new();
    for node in nodes.values() {
        let meta = &node.entry.meta;
        let (value, thread_kind) = match &node.entry.kind {
            Kind::Folder => {
                let mut v = vec![0u8; 88];
                common(&mut v, 1, 0x0010, node.cnid, meta);
                be32(&mut v, 4, node.valence);
                be32(&mut v, 84, node.folders);
                (v, 3u16)
            }
            Kind::File(data) | Kind::Symlink(data) => {
                let mut v = vec![0u8; 248];
                // kHFSThreadExistsMask
                common(&mut v, 2, 0x0002, node.cnid, meta);
                fork(&mut v, 88, data.len() as u64, block_size, node.extent);
                (v, 4u16)
            }
        };
        records.push((catalog_key(node.parent, &node.name), value));
        let mut thread = vec![0u8; 10 + 2 * node.name.len()];
        be16(&mut thread, 0, thread_kind);
        be32(&mut thread, 4, node.parent);
        be16(&mut thread, 8, node.name.len() as u16);
        for (i, unit) in node.name.iter().enumerate() {
            be16(&mut thread, 10 + 2 * i, *unit);
        }
        records.push((catalog_key(node.cnid, &[]), thread));
    }
    // HFSX binary order: parent, then the name's code units.
    records.sort_by(|a, b| {
        let pa = u32::from_be_bytes(a.0[..4].try_into().unwrap());
        let pb = u32::from_be_bytes(b.0[..4].try_into().unwrap());
        let units = |k: &[u8]| -> Vec<u16> { k[6..].chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect() };
        (pa, units(&a.0)).cmp(&(pb, units(&b.0)))
    });
    records
}

/// The parameters of a B-tree's header record.
struct TreeKind {
    max_key: u16,
    compare: u8,
    attributes: u32,
}

const CATALOG: TreeKind = TreeKind { max_key: 516, compare: 0xbc, attributes: 6 };
const EXTENTS: TreeKind = TreeKind { max_key: 10, compare: 0, attributes: 2 };
const ATTRIBUTES: TreeKind = TreeKind { max_key: 266, compare: 0, attributes: 6 };

/// A node: descriptor, records, then the offset table from the end.
fn node(kind: u8, height: u8, records: &[Vec<u8>], links: (u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8; NODE];
    be32(&mut out, 0, links.0);
    be32(&mut out, 4, links.1);
    out[8] = kind;
    out[9] = height;
    be16(&mut out, 10, records.len() as u16);
    let mut at = 14;
    for (i, record) in records.iter().enumerate() {
        out[at..at + record.len()].copy_from_slice(record);
        be16(&mut out, NODE - 2 * (i + 1), at as u16);
        at += record.len();
    }
    be16(&mut out, NODE - 2 * (records.len() + 1), at as u16);
    out
}

fn fits(records: &[Vec<u8>], next: &[u8]) -> bool {
    let used: usize = records.iter().map(Vec::len).sum();
    14 + used + next.len() + 2 * (records.len() + 2) <= NODE
}

/// A tree level: each node's records and its first key.
type Level = Vec<(Vec<Vec<u8>>, Vec<u8>)>;

/// Packs leaf records into a B-tree: node 0 the header, then leaves, then
/// index levels up to the root. Returns the tree's bytes.
fn btree(records: &[(Vec<u8>, Vec<u8>)], kind: &TreeKind) -> Result<Vec<u8>> {
    let with_length = |key: &[u8]| -> Vec<u8> {
        let mut r = (key.len() as u16).to_be_bytes().to_vec();
        r.extend(key);
        r
    };
    // Each level: nodes as (records, first key).
    let mut levels: Vec<Level> = Vec::new();
    let mut leaves: Level = Vec::new();
    for (key, value) in records {
        let mut record = with_length(key);
        record.extend(value);
        if record.len() % 2 != 0 {
            return Err(Error("odd-sized HFS record"));
        }
        if leaves.last().is_none_or(|(r, _)| !fits(r, &record)) {
            leaves.push((Vec::new(), key.clone()));
        }
        leaves.last_mut().unwrap().0.push(record);
    }
    if !leaves.is_empty() {
        levels.push(leaves);
    }
    // Node numbers: leaves from 1, then each index level after the last.
    let mut first_number = vec![];
    let mut next = 1u32;
    while let Some(level) = levels.last() {
        first_number.push(next);
        next += level.len() as u32;
        if level.len() == 1 {
            break;
        }
        let mut index: Level = Vec::new();
        let base = *first_number.last().unwrap();
        for (i, (_, first_key)) in level.iter().enumerate() {
            let mut record = with_length(first_key);
            record.extend((base + i as u32).to_be_bytes());
            if index.last().is_none_or(|(r, _)| !fits(r, &record)) {
                index.push((Vec::new(), first_key.clone()));
            }
            index.last_mut().unwrap().0.push(record);
        }
        levels.push(index);
    }
    let total = next;
    let map_bytes = NODE - 14 - 106 - 128 - 8;
    if total as usize > map_bytes * 8 {
        return Err(Error("HFS B-tree too large for one map record"));
    }
    let depth = levels.len() as u16;
    let leaf_count: u32 = records.len() as u32;
    let (root, first_leaf, last_leaf) = match (levels.first(), levels.last()) {
        (Some(leaves), Some(_)) => (next - 1, 1, leaves.len() as u32),
        _ => (0, 0, 0),
    };
    let mut header = vec![0u8; 106];
    be16(&mut header, 0, depth);
    be32(&mut header, 2, root);
    be32(&mut header, 6, leaf_count);
    be32(&mut header, 10, first_leaf);
    be32(&mut header, 14, last_leaf);
    be16(&mut header, 18, NODE as u16);
    be16(&mut header, 20, kind.max_key);
    be32(&mut header, 22, total);
    be32(&mut header, 26, 0);
    be32(&mut header, 32, NODE as u32);
    header[36] = 0;
    header[37] = kind.compare;
    be32(&mut header, 38, kind.attributes);
    let mut map = vec![0u8; map_bytes];
    for n in 0..total as usize {
        map[n / 8] |= 0x80 >> (n % 8);
    }
    let mut out = node(1, 0, &[header, vec![0u8; 128], map], (0, 0));
    for (level_index, level) in levels.iter().enumerate() {
        let base = first_number[level_index];
        let leaf = level_index == 0;
        for (i, (recs, _)) in level.iter().enumerate() {
            let links = if leaf {
                let forward = if i + 1 < level.len() { base + i as u32 + 1 } else { 0 };
                let back = if i > 0 { base + i as u32 - 1 } else { 0 };
                (forward, back)
            } else {
                (0, 0)
            };
            out.extend(node(if leaf { 0xff } else { 0 }, level_index as u8 + 1, recs, links));
        }
    }
    Ok(out)
}

/// The volume's bytes.
pub fn write(entries: &[Entry], options: &Options) -> Result<Vec<u8>> {
    let bs = options.block_size;
    if bs < 512 || !bs.is_power_of_two() || !NODE.is_multiple_of(bs as usize) && !(bs as usize).is_multiple_of(NODE) {
        return Err(Error("unsupported HFS block size"));
    }
    let name: Vec<u16> = options.name.encode_utf16().collect();
    if name.is_empty() || name.len() > 255 {
        return Err(Error("bad HFS volume name"));
    }
    let root = entries.iter().find(|e| e.path == "/").ok_or(Error("no root folder among the HFS entries"))?;
    if root.kind != Kind::Folder {
        return Err(Error("the HFS root is not a folder"));
    }
    // CNIDs in path order, so the same entries always give the same image.
    let mut sorted: Vec<&Entry> = entries.iter().filter(|e| e.path != "/").collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut by_path: BTreeMap<&str, u32> = BTreeMap::new();
    by_path.insert("/", ROOT);
    let mut nodes: BTreeMap<u32, Node> = BTreeMap::new();
    nodes.insert(ROOT, Node { cnid: ROOT, parent: ROOT_PARENT, name: name.clone(), entry: with_type(root)?, extent: (0, 0), valence: 0, folders: 0 });
    let mut cnid = FIRST_USER;
    for entry in sorted {
        let (parent_path, leaf) = entry.path.rsplit_once('/').ok_or(Error("HFS paths start with a slash"))?;
        let parent_path = if parent_path.is_empty() { "/" } else { parent_path };
        if leaf.is_empty() || leaf == "." || leaf == ".." || leaf.contains('\0') {
            return Err(Error("bad HFS entry name"));
        }
        let parent = *by_path.get(parent_path).ok_or(Error("an HFS entry's folder is missing"))?;
        if nodes[&parent].entry.kind != Kind::Folder {
            return Err(Error("an HFS entry's parent is not a folder"));
        }
        if by_path.insert(&entry.path, cnid).is_some() {
            return Err(Error("duplicate HFS path"));
        }
        let units: Vec<u16> = leaf.encode_utf16().collect();
        if units.len() > 255 {
            return Err(Error("HFS name longer than 255 units"));
        }
        let folder = entry.kind == Kind::Folder;
        let p = nodes.get_mut(&parent).unwrap();
        p.valence += 1;
        p.folders += u32::from(folder);
        nodes.insert(cnid, Node { cnid, parent, name: units, entry: with_type(entry)?, extent: (0, 0), valence: 0, folders: 0 });
        cnid += 1;
    }
    let next_cnid = cnid;

    // Sizes first: the catalog's size doesn't depend on where data goes.
    let blocks_for = |bytes: u64| bytes.div_ceil(u64::from(bs));
    let catalog_len = btree(&catalog_records(&nodes, bs), &CATALOG)?.len() as u64;
    let extents_len = btree(&[], &EXTENTS)?.len() as u64;
    let attributes_len = btree(&[], &ATTRIBUTES)?.len() as u64;
    let data_blocks: u64 = nodes.values().map(|n| blocks_for(data_of(&n.entry).len() as u64)).sum();
    let meta_blocks = blocks_for(catalog_len) + blocks_for(extents_len) + blocks_for(attributes_len);
    let first_block = blocks_for(HEADER_OFFSET as u64 + 512);
    let mut total = (first_block + meta_blocks + data_blocks + blocks_for(options.slack) + 2)
        .max(blocks_for(options.min_size));
    let mut bitmap_blocks;
    loop {
        bitmap_blocks = blocks_for(total.div_ceil(8));
        let needed = first_block + bitmap_blocks + meta_blocks + data_blocks + blocks_for(options.slack) + 1;
        if needed <= total {
            break;
        }
        total = needed;
    }
    let total = u32::try_from(total).map_err(|_| Error("HFS volume too large"))?;

    // Layout: header, bitmap, extents, catalog, attributes, file data.
    let mut at = first_block as u32;
    let bitmap = (at, bitmap_blocks as u32);
    at += bitmap.1;
    let extents = (at, blocks_for(extents_len) as u32);
    at += extents.1;
    let catalog = (at, blocks_for(catalog_len) as u32);
    at += catalog.1;
    let attributes = (at, blocks_for(attributes_len) as u32);
    at += attributes.1;
    for node in nodes.values_mut() {
        let n = blocks_for(data_of(&node.entry).len() as u64) as u32;
        node.extent = if n == 0 { (0, 0) } else { (at, n) };
        at += n;
    }
    let used_end = at;

    let size = total as usize * bs as usize;
    let mut image = vec![0u8; size];
    let block = |b: u32| b as usize * bs as usize;
    let catalog_bytes = btree(&catalog_records(&nodes, bs), &CATALOG)?;
    image[block(catalog.0)..block(catalog.0) + catalog_bytes.len()].copy_from_slice(&catalog_bytes);
    let extents_bytes = btree(&[], &EXTENTS)?;
    image[block(extents.0)..block(extents.0) + extents_bytes.len()].copy_from_slice(&extents_bytes);
    let attributes_bytes = btree(&[], &ATTRIBUTES)?;
    image[block(attributes.0)..block(attributes.0) + attributes_bytes.len()].copy_from_slice(&attributes_bytes);
    let (mut files, mut folders) = (0u32, 0u32);
    for node in nodes.values() {
        let data = data_of(&node.entry);
        image[block(node.extent.0)..block(node.extent.0) + data.len()].copy_from_slice(data);
        match node.entry.kind {
            Kind::Folder if node.cnid != ROOT => folders += 1,
            Kind::Folder => {}
            _ => files += 1,
        }
    }
    let mut used = vec![false; total as usize];
    used[..used_end as usize].fill(true);
    // The block with the alternate volume header.
    let alternate_block = ((size - HEADER_OFFSET) / bs as usize) as u32;
    used[alternate_block as usize..].fill(true);
    let free = used.iter().filter(|u| !**u).count() as u32;
    let bitmap_at = block(bitmap.0);
    for (b, &u) in used.iter().enumerate() {
        if u {
            image[bitmap_at + b / 8] |= 0x80 >> (b % 8);
        }
    }

    let mut header = vec![0u8; 512];
    header[0..2].copy_from_slice(b"HX");
    be16(&mut header, 2, 5);
    be32(&mut header, 4, 0x0000_0100); // unmounted cleanly
    header[8..12].copy_from_slice(b"10.0");
    be32(&mut header, 16, options.created);
    be32(&mut header, 20, options.modified);
    be32(&mut header, 28, options.modified);
    be32(&mut header, 32, files);
    be32(&mut header, 36, folders);
    be32(&mut header, 40, bs);
    be32(&mut header, 44, total);
    be32(&mut header, 48, free);
    be32(&mut header, 52, used_end);
    be32(&mut header, 56, 65536);
    be32(&mut header, 60, 65536);
    be32(&mut header, 64, next_cnid);
    be64(&mut header, 72, 1); // MacRoman, the only encoding used
    header[80..112].copy_from_slice(&options.finder);
    fork(&mut header, 112, u64::from(bitmap.1) * u64::from(bs), bs, bitmap);
    fork(&mut header, 192, extents_len, NODE as u32, extents);
    fork(&mut header, 272, catalog_len, NODE as u32, catalog);
    fork(&mut header, 352, attributes_len, NODE as u32, attributes);
    image[HEADER_OFFSET..HEADER_OFFSET + 512].copy_from_slice(&header);
    image[size - HEADER_OFFSET..size - HEADER_OFFSET + 512].copy_from_slice(&header);
    Ok(image)
}

/// The entry with the file type bits its kind needs, and a symlink's Finder
/// type when it has none.
fn with_type(entry: &Entry) -> Result<Entry> {
    let mut entry = entry.clone();
    let want = match entry.kind {
        Kind::Folder => S_IFDIR,
        Kind::File(_) => S_IFREG,
        Kind::Symlink(_) => S_IFLNK,
    };
    match entry.meta.mode & S_IFMT {
        0 => entry.meta.mode |= want,
        t if t == want => {}
        _ => return Err(Error("an HFS entry's mode disagrees with its kind")),
    }
    if matches!(entry.kind, Kind::Symlink(_)) && entry.meta.finder[..8] == [0; 8] {
        entry.meta.finder[..8].copy_from_slice(b"slnkrhap");
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hfs::Volume;

    fn meta(mode: u16) -> Meta {
        Meta { mode, owner: 0, group: 0, created: 100, modified: 200, ..Meta::default() }
    }

    fn options() -> Options {
        Options { name: "Test".into(), block_size: 4096, created: 1, modified: 2, finder: [0; 32], min_size: 0, slack: 0 }
    }

    fn sample() -> Vec<Entry> {
        let mut entries = vec![
            Entry { path: "/".into(), meta: meta(0o755), kind: Kind::Folder },
            Entry { path: "/bin".into(), meta: meta(0o755), kind: Kind::Folder },
            Entry { path: "/bin/tool".into(), meta: meta(0o755), kind: Kind::File(b"#!/bin/sh\n".to_vec()) },
            Entry { path: "/empty".into(), meta: meta(0o644), kind: Kind::File(Vec::new()) },
            Entry { path: "/link".into(), meta: meta(0o755), kind: Kind::Symlink(b"bin/tool".to_vec()) },
        ];
        // Enough files for several catalog leaves and an index node.
        for i in 0..300 {
            entries.push(Entry {
                path: format!("/many/file-{i:03}"),
                meta: meta(0o600),
                kind: Kind::File(vec![i as u8; 5000 + i]),
            });
        }
        entries.push(Entry { path: "/many".into(), meta: meta(0o700), kind: Kind::Folder });
        entries
    }

    #[test]
    fn written_volumes_read_back_entry_for_entry() {
        let entries = sample();
        let image = write(&entries, &options()).unwrap();
        let mut volume = Volume::open(image.as_slice(), 0).unwrap();
        assert_eq!(volume.name().unwrap(), "Test");
        let back = volume.entries().unwrap();
        let mut want: Vec<Entry> = entries.iter().map(|e| with_type(e).unwrap()).collect();
        want.sort_by(|a, b| (a.path != "/", &a.path).cmp(&(b.path != "/", &b.path)));
        assert_eq!(back.len(), want.len());
        for (b, w) in back.iter().zip(&want) {
            assert_eq!(b, w, "{}", w.path);
        }
        assert_eq!(volume.read("many/file-299").unwrap(), vec![299u16 as u8; 5299]);
    }

    #[test]
    fn the_same_entries_give_the_same_bytes() {
        let mut shuffled = sample();
        shuffled.reverse();
        assert_eq!(write(&sample(), &options()).unwrap(), write(&shuffled, &options()).unwrap());
    }

    #[test]
    fn broken_trees_are_refused() {
        let mut orphan = sample();
        orphan.push(Entry { path: "/nowhere/x".into(), meta: meta(0o644), kind: Kind::File(vec![]) });
        assert!(write(&orphan, &options()).is_err());
        let mut twice = sample();
        twice.push(twice[2].clone());
        assert!(write(&twice, &options()).is_err());
        let wrong = vec![Entry { path: "/".into(), meta: meta(0o100644), kind: Kind::Folder }];
        assert!(write(&wrong, &options()).is_err());
    }
}

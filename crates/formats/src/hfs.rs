//! Read-only HFS+: finding a file by path and reading it, including files
//! the filesystem compressed (decmpfs). Ported from orchard's `hfs.py`
//! (adapted from rzxvx / Project Cascadia).
//!
//! Catalog, attributes and overflow extents are sought by numeric key
//! prefix, then scanned only within that prefix. That avoids both a full
//! catalog scan and approximating Apple's Unicode collation.
//!
//! The volume reads through [`Disk`], so this module does no I/O itself.
use crate::decmpfs::{self, Header};
use crate::{Error, Result};
use std::collections::HashSet;

/// Random access to the bytes the volume is on.
pub trait Disk {
    fn size(&self) -> u64;
    fn read(&mut self, offset: u64, len: usize) -> Result<Vec<u8>>;
}

impl Disk for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }
    fn read(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let start = usize::try_from(offset).map_err(|_| Error("read outside the disk"))?;
        self.get(start..start.checked_add(len).ok_or(Error("read outside the disk"))?)
            .map(<[u8]>::to_vec)
            .ok_or(Error("read outside the disk"))
    }
}

fn u16_at(b: &[u8], at: usize) -> Result<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]])).ok_or(Error("truncated HFS structure"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap())).ok_or(Error("truncated HFS structure"))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64> {
    b.get(at..at + 8).map(|s| u64::from_be_bytes(s.try_into().unwrap())).ok_or(Error("truncated HFS structure"))
}

fn utf16be(b: &[u8]) -> Option<String> {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
    String::from_utf16(&units).ok()
}

/// What a catalog entry says about itself besides its contents, as HFS+
/// stores it (dates in seconds since 1904).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Meta {
    pub created: u32,
    pub modified: u32,
    pub attributes_modified: u32,
    pub accessed: u32,
    pub backed_up: u32,
    pub owner: u32,
    pub group: u32,
    pub admin_flags: u8,
    pub owner_flags: u8,
    /// The BSD mode, file type bits included.
    pub mode: u16,
    /// Finder info and extended Finder info (32 bytes).
    pub finder: [u8; 32],
}

/// A folder, a file with its contents, or a symbolic link with its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Folder,
    File(Vec<u8>),
    Symlink(Vec<u8>),
}

/// One entry of a volume: `/` for the root, `/a/b` below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub meta: Meta,
    pub kind: Kind,
}

/// The owner flag that marks a file the filesystem compressed.
pub const UF_COMPRESSED: u8 = 0x20;

impl Meta {
    /// From a catalog folder or file record (the first 80 bytes are shared).
    fn parse(record: &[u8]) -> Result<Meta> {
        let mut finder = [0u8; 32];
        finder.copy_from_slice(record.get(48..80).ok_or(Error("truncated HFS catalog record"))?);
        Ok(Meta {
            created: u32_at(record, 12)?,
            modified: u32_at(record, 16)?,
            attributes_modified: u32_at(record, 20)?,
            accessed: u32_at(record, 24)?,
            backed_up: u32_at(record, 28)?,
            owner: u32_at(record, 32)?,
            group: u32_at(record, 36)?,
            admin_flags: record[40],
            owner_flags: record[41],
            mode: u16_at(record, 42)?,
            finder,
        })
    }
}

/// A fork: its logical size and the extents that hold it.
#[derive(Debug, Clone)]
struct Fork {
    size: u64,
    extents: Vec<(u32, u32)>,
}

/// Which B-tree a record search walks.
#[derive(Clone, Copy)]
enum Tree {
    Extents,
    Catalog,
    Attributes,
}

/// An open HFS+ volume.
pub struct Volume<D> {
    disk: D,
    base: u64,
    block_size: u64,
    blocks: u64,
    extents: Fork,
    catalog: Fork,
    attributes: Fork,
}

impl<D: Disk> Volume<D> {
    /// Opens the volume that starts `base` bytes into the disk.
    pub fn open(mut disk: D, base: u64) -> Result<Volume<D>> {
        let header = disk.read(base + 1024, 512)?;
        if &header[..2] != b"H+" && &header[..2] != b"HX" {
            return Err(Error("not an HFS+ volume"));
        }
        let block_size = u64::from(u32_at(&header, 40)?);
        let blocks = u64::from(u32_at(&header, 44)?);
        if block_size < 512
            || !block_size.is_power_of_two()
            || block_size.checked_mul(blocks).and_then(|n| n.checked_add(base)).is_none_or(|end| end > disk.size())
        {
            return Err(Error("invalid HFS volume geometry"));
        }
        let mut volume = Volume {
            disk,
            base,
            block_size,
            blocks,
            extents: Fork { size: 0, extents: Vec::new() },
            catalog: Fork { size: 0, extents: Vec::new() },
            attributes: Fork { size: 0, extents: Vec::new() },
        };
        volume.extents = volume.fork(&header[192..272], None)?;
        volume.catalog = volume.fork(&header[272..352], Some((0, 4)))?;
        volume.attributes = volume.fork(&header[352..432], Some((0, 8)))?;
        Ok(volume)
    }

    /// A fork from its 80-byte record, with overflow extents looked up for
    /// `owner` (fork kind, CNID) when the eight inline ones don't cover it.
    fn fork(&mut self, record: &[u8], owner: Option<(u8, u32)>) -> Result<Fork> {
        if record.len() != 80 {
            return Err(Error("truncated HFS fork"));
        }
        let size = u64_at(record, 0)?;
        let mut extents = Vec::with_capacity(8);
        for i in 0..8 {
            extents.push((u32_at(record, 16 + 8 * i)?, u32_at(record, 20 + 8 * i)?));
        }
        let mut covered: u64 = extents.iter().map(|&(_, n)| u64::from(n)).sum();
        if covered * self.block_size < size {
            let (kind, cnid) = owner.ok_or(Error("fragmented HFS metadata bootstrap is unsupported"))?;
            let target = (u64::from(kind) << 32) | u64::from(cnid);
            let prefix = |k: &[u8]| -> Result<u64> { Ok((u64::from(k[0]) << 32) | u64::from(u32_at(k, 2)?)) };
            for (key, value) in self.records(Tree::Extents, target, &prefix)? {
                if u64::from(u32_at(&key, 6)?) != covered || value.len() < 64 {
                    return Err(Error("missing HFS overflow extents"));
                }
                for i in 0..8 {
                    let extent = (u32_at(&value, 8 * i)?, u32_at(&value, 8 * i + 4)?);
                    covered += u64::from(extent.1);
                    extents.push(extent);
                }
            }
        }
        if covered * self.block_size < size {
            return Err(Error("HFS extents do not cover the fork"));
        }
        if extents.iter().any(|&(start, count)| u64::from(start) + u64::from(count) > self.blocks) {
            return Err(Error("HFS extent outside the volume"));
        }
        Ok(Fork { size, extents })
    }

    fn read_fork(&mut self, fork: &Fork, mut offset: u64, mut len: u64) -> Result<Vec<u8>> {
        if offset.checked_add(len).is_none_or(|end| end > fork.size) {
            return Err(Error("read outside HFS fork"));
        }
        let mut out = Vec::with_capacity(len as usize);
        for &(start, count) in &fork.extents {
            let span = u64::from(count) * self.block_size;
            if offset >= span {
                offset -= span;
                continue;
            }
            let take = len.min(span - offset);
            let at = self.base + u64::from(start) * self.block_size + offset;
            out.extend(self.disk.read(at, take as usize)?);
            len -= take;
            offset = 0;
            if len == 0 {
                break;
            }
        }
        if len != 0 {
            return Err(Error("short HFS fork"));
        }
        Ok(out)
    }

    fn tree(&self, tree: Tree) -> Fork {
        match tree {
            Tree::Extents => self.extents.clone(),
            Tree::Catalog => self.catalog.clone(),
            Tree::Attributes => self.attributes.clone(),
        }
    }

    /// The leaf records of a B-tree whose key prefix equals `target`.
    fn records(
        &mut self,
        tree: Tree,
        target: u64,
        prefix: &dyn Fn(&[u8]) -> Result<u64>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let fork = self.tree(tree);
        let mut found = Vec::new();
        if fork.size == 0 {
            return Ok(found);
        }
        let header = self.read_fork(&fork, 0, 64)?;
        let root = u32_at(&header, 16)?;
        let node_size = u64::from(u16_at(&header, 32)?);
        if node_size < 512 || !node_size.is_power_of_two() {
            return Err(Error("invalid HFS B-tree node size"));
        }
        let mut seen = HashSet::new();
        let mut number = root;
        while number != 0 {
            if !seen.insert(number) {
                return Err(Error("HFS B-tree cycle"));
            }
            let raw = self.read_fork(&fork, u64::from(number) * node_size, node_size)?;
            let entries = node_records(&raw, node_size as usize)?;
            match raw[8] {
                // Index: keep the child before the first key at or past the target.
                0 => {
                    let mut chosen = entries.first().ok_or(Error("empty HFS index"))?.1.clone();
                    for (key, value) in &entries {
                        if prefix(key)? >= target {
                            break;
                        }
                        chosen = value.clone();
                    }
                    number = u32_at(&chosen, 0)?;
                }
                0xff => {
                    for (key, value) in entries {
                        let p = prefix(&key)?;
                        if p > target {
                            return Ok(found);
                        }
                        if p == target {
                            found.push((key, value));
                        }
                    }
                    number = u32_at(&raw, 0)?;
                }
                _ => return Err(Error("unexpected HFS node kind")),
            }
        }
        Ok(found)
    }

    /// The catalog file record at `path` (`a/b/c`, relative to the root).
    fn lookup(&mut self, path: &str) -> Result<Vec<u8>> {
        let names: Vec<&str> = path.trim_matches('/').split('/').collect();
        if names.iter().any(|n| n.is_empty() || *n == "." || *n == "..") {
            return Err(Error("invalid HFS file path"));
        }
        let mut parent = 2u32;
        let mut record = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let parent_id = u64::from(parent);
            let prefix = |k: &[u8]| -> Result<u64> { Ok(u64::from(u32_at(k, 0)?)) };
            let mut found = Vec::new();
            for (key, value) in self.records(Tree::Catalog, parent_id, &prefix)? {
                let length = usize::from(u16_at(&key, 4)?);
                if key.len() != 6 + 2 * length {
                    return Err(Error("invalid HFS catalog name"));
                }
                if utf16be(&key[6..]).as_deref() == Some(*name) {
                    found.push(value);
                }
            }
            if found.len() != 1 {
                return Err(Error("HFS file not found or ambiguous"));
            }
            record = found.remove(0);
            let last = i == names.len() - 1;
            let (min, kind) = if last { (248, 2) } else { (88, 1) };
            if record.len() < min || u16_at(&record, 0)? != kind {
                return Err(Error("unexpected HFS catalog record"));
            }
            parent = u32_at(&record, 8)?;
        }
        Ok(record)
    }

    fn xattr(&mut self, cnid: u32, name: &str) -> Result<Vec<u8>> {
        let prefix = |k: &[u8]| -> Result<u64> { Ok(u64::from(u32_at(k, 2)?)) };
        for (key, record) in self.records(Tree::Attributes, u64::from(cnid), &prefix)? {
            if key.len() < 12 || key.len() != 12 + usize::from(u16_at(&key, 10)?) * 2 {
                return Err(Error("invalid HFS attribute key"));
            }
            if utf16be(&key[12..]).as_deref() == Some(name) {
                let len = u32_at(&record, 12)? as usize;
                if record.len() < 16 || u32_at(&record, 0)? != 0x10 || len > record.len() - 16 {
                    return Err(Error("unsupported or truncated HFS attribute"));
                }
                return Ok(record[16..16 + len].to_vec());
            }
        }
        Err(Error("HFS compression attribute missing"))
    }

    /// The contents of the file at `path`, decompressed if the filesystem
    /// compressed it.
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let record = self.lookup(path)?;
        self.contents(&record)
    }

    /// Every leaf record of a B-tree, in key order.
    fn leaves(&mut self, tree: Tree) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let fork = self.tree(tree);
        let mut out = Vec::new();
        if fork.size == 0 {
            return Ok(out);
        }
        let header = self.read_fork(&fork, 0, 64)?;
        let node_size = u64::from(u16_at(&header, 32)?);
        if node_size < 512 || !node_size.is_power_of_two() {
            return Err(Error("invalid HFS B-tree node size"));
        }
        let mut seen = HashSet::new();
        let mut number = u32_at(&header, 24)?;
        while number != 0 {
            if !seen.insert(number) {
                return Err(Error("HFS B-tree cycle"));
            }
            let raw = self.read_fork(&fork, u64::from(number) * node_size, node_size)?;
            if raw[8] != 0xff {
                return Err(Error("HFS leaf chain reaches a non-leaf node"));
            }
            out.extend(node_records(&raw, node_size as usize)?);
            number = u32_at(&raw, 0)?;
        }
        Ok(out)
    }

    /// The volume's name: the root folder's name.
    pub fn name(&mut self) -> Result<String> {
        for (key, value) in self.leaves(Tree::Catalog)? {
            if u32_at(&key, 0)? == 1 && u16_at(&value, 0)? == 1 && u32_at(&value, 8)? == 2 {
                return utf16be(&key[6..]).ok_or(Error("invalid HFS volume name"));
            }
        }
        Err(Error("HFS volume has no root folder"))
    }

    /// Every folder, file and symbolic link, the root first, then by path.
    /// Compressed files come back decompressed (and without the compressed
    /// flag); hard links and resource forks are refused.
    pub fn entries(&mut self) -> Result<Vec<Entry>> {
        use std::collections::HashMap;
        // CNID → (parent, name, record)
        let mut nodes: HashMap<u32, (u32, String, Vec<u8>)> = HashMap::new();
        for (key, value) in self.leaves(Tree::Catalog)? {
            let kind = u16_at(&value, 0)?;
            if kind != 1 && kind != 2 {
                continue;
            }
            let length = usize::from(u16_at(&key, 4)?);
            if key.len() != 6 + 2 * length {
                return Err(Error("invalid HFS catalog name"));
            }
            let name = utf16be(&key[6..]).ok_or(Error("invalid HFS catalog name"))?;
            if name.contains(['/', '\0']) {
                return Err(Error("HFS name with a slash or NUL"));
            }
            nodes.insert(u32_at(&value, 8)?, (u32_at(&key, 0)?, name, value));
        }
        let path_of = |mut cnid: u32| -> Result<String> {
            let mut parts = Vec::new();
            for _ in 0..256 {
                if cnid == 2 {
                    parts.reverse();
                    return Ok(format!("/{}", parts.join("/")));
                }
                let (parent, name, _) = nodes.get(&cnid).ok_or(Error("HFS entry without a parent"))?;
                parts.push(name.clone());
                cnid = *parent;
            }
            Err(Error("HFS folders nest too deep"))
        };
        let mut out = Vec::new();
        let records: Vec<(u32, Vec<u8>)> = nodes.iter().map(|(c, (_, _, r))| (*c, r.clone())).collect();
        for (cnid, record) in records {
            let meta = Meta::parse(&record)?;
            let path = path_of(cnid)?;
            let kind = if u16_at(&record, 0)? == 1 {
                Kind::Folder
            } else {
                if record.len() < 248 {
                    return Err(Error("unexpected HFS catalog record"));
                }
                let (file_type, creator) = (&record[48..52], &record[52..56]);
                if file_type == b"hlnk" && creator == b"hfs+" {
                    return Err(Error("HFS hard links are not supported"));
                }
                if u64_at(&record, 168)? != 0 && meta.owner_flags & UF_COMPRESSED == 0 {
                    return Err(Error("HFS resource forks are not supported"));
                }
                let data = self.contents(&record)?;
                if file_type == b"slnk" && creator == b"rhap" {
                    Kind::Symlink(data)
                } else {
                    Kind::File(data)
                }
            };
            let meta = Meta { owner_flags: meta.owner_flags & !UF_COMPRESSED, ..meta };
            out.push(Entry { path, meta, kind });
        }
        out.sort_by(|a, b| (a.path != "/", &a.path).cmp(&(b.path != "/", &b.path)));
        Ok(out)
    }

    /// A file record's contents, decompressed if needed.
    fn contents(&mut self, record: &[u8]) -> Result<Vec<u8>> {
        let cnid = u32_at(record, 8)?;
        if record[41] & 0x20 == 0 {
            let fork = self.fork(&record[88..168], Some((0, cnid)))?;
            return self.read_fork(&fork, 0, fork.size);
        }
        let attribute = self.xattr(cnid, decmpfs::ATTRIBUTE)?;
        let header = Header::parse(&attribute)?;
        if header.in_fork {
            let fork = self.fork(&record[168..248], Some((0xff, cnid)))?;
            let data = self.read_fork(&fork, 0, fork.size)?;
            decmpfs::resource(&header, &data)
        } else {
            decmpfs::inline(&header, &attribute[16..])
        }
    }
}

/// A node's records: (key, value) for each, as the offset table says.
fn node_records(raw: &[u8], node_size: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let count = usize::from(u16_at(raw, 10)?);
    let table = node_size.checked_sub(2 * (count + 1)).ok_or(Error("invalid HFS record offsets"))?;
    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..=count {
        offsets.push(usize::from(u16_at(raw, node_size - 2 * (i + 1))?));
    }
    if table < 14 || offsets.windows(2).any(|w| !(14 <= w[0] && w[0] < w[1] && w[1] <= table)) {
        return Err(Error("invalid HFS record offsets"));
    }
    let mut records = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let (a, b) = (w[0], w[1]);
        let key_size = usize::from(u16_at(raw, a)?);
        let end = a + 2 + key_size;
        if key_size < 6 || end > b {
            return Err(Error("invalid HFS key"));
        }
        records.push((raw[a + 2..end].to_vec(), raw[(end + 1) & !1..b].to_vec()));
    }
    Ok(records)
}

//! Reading files from an APFS container, such as the one inside a firmware
//! disk image. The source covers exactly the container's partition.
use orchard_cache::source::Source;
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::apfs::{self, Inode, Node, Tree, Xattr};
pub use orchard_formats::apfs::{DirEntry, EntryKind};
use orchard_formats::decmpfs;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// APFS trees in firmware images are two or three levels deep.
const MAX_DEPTH: usize = 16;
/// Checkpoint areas this large don't occur in practice.
const MAX_CHECKPOINTS: u64 = 4096;
/// Metadata blocks kept in memory.
const CACHE_BYTES: usize = 16 << 20;
/// Resource forks and other attribute streams larger than this are refused.
const MAX_ATTRIBUTE: u64 = 64 << 20;

pub struct Container<S> {
    source: S,
    block_size: usize,
    blocks: u64,
    xid: u64,
    /// Root of the container object map tree.
    omap: u64,
    volumes: Vec<u64>,
    cache: HashMap<u64, Rc<[u8]>>,
    cached: usize,
}

/// One B-tree and how to reach its nodes.
#[derive(Clone, Copy)]
struct View {
    root: u64,
    root_oid: u64,
    tree: Tree,
    /// The object map that turns child ids into blocks, for virtual trees.
    omap: Option<u64>,
    xid: u64,
}

impl<S: Source> Container<S> {
    pub fn open(mut source: S) -> Result<Container<S>> {
        let block_size = apfs::block_size(&source.read_at(0, 4096)?)?;
        let mut best = apfs::Container::parse(&source.read_at(0, block_size)?)?;
        let bytes = best.blocks.checked_mul(block_size as u64);
        if best.blocks == 0 || bytes.is_none_or(|b| b > source.size()) {
            return Err(Error::new(
                Code::Format,
                "APFS container is larger than its partition",
            ));
        }
        // The newest valid superblock in the checkpoint area wins.
        let (base, count) = best.checkpoints;
        if count > MAX_CHECKPOINTS || base.checked_add(count).is_none_or(|end| end > best.blocks) {
            return Err(Error::new(
                Code::Format,
                "APFS checkpoint area is out of range",
            ));
        }
        for i in 0..count {
            let block = source.read_at((base + i) * block_size as u64, block_size)?;
            if block.get(32..36) != Some(b"NXSB") {
                continue;
            }
            let candidate = apfs::Container::parse(&block)?;
            if candidate.uuid != best.uuid || candidate.blocks != best.blocks {
                return Err(Error::new(
                    Code::Format,
                    "APFS checkpoint belongs to another container",
                ));
            }
            if candidate.xid > best.xid {
                best = candidate;
            }
        }
        let mut container = Container {
            source,
            block_size,
            blocks: best.blocks,
            xid: best.xid,
            omap: 0,
            volumes: best.volumes,
            cache: HashMap::new(),
            cached: 0,
        };
        container.omap = apfs::omap_tree(&container.block(best.omap)?)?;
        Ok(container)
    }

    pub fn volume_count(&self) -> usize {
        self.volumes.len()
    }

    pub fn volume(&mut self, index: usize) -> Result<Volume<'_, S>> {
        let superblock = self.superblock(index)?;
        Volume::open(self, superblock)
    }

    /// The first volume with this name, such as `System` or `Preboot`.
    pub fn volume_named(&mut self, name: &str) -> Result<Volume<'_, S>> {
        for i in 0..self.volumes.len() {
            let superblock = self.superblock(i)?;
            if superblock.name == name {
                return Volume::open(self, superblock);
            }
        }
        Err(Error::new(
            Code::NotFound,
            format!("APFS container has no volume named {name}"),
        ))
    }

    fn superblock(&mut self, index: usize) -> Result<apfs::Volume> {
        let oid = *self.volumes.get(index).ok_or_else(|| {
            Error::new(
                Code::NotFound,
                format!("APFS container has no volume {index}"),
            )
        })?;
        let block = self.lookup(self.omap, oid, self.xid)?;
        let superblock = apfs::Volume::parse(&self.block(block)?)?;
        if superblock.oid != oid {
            return Err(Error::new(
                Code::Format,
                "APFS volume superblock has the wrong id",
            ));
        }
        Ok(superblock)
    }

    /// A metadata block, checksum-checked.
    fn block(&mut self, n: u64) -> Result<Rc<[u8]>> {
        if let Some(block) = self.cache.get(&n) {
            return Ok(block.clone());
        }
        if n >= self.blocks {
            return Err(Error::new(
                Code::Format,
                "APFS object lies outside the container",
            ));
        }
        let block: Rc<[u8]> = self
            .source
            .read_at(n * self.block_size as u64, self.block_size)?
            .into();
        if apfs::object(&block)?.xid > self.xid {
            return Err(Error::new(
                Code::Format,
                "APFS object is newer than the container",
            ));
        }
        if self.cached + block.len() > CACHE_BYTES {
            self.cache.clear();
            self.cached = 0;
        }
        self.cached += block.len();
        self.cache.insert(n, block.clone());
        Ok(block)
    }

    /// The block where virtual object `oid` lives at transaction `xid`.
    fn lookup(&mut self, omap: u64, oid: u64, xid: u64) -> Result<u64> {
        let view = View {
            root: omap,
            root_oid: omap,
            tree: Tree::Plain,
            omap: None,
            xid: self.xid,
        };
        let mut best: Option<apfs::Mapping> = None;
        for (key, value) in self.records(view, (oid, 0))? {
            let m = apfs::mapping(&key, &value, self.block_size)?;
            if m.xid <= xid && best.is_none_or(|b| m.xid > b.xid) {
                best = Some(m);
            }
        }
        match best {
            Some(m) if !m.deleted => Ok(m.block),
            _ => Err(Error::new(
                Code::Format,
                format!("APFS object {oid} is missing"),
            )),
        }
    }

    /// All leaf records whose key prefix equals `target`.
    fn records(&mut self, view: View, target: (u64, u8)) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(view.root, view.root_oid, None::<u16>)];
        while let Some((block, oid, parent)) = stack.pop() {
            if seen.len() > MAX_DEPTH * 1024 || !seen.insert(block) {
                return Err(Error::new(Code::Format, "APFS tree loops"));
            }
            let data = self.block(block)?;
            let node = Node::parse(&data)?;
            let fits = node.oid == oid
                && node.xid <= view.xid
                && node.root == parent.is_none()
                && parent.is_none_or(|p| node.level + 1 == p)
                && usize::from(node.level) < MAX_DEPTH;
            if !fits {
                return Err(Error::new(
                    Code::Format,
                    "APFS tree node does not fit its parent",
                ));
            }
            for i in 0..node.len() {
                let prefix = view.tree.prefix(node.key(i))?;
                if node.leaf() {
                    if prefix == target {
                        let value = node
                            .value(i)
                            .ok_or_else(|| Error::new(Code::Format, "APFS record has no value"))?;
                        out.push((node.key(i).to_vec(), value.to_vec()));
                    }
                    continue;
                }
                // Descend into every child whose key range can contain `target`.
                if prefix > target {
                    break;
                }
                if i + 1 < node.len() && view.tree.prefix(node.key(i + 1))? < target {
                    continue;
                }
                let child = node.child(i)?;
                let physical = match view.omap {
                    Some(omap) => self.lookup(omap, child, view.xid)?,
                    None => child,
                };
                stack.push((physical, child, Some(node.level)));
            }
        }
        Ok(out)
    }
}

pub struct Volume<'a, S> {
    container: &'a mut Container<S>,
    view: View,
    extents: u64,
    hashed_names: bool,
    name: String,
}

impl<'a, S: Source> Volume<'a, S> {
    fn open(container: &'a mut Container<S>, superblock: apfs::Volume) -> Result<Volume<'a, S>> {
        let omap = apfs::omap_tree(&container.block(superblock.omap)?)?;
        let root = match superblock.root_virtual {
            true => container.lookup(omap, superblock.root, superblock.xid)?,
            false => superblock.root,
        };
        Ok(Volume {
            container,
            view: View {
                root,
                root_oid: superblock.root,
                tree: Tree::Files,
                omap: superblock.root_virtual.then_some(omap),
                xid: superblock.xid,
            },
            extents: superblock.extents,
            hashed_names: superblock.hashed_names,
            name: superblock.name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The entries of a directory, sorted by name.
    pub fn list(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        self.list_at(path).context(path)
    }

    /// A file's contents, refusing files larger than `limit`.
    pub fn read(&mut self, path: &str, limit: u64) -> Result<Vec<u8>> {
        self.read_at(path, limit).context(path)
    }

    fn list_at(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let id = self.find(path)?;
        if !self.inode(id)?.is_dir() {
            return Err(Error::new(Code::InvalidInput, "not a directory"));
        }
        self.children(id)
    }

    fn read_at(&mut self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let id = self.find(path)?;
        let inode = self.inode(id)?;
        if !inode.is_file() {
            return Err(Error::new(Code::InvalidInput, "not a regular file"));
        }
        let too_large = || Error::new(Code::TooLarge, format!("larger than {limit} bytes"));
        if !inode.compressed {
            if inode.size > limit {
                return Err(too_large());
            }
            return self.data(inode.data_id, inode.size);
        }
        let attribute = self
            .attribute(id, decmpfs::ATTRIBUTE)?
            .ok_or_else(|| Error::new(Code::Format, "compressed, but has no compression header"))?;
        let header = decmpfs::Header::parse(&attribute)?;
        if header.size > limit {
            return Err(too_large());
        }
        if !header.in_fork {
            return Ok(decmpfs::inline(&header, &attribute[16..])?);
        }
        let fork = self
            .attribute(id, decmpfs::RESOURCE_FORK)?
            .ok_or_else(|| Error::new(Code::Format, "compressed, but has no resource fork"))?;
        Ok(decmpfs::resource(&header, &fork)?)
    }

    /// The inode number at `path`. Paths are exact and case-sensitive; symlinks
    /// are not followed.
    fn find(&mut self, path: &str) -> Result<u64> {
        let mut id = apfs::ROOT_DIR;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if part == "." || part == ".." {
                return Err(Error::new(
                    Code::InvalidInput,
                    "use a plain path without . or ..",
                ));
            }
            let entry = self
                .children(id)?
                .into_iter()
                .find(|e| e.name == part)
                .ok_or_else(|| {
                    Error::new(Code::NotFound, format!("not in volume {}", self.name))
                })?;
            if entry.kind == EntryKind::Symlink {
                return Err(Error::new(
                    Code::InvalidInput,
                    "goes through a symlink, which is not followed",
                ));
            }
            id = entry.inode;
        }
        Ok(id)
    }

    fn inode(&mut self, id: u64) -> Result<Inode> {
        match self
            .container
            .records(self.view, (id, apfs::INODE))?
            .as_slice()
        {
            [(_, value)] => Ok(apfs::inode(value)?),
            _ => Err(Error::new(
                Code::Format,
                format!("APFS inode {id} is missing or duplicated"),
            )),
        }
    }

    fn children(&mut self, id: u64) -> Result<Vec<DirEntry>> {
        let mut entries = self
            .container
            .records(self.view, (id, apfs::DIR_REC))?
            .iter()
            .map(|(key, value)| apfs::dir_entry(key, value, self.hashed_names))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn attribute(&mut self, id: u64, name: &str) -> Result<Option<Vec<u8>>> {
        for (key, value) in self.container.records(self.view, (id, apfs::XATTR))? {
            let (found, attr) = apfs::xattr(&key, &value)?;
            if found != name {
                continue;
            }
            return Ok(Some(match attr {
                Xattr::Embedded(data) => data.to_vec(),
                Xattr::Stream { size, .. } if size > MAX_ATTRIBUTE => {
                    return Err(Error::new(Code::TooLarge, "too large")
                        .context(format!("attribute {name}")))
                }
                Xattr::Stream { id, size } => self.data(id, size)?,
            }));
        }
        Ok(None)
    }

    /// The bytes filed under data id `id`, which must cover `size` bytes.
    fn data(&mut self, id: u64, size: u64) -> Result<Vec<u8>> {
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut records = self.container.records(self.view, (id, apfs::FILE_EXTENT))?;
        // Sealed volumes keep extents in a tree of their own.
        if records.is_empty() && self.extents != 0 {
            let view = View {
                root: self.extents,
                root_oid: self.extents,
                tree: Tree::Plain,
                omap: None,
                xid: self.view.xid,
            };
            records = self.container.records(view, (id, 0))?;
        }
        let mut extents = records
            .iter()
            .map(|(k, v)| apfs::extent(k, v))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        extents.sort_by_key(|e| e.logical);
        let block_size = self.container.block_size as u64;
        let mut out = Vec::with_capacity(size as usize);
        for e in extents {
            if out.len() as u64 >= size {
                break;
            }
            if e.logical != out.len() as u64 || e.len == 0 {
                return Err(Error::new(
                    Code::Format,
                    "APFS file extents leave a gap or overlap",
                ));
            }
            let len = e.len.min(size - out.len() as u64);
            if e.block == 0 {
                out.resize(out.len() + len as usize, 0);
                continue;
            }
            let end = e
                .block
                .checked_mul(block_size)
                .and_then(|start| start.checked_add(len));
            if end.is_none_or(|end| end > self.container.blocks * block_size) {
                return Err(Error::new(
                    Code::Format,
                    "APFS file extent lies outside the container",
                ));
            }
            out.extend(
                self.container
                    .source
                    .read_at(e.block * block_size, len as usize)?,
            );
        }
        if out.len() as u64 != size {
            return Err(Error::new(Code::Format, "APFS file is missing data"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;

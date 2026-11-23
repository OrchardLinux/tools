//! Small APFS containers built in memory. Block layout:
//! 0 container superblock, 2/3 container object map and tree, 4 volume
//! superblock, 5/6 volume object map and tree, 7 filesystem root node,
//! 8.. leaf nodes, 60 file extent tree, 64.. file data.
use super::*;
use std::io::Write;

const BS: usize = 4096;
const BLOCKS: usize = 256;
const DIR: u64 = 10;

fn put16(b: &mut [u8], at: usize, n: u16) {
    b[at..at + 2].copy_from_slice(&n.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, n: u32) {
    b[at..at + 4].copy_from_slice(&n.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, n: u64) {
    b[at..at + 8].copy_from_slice(&n.to_le_bytes());
}

/// Fletcher-64 written as weighted sums, independent of the reader's loop.
fn seal(b: &mut [u8]) {
    let words: Vec<u128> = b[8..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w).into())
        .collect();
    let m = u128::from(u32::MAX);
    let a = words.iter().sum::<u128>() % m;
    let c = words
        .iter()
        .enumerate()
        .map(|(i, w)| (words.len() - i) as u128 * w)
        .sum::<u128>()
        % m;
    let low = m - (a + c) % m;
    let high = m - (a + low) % m;
    put64(b, 0, (low | high << 32) as u64);
}

fn object(oid: u64, kind: u32, xid: u64) -> Vec<u8> {
    let mut b = vec![0; BS];
    put64(&mut b, 8, oid);
    put64(&mut b, 16, xid);
    put32(&mut b, 24, 0x4000_0000 | kind);
    b
}

type Row = (Vec<u8>, Vec<u8>);

fn key(id: u64, kind: u8) -> Vec<u8> {
    (id | u64::from(kind) << 60).to_le_bytes().to_vec()
}

fn pair(a: u64, b: u64) -> Vec<u8> {
    [a.to_le_bytes(), b.to_le_bytes()].concat()
}

fn mapping(oid: u64, block: u64) -> Row {
    let mut v = vec![0; 16];
    put32(&mut v, 4, BS as u32);
    put64(&mut v, 8, block);
    (pair(oid, 1), v)
}

fn inode(id: u64, data_id: u64, mode: u16, compressed: bool, size: Option<u64>) -> Row {
    let mut v = vec![0; if size.is_some() { 140 } else { 96 }];
    put64(&mut v, 8, data_id);
    put32(&mut v, 68, if compressed { 0x20 } else { 0 });
    put16(&mut v, 80, mode);
    if let Some(size) = size {
        put16(&mut v, 92, 1);
        put16(&mut v, 94, 40);
        v[96] = 8;
        put16(&mut v, 98, 40);
        put64(&mut v, 100, size);
    }
    (key(id, apfs::INODE), v)
}

fn entry(parent: u64, name: &str, id: u64, kind: u16, hashed: bool) -> Row {
    let mut k = key(parent, apfs::DIR_REC);
    let len = name.len() + 1;
    if hashed {
        k.extend((len as u32).to_le_bytes());
    } else {
        k.extend((len as u16).to_le_bytes());
    }
    k.extend(name.as_bytes());
    k.push(0);
    let mut v = vec![0; 18];
    put64(&mut v, 0, id);
    put16(&mut v, 16, kind);
    (k, v)
}

fn extent(id: u64, logical: u64, len: u64, block: u64) -> Row {
    let mut k = key(id, apfs::FILE_EXTENT);
    k.extend(logical.to_le_bytes());
    let mut v = vec![0; 24];
    put64(&mut v, 0, len);
    put64(&mut v, 8, block);
    (k, v)
}

fn xattr(id: u64, name: &str, flags: u16, data: &[u8]) -> Row {
    let mut k = key(id, apfs::XATTR);
    k.extend(((name.len() + 1) as u16).to_le_bytes());
    k.extend(name.as_bytes());
    k.push(0);
    let mut v = vec![0; 4];
    put16(&mut v, 0, flags);
    put16(&mut v, 2, data.len() as u16);
    v.extend(data);
    (k, v)
}

fn node(oid: u64, root: bool, level: u16, fixed: bool, rows: &[Row]) -> Vec<u8> {
    let leaf = level == 0;
    let mut b = object(oid, if root { apfs::BTREE } else { apfs::BTREE_NODE }, 1);
    put16(
        &mut b,
        32,
        u16::from(root) | u16::from(leaf) << 1 | u16::from(fixed) << 2,
    );
    put16(&mut b, 34, level);
    put32(&mut b, 36, rows.len() as u32);
    let width = if fixed { 4 } else { 8 };
    let toc_len = width * rows.len();
    put16(&mut b, 42, toc_len as u16);
    let values_end = BS - if root { 40 } else { 0 };
    let (mut k_at, mut v_at) = (56 + toc_len, values_end);
    for (i, (k, v)) in rows.iter().enumerate() {
        let at = 56 + i * width;
        put16(&mut b, at, (k_at - 56 - toc_len) as u16);
        b[k_at..k_at + k.len()].copy_from_slice(k);
        k_at += k.len();
        v_at -= v.len();
        assert!(k_at <= v_at, "node overflow");
        b[v_at..v_at + v.len()].copy_from_slice(v);
        if fixed {
            put16(&mut b, at + 2, (values_end - v_at) as u16);
        } else {
            put16(&mut b, at + 2, k.len() as u16);
            put16(&mut b, at + 4, (values_end - v_at) as u16);
            put16(&mut b, at + 6, v.len() as u16);
        }
    }
    if root {
        put32(&mut b, values_end + 4, BS as u32);
    }
    seal(&mut b);
    b
}

/// A container with one volume, `Test`, holding directory `/dir`.
struct Fixture {
    rows: Vec<Row>,
    blocks: HashMap<usize, Vec<u8>>,
    virtual_tree: bool,
    hashed: bool,
    sealed: bool,
    next_id: u64,
    next_block: usize,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            rows: vec![
                inode(apfs::ROOT_DIR, 2, 0o40755, false, None),
                inode(DIR, DIR, 0o40755, false, None),
            ],
            blocks: HashMap::new(),
            virtual_tree: true,
            hashed: false,
            sealed: false,
            next_id: 20,
            next_block: 64,
        }
    }

    fn data(&mut self, bytes: &[u8]) -> u64 {
        let first = self.next_block;
        for part in bytes.chunks(BS) {
            let mut block = vec![0; BS];
            block[..part.len()].copy_from_slice(part);
            self.blocks.insert(self.next_block, block);
            self.next_block += 1;
        }
        first as u64
    }

    fn extent_for(&mut self, id: u64, bytes: &[u8]) {
        let block = self.data(bytes);
        self.rows.push(extent(
            id,
            0,
            bytes.len().div_ceil(BS) as u64 * BS as u64,
            block,
        ));
    }

    fn file(&mut self, name: &str, bytes: &[u8]) {
        let id = self.next_id;
        self.next_id += 1;
        self.rows.push(inode(
            id,
            10_000 + id,
            0o100644,
            false,
            Some(bytes.len() as u64),
        ));
        self.rows.push(entry(DIR, name, id, 8, self.hashed));
        self.extent_for(10_000 + id, bytes);
    }

    fn compressed(
        &mut self,
        name: &str,
        kind: u32,
        size: usize,
        inline: &[u8],
        fork: Option<&[u8]>,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        self.rows.push(inode(id, id, 0o100644, true, None));
        self.rows.push(entry(DIR, name, id, 8, self.hashed));
        let mut header = b"fpmc".to_vec();
        header.extend(kind.to_le_bytes());
        header.extend((size as u64).to_le_bytes());
        header.extend(inline);
        self.rows.push(xattr(id, decmpfs::ATTRIBUTE, 2, &header));
        if let Some(fork) = fork {
            self.extent_for(20_000 + id, fork);
            let mut stream = vec![0; 48];
            put64(&mut stream, 0, 20_000 + id);
            put64(&mut stream, 8, fork.len() as u64);
            self.rows
                .push(xattr(id, decmpfs::RESOURCE_FORK, 1, &stream));
        }
    }

    fn build(mut self) -> Vec<u8> {
        self.rows
            .push(entry(apfs::ROOT_DIR, "dir", DIR, 4, self.hashed));
        self.rows.sort_by_key(|(k, _)| {
            let word = u64::from_le_bytes(k[..8].try_into().unwrap());
            (word & ((1 << 60) - 1), word >> 60, k[8..].to_vec())
        });
        let mut fext = Vec::new();
        if self.sealed {
            self.rows.retain(|(k, v)| {
                let word = u64::from_le_bytes(k[..8].try_into().unwrap());
                if word >> 60 != u64::from(apfs::FILE_EXTENT) {
                    return true;
                }
                fext.push((
                    pair(
                        word & ((1 << 60) - 1),
                        u64::from_le_bytes(k[8..16].try_into().unwrap()),
                    ),
                    v[..16].to_vec(),
                ));
                false
            });
        }
        // Leaves of up to 8 rows each, so the tree has more than one leaf.
        let mut root_rows = Vec::new();
        let mut maps = vec![mapping(1000, 7)];
        for (i, rows) in self.rows.chunks(8).enumerate() {
            let (block, oid) = (8 + i, 1001 + i as u64);
            let child = if self.virtual_tree { oid } else { block as u64 };
            root_rows.push((rows[0].0.clone(), child.to_le_bytes().to_vec()));
            self.blocks
                .insert(block, node(child, false, 0, false, rows));
            maps.push(mapping(oid, block as u64));
        }
        let root = if self.virtual_tree { 1000 } else { 7 };
        self.blocks
            .insert(7, node(root, true, 1, false, &root_rows));
        self.blocks.insert(6, node(6, true, 0, true, &maps));
        self.blocks
            .insert(3, node(3, true, 0, true, &[mapping(100, 4)]));
        for (at, tree) in [(2, 3), (5, 6)] {
            let mut omap = object(at as u64, apfs::OMAP, 1);
            put32(&mut omap, 40, 0x4000_0002);
            put64(&mut omap, 48, tree);
            seal(&mut omap);
            self.blocks.insert(at, omap);
        }
        let mut nx = object(1, apfs::NX_SUPERBLOCK, 1);
        nx[32..36].copy_from_slice(b"NXSB");
        put32(&mut nx, 36, BS as u32);
        put64(&mut nx, 40, BLOCKS as u64);
        put64(&mut nx, 160, 2);
        put32(&mut nx, 180, 100);
        put64(&mut nx, 184, 100);
        seal(&mut nx);
        self.blocks.insert(0, nx);
        let mut volume = object(100, apfs::FS, 1);
        volume[32..36].copy_from_slice(b"APSB");
        put64(
            &mut volume,
            56,
            u64::from(self.hashed) | if self.sealed { 0x20 } else { 0 },
        );
        put32(
            &mut volume,
            116,
            if self.virtual_tree { 2 } else { 0x4000_0002 },
        );
        put64(&mut volume, 128, 5);
        put64(&mut volume, 136, root);
        put64(&mut volume, 264, 1);
        volume[704..708].copy_from_slice(b"Test");
        if self.sealed {
            put64(&mut volume, 1032, 60);
            self.blocks.insert(60, node(60, true, 0, true, &fext));
        }
        seal(&mut volume);
        self.blocks.insert(4, volume);
        let mut image = vec![0; BLOCKS * BS];
        for (at, block) in self.blocks {
            image[at * BS..(at + 1) * BS].copy_from_slice(&block);
        }
        image
    }
}

fn read(image: Vec<u8>, path: &str) -> Result<Vec<u8>> {
    let mut container = Container::open(image)?;
    let mut volume = container.volume_named("Test")?;
    volume.read(path, 1 << 20)
}

fn sample(f: &mut Fixture) {
    f.file("plain", b"plain firmware\n");
    f.file("large", &vec![0xab; 3 * BS + 100]);
    for i in 0..12 {
        f.file(&format!("filler{i:02}"), b"x");
    }
}

#[test]
fn reads_files_and_lists_directories() {
    let mut f = Fixture::new();
    sample(&mut f);
    let image = f.build();
    assert_eq!(
        read(image.clone(), "/dir/plain").unwrap(),
        b"plain firmware\n"
    );
    assert_eq!(
        read(image.clone(), "dir/large").unwrap(),
        vec![0xab; 3 * BS + 100]
    );
    let mut container = Container::open(image).unwrap();
    assert_eq!(container.volume_count(), 1);
    let mut volume = container.volume(0).unwrap();
    assert_eq!(volume.name(), "Test");
    let names: Vec<_> = volume
        .list("/dir")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names.len(), 14);
    assert_eq!(names[0], "filler00");
    assert_eq!(volume.list("/").unwrap()[0].kind, EntryKind::Directory);
}

#[test]
fn physical_trees_hashed_names_and_sealed_volumes() {
    for (virtual_tree, hashed, sealed) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let mut f = Fixture::new();
        (f.virtual_tree, f.hashed, f.sealed) = (virtual_tree, hashed, sealed);
        sample(&mut f);
        assert_eq!(read(f.build(), "/dir/plain").unwrap(), b"plain firmware\n");
    }
}

#[test]
fn compressed_files() {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(b"zlib firmware\n").unwrap();
    let zlib = z.finish().unwrap();
    let lzvn = [
        0xe9, b'l', b'z', b'v', b'n', b' ', b'f', b'o', b'r', b'k', 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut fork = Vec::new();
    fork.extend(8u32.to_le_bytes());
    fork.extend((8 + lzvn.len() as u32).to_le_bytes());
    fork.extend(lzvn);

    let mut f = Fixture::new();
    f.compressed("stored", 1, 16, b"stored firmware\n", None);
    f.compressed("zlib", 3, 14, &zlib, None);
    f.compressed("fork", 8, 9, &[], Some(&fork));
    let image = f.build();
    assert_eq!(
        read(image.clone(), "/dir/stored").unwrap(),
        b"stored firmware\n"
    );
    assert_eq!(
        read(image.clone(), "/dir/zlib").unwrap(),
        b"zlib firmware\n"
    );
    assert_eq!(read(image, "/dir/fork").unwrap(), b"lzvn fork");
}

#[test]
fn limits_paths_and_kinds() {
    let mut f = Fixture::new();
    sample(&mut f);
    let mut container = Container::open(f.build()).unwrap();
    let mut volume = container.volume_named("Test").unwrap();
    let e = volume.read("/dir/large", 100).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (Code::TooLarge, "/dir/large: larger than 100 bytes")
    );
    let e = volume.read("/dir/nope", 100).unwrap_err();
    assert_eq!(e.message, "/dir/nope: not in volume Test");
    assert_eq!(
        volume.read("/dir/nope", 100).unwrap_err().code,
        Code::NotFound
    );
    assert_eq!(
        volume.read("/dir/../dir/plain", 100).unwrap_err().code,
        Code::InvalidInput
    );
    assert_eq!(
        volume.read("/dir", 100).unwrap_err().code,
        Code::InvalidInput
    );
    assert!(volume.list("/dir/plain").is_err());
    drop(volume);
    assert_eq!(
        container.volume_named("Other").err().unwrap().code,
        Code::NotFound
    );
}

#[test]
fn damaged_metadata_is_detected() {
    let mut f = Fixture::new();
    sample(&mut f);
    let image = f.build();
    // A flipped byte in the first leaf node fails its checksum.
    let mut damaged = image.clone();
    damaged[8 * BS + 100] ^= 1;
    assert!(read(damaged, "/dir/plain").is_err());
    // A container larger than its partition is refused.
    assert!(Container::open(image[..BLOCKS * BS - BS].to_vec()).is_err());
}

#[test]
fn files_with_missing_data_are_refused() {
    let mut f = Fixture::new();
    let id = 30;
    f.rows
        .push(inode(id, 9_000, 0o100644, false, Some(2 * BS as u64)));
    f.rows.push(entry(DIR, "short", id, 8, false));
    let block = f.data(&[1; BS]);
    f.rows.push(extent(9_000, 0, BS as u64, block));
    assert_eq!(
        read(f.build(), "/dir/short").unwrap_err().code,
        Code::Format
    );
}

//! Where a system is installed and the plan the device carries out, from the
//! layout the install system reports (`Inspect`). A port of orchard's
//! `install_layout.py` for GPT disks (install protocol 1); the device checks
//! the plan against its live table again before it changes anything.
//!
//! The rules: APFS and every partition that is not a Linux type are never
//! touched. A target is a free range or one existing Linux partition to
//! replace. The disk has at most one XBOOTLDR boot partition: an existing one
//! is reused, a missing one is made at the start of the target.
use orchard_cache::{Code, Error, Result};
use serde::Deserialize;
use orchard_formats::lwvm;
use serde_json::{json, Value};

pub const XBOOTLDR: &str = "bc13c2ff-59e6-4262-a352-b275fd6f7172";
pub const ROOT_ARM64: &str = "b921b045-1df0-41c3-af44-4c6f280d3fae";
pub const LINUX_DATA: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
pub const BOOT_BYTES: u64 = GIB;
pub const MIN_ROOT_BYTES: u64 = 8 * GIB;
pub const PLAN_VERSION: u64 = 2;
/// The boot partition on an LwVM disk, whose space is the tablet's own
/// flash: the kernel, initrd and device tree of a few systems fit, and
/// FAT32 is possible. It takes whole chunks, so at least this.
pub const LWVM_BOOT_BYTES: u64 = 512 * MIB;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Disk {
    pub path: String,
    pub sector: u64,
    #[serde(default)]
    pub bytes: u64,
    pub table_sha256: String,
    /// `lwvm` on the 32-bit boards, whose disk is iOS's; a GPT disk says
    /// nothing or `gpt`.
    #[serde(default)]
    pub table: Option<String>,
    /// On an LwVM disk: how many of its eight entries are taken.
    #[serde(default)]
    pub entries: Option<u64>,
    /// On an LwVM disk: the size of a chunk, the unit entries are made of.
    #[serde(default)]
    pub chunk_bytes: Option<u64>,
    /// On an LwVM disk: the header as the device read it, which a new
    /// entry is added to.
    #[serde(default, skip)]
    pub header: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Partition {
    pub number: u64,
    pub start: u64,
    pub blocks: u64,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub partuuid: String,
    /// `ext4`, `vfat`, `apfs` or None, from the partition's first 4 KiB.
    #[serde(default)]
    pub fs: Option<String>,
    #[serde(default)]
    pub fs_uuid: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// An LwVM entry's attributes; any (encryption) rule it out as a target.
    #[serde(default)]
    pub attributes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Free {
    pub start: u64,
    pub blocks: u64,
}

/// The layout the install system reports.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Layout {
    pub disk: Disk,
    pub partitions: Vec<Partition>,
    pub free: Vec<Free>,
}

impl Layout {
    pub fn from_inspect(value: &Value) -> Result<Layout> {
        if value["disk"]["table"].as_str() == Some("lwvm") && value["disk"]["header"].is_string() {
            return Layout::from_lwvm(value);
        }
        serde_json::from_value(value.clone()).map_err(|e| Error::new(Code::Format, format!("unreadable layout: {e}")))
    }

    /// The layout of an LwVM disk, from the header the install system
    /// hands over and the partitions its kernel made of it
    /// (`install_direct.inspect_lwvm`). The kernel's reader says where a
    /// partition lies, the header what it is; a partition whose chunks are
    /// scattered, which the kernel skips, is not offered, and one that lies
    /// elsewhere than its entry says stops the install.
    fn from_lwvm(value: &Value) -> Result<Layout> {
        let bad = |what: &str| Error::new(Code::Format, format!("unreadable LwVM layout: {what}"));
        let disk = &value["disk"];
        let text = |key: &str| disk[key].as_str().map(str::to_owned).ok_or_else(|| bad(key));
        let number = |key: &str| disk[key].as_u64().ok_or_else(|| bad(key));
        let hex = text("header")?;
        if hex.len() != lwvm::HEADER_BYTES * 2 || !hex.is_ascii() {
            return Err(bad("header"));
        }
        let header: Vec<u8> = (0..lwvm::HEADER_BYTES)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| bad("header")))
            .collect::<Result<_>>()?;
        let (sector, media) = (number("sector")?, number("bytes")?);
        if sector == 0 {
            return Err(bad("sector"));
        }
        let snapshot = lwvm::parse(&header, &header, Some(media)).map_err(|e| Error::new(Code::Format, format!("the LwVM header: {e}")))?;
        let mut partitions = Vec::new();
        for seen in value["kernel"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let (Some(index), Some(start), Some(sectors)) = (seen["number"].as_u64(), seen["start"].as_u64(), seen["sectors"].as_u64()) else {
                return Err(bad("kernel"));
            };
            let Some(entry) = snapshot.partitions.iter().find(|p| p.number as u64 == index) else {
                continue;
            };
            let in_place = entry.extents.len() == 1 && entry.extents[0].physical == start * 512 && entry.bytes == sectors * 512;
            if !in_place {
                return Err(Error::new(Code::Format, format!("partition {index} lies elsewhere than its LwVM entry says")));
            }
            let field = |key: &str| seen[key].as_str().map(str::to_owned);
            partitions.push(Partition {
                number: index,
                start: start * 512 / sector,
                blocks: sectors * 512 / sector,
                name: entry.name.clone(),
                kind: entry.kind.clone(),
                partuuid: entry.partuuid.clone(),
                fs: field("fs"),
                fs_uuid: field("fs_uuid"),
                label: field("label"),
                attributes: entry.attributes,
            });
        }
        partitions.sort_by_key(|p| p.start);
        let chunk = snapshot.chunk_bytes;
        let mut free: Vec<Free> = lwvm::free_space(&header, &snapshot).into_iter().map(|(first, count)| Free { start: first * chunk / sector, blocks: count * chunk / sector }).collect();
        free.sort_by_key(|f| f.start);
        Ok(Layout {
            disk: Disk {
                path: text("path")?,
                sector,
                bytes: media,
                table_sha256: text("table_sha256")?,
                table: Some("lwvm".into()),
                entries: Some(snapshot.partitions.len() as u64),
                chunk_bytes: Some(chunk),
                header: Some(header),
            },
            partitions,
            free,
        })
    }

    /// Whether the disk is partitioned with LwVM (32-bit boards): it has no
    /// boot partition, and a root goes into one Linux entry.
    pub fn lwvm(&self) -> bool {
        self.disk.table.as_deref() == Some("lwvm")
    }

    fn replaceable(kind: &str) -> bool {
        let kind = kind.to_ascii_lowercase();
        kind == ROOT_ARM64 || kind == LINUX_DATA
    }

    /// The disk's XBOOTLDR partition. More than one is refused.
    pub fn boot_partition(&self) -> Result<Option<&Partition>> {
        let boots: Vec<&Partition> = self.partitions.iter().filter(|p| p.kind.eq_ignore_ascii_case(XBOOTLDR)).collect();
        match boots.len() {
            0 => Ok(None),
            1 => Ok(Some(boots[0])),
            _ => Err(Error::new(Code::Format, "the disk has more than one XBOOTLDR boot partition")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Free,
    /// Replaces this Linux partition; everything on it is erased.
    Replace(Partition),
}

/// A place a system can go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    pub start: u64,
    pub blocks: u64,
    pub creates_boot: bool,
}

impl Target {
    pub fn bytes(&self, sector: u64) -> u64 {
        self.blocks * sector
    }
}

fn align_up(value: u64, step: u64) -> u64 {
    value.div_ceil(step) * step
}

fn align_down(value: u64, step: u64) -> u64 {
    value / step * step
}

/// Every allowed target, in disk order (`install_layout.options`).
pub fn targets(layout: &Layout) -> Result<Vec<Target>> {
    let block = layout.disk.sector;
    if block == 0 || !MIB.is_multiple_of(block) {
        return Err(Error::new(Code::Format, "unusual sector size"));
    }
    if layout.lwvm() {
        return Ok(lwvm_targets(layout));
    }
    let boot = layout.boot_partition()?;
    let needed = MIN_ROOT_BYTES + if boot.is_some() { 0 } else { BOOT_BYTES };
    let align = MIB / block;
    let mut out = Vec::new();
    for free in &layout.free {
        let start = align_up(free.start, align);
        let end = align_down(free.start + free.blocks, align);
        if end > start && (end - start) * block >= needed {
            out.push(Target { kind: Kind::Free, start, blocks: end - start, creates_boot: boot.is_none() });
        }
    }
    for part in &layout.partitions {
        if Layout::replaceable(&part.kind) && part.blocks * block >= needed {
            out.push(Target {
                kind: Kind::Replace(part.clone()),
                start: part.start,
                blocks: part.blocks,
                creates_boot: boot.is_none(),
            });
        }
    }
    out.sort_by_key(|t| t.start);
    Ok(out)
}

/// The targets of an LwVM disk (`install_layout.lwvm_options`): free space
/// a new Linux entry can take, where an entry is left, and the Linux
/// partitions a root fits in. Nothing is aligned: chunks are the unit.
fn lwvm_targets(layout: &Layout) -> Vec<Target> {
    let block = layout.disk.sector;
    let mut out = Vec::new();
    if layout.disk.entries.unwrap_or(8) < 8 {
        for free in &layout.free {
            if free.blocks * block >= MIN_ROOT_BYTES {
                out.push(Target { kind: Kind::Free, start: free.start, blocks: free.blocks, creates_boot: false });
            }
        }
    }
    for part in &layout.partitions {
        if part.kind.eq_ignore_ascii_case(LINUX_DATA) && part.attributes == 0 && part.blocks * block >= MIN_ROOT_BYTES {
            out.push(Target { kind: Kind::Replace(part.clone()), start: part.start, blocks: part.blocks, creates_boot: false });
        }
    }
    out.sort_by_key(|t| t.start);
    out
}

/// The plan for an LwVM disk (`install_layout.lwvm_plan`): the root fills
/// the chosen Linux partition, which keeps its place and identity, or a new
/// Linux entry over a free run. No partition is removed and there is no
/// boot partition: the boot entry goes to the root's own /boot.
#[allow(clippy::too_many_arguments)]
fn lwvm_plan(
    layout: &Layout,
    target: &Target,
    sizes: Sizes,
    image: Value,
    layer: Value,
    entry: Value,
    firmware: Option<Value>,
    account: Option<Value>,
    uuid: &mut dyn FnMut() -> String,
) -> Result<Value> {
    let block = layout.disk.sector;
    if matches!(&target.kind, Kind::Replace(part) if !part.kind.eq_ignore_ascii_case(LINUX_DATA) || part.attributes != 0) {
        return Err(Error::new(Code::Format, "on an LwVM disk only an existing, unencrypted Linux partition can be replaced"));
    }
    let entries = layout.disk.entries.unwrap_or(0);
    // The boot partition m1n1 starts the system from: the one that is
    // there, else a new one over the first chunks of the target's free
    // space, or of another free run when a partition is replaced. With a
    // header to add entries to, that is; without one (an install system
    // that hands none over) and without room, the boot entry goes to the
    // root's own /boot, where nothing on the device starts it.
    let existing = layout.partitions.iter().find(|p| p.kind.eq_ignore_ascii_case(XBOOTLDR) && p.attributes == 0);
    let chunk_blocks = layout.disk.chunk_bytes.map(|chunk| chunk / block).filter(|blocks| *blocks > 0);
    let boot_blocks = chunk_blocks.map(|chunk| (LWVM_BOOT_BYTES / block).div_ceil(chunk) * chunk);
    let (mut start, mut blocks) = (target.start, target.blocks);
    let mut next = entries + 1;
    let mut header = layout.disk.header.clone();
    let mut add = |header: &mut Option<Vec<u8>>, kind: &str, name: &str, partuuid: &str, start: u64, blocks: u64| -> Result<()> {
        let (Some(old), Some(chunk)) = (header.as_ref(), chunk_blocks) else {
            return Err(Error::new(Code::Format, "the install system handed over no LwVM header to add an entry to"));
        };
        let (made, _) = lwvm::allocate(old, old, kind, name, partuuid, start / chunk, blocks / chunk)
            .map_err(|e| Error::new(Code::Format, format!("the new LwVM entry: {e}")))?;
        *header = Some(made);
        Ok(())
    };
    let boot = match (existing, boot_blocks) {
        (Some(part), _) => json!({"create": false, "number": part.number, "start": part.start, "blocks": part.blocks,
                                  "partuuid": part.partuuid, "fs_uuid": part.fs_uuid}),
        (None, Some(wanted)) if layout.disk.header.is_some() && entries + 2 <= 8 => {
            let place = match &target.kind {
                Kind::Free if blocks >= wanted + MIN_ROOT_BYTES / block => {
                    let at = start;
                    start += wanted;
                    blocks -= wanted;
                    Some(at)
                }
                Kind::Free => None,
                Kind::Replace(_) => layout.free.iter().find(|f| f.blocks >= wanted).map(|f| f.start),
            };
            match place {
                Some(at) => {
                    let partuuid = uuid();
                    let fs_uuid: String = uuid().replace('-', "")[..8].to_ascii_uppercase();
                    add(&mut header, XBOOTLDR, "Boot", &partuuid, at, wanted)?;
                    let number = next;
                    next += 1;
                    json!({"create": true, "number": number, "start": at, "blocks": wanted, "partuuid": partuuid, "fs_uuid": fs_uuid})
                }
                None => Value::Null,
            }
        }
        _ => Value::Null,
    };
    let (number, partuuid, create) = match &target.kind {
        Kind::Free => {
            let partuuid = uuid();
            if layout.disk.header.is_some() {
                add(&mut header, LINUX_DATA, "Linux", &partuuid, start, blocks)?;
            }
            (next, partuuid, true)
        }
        Kind::Replace(part) => (part.number, part.partuuid.clone(), false),
    };
    if blocks * block < MIN_ROOT_BYTES {
        return Err(Error::new(Code::Format, "the target is too small for a root partition"));
    }
    if ((blocks * block) as f64) < (sizes.image_expanded + sizes.layer_expanded) as f64 * 1.25 {
        return Err(Error::new(Code::Format, "the target is too small for this system"));
    }
    let label: String = entry.get("id").and_then(Value::as_str).unwrap_or("linux").chars().take(16).collect();
    let mut disk = json!({"path": layout.disk.path, "sector": block, "table": "lwvm", "table_sha256": layout.disk.table_sha256});
    let created = create || boot["create"].as_bool() == Some(true);
    if let (true, Some(header)) = (created, header.filter(|_| layout.disk.header.is_some())) {
        disk["header"] = Value::String(header.iter().map(|b| format!("{b:02x}")).collect());
    }
    Ok(json!({
        "version": PLAN_VERSION,
        "disk": disk,
        "remove": Value::Null,
        "boot": boot,
        "root": {"number": number, "start": start, "blocks": blocks, "partuuid": partuuid, "create": create,
                 "fs_uuid": uuid(), "fs": "ext4", "label": label},
        "image": image,
        "layer": layer,
        "entry": entry,
        "firmware": firmware,
        "account": account,
    }))
}

/// The installer's choice: everything outside iPadOS is Omarchy's, so the
/// one target, or the largest of several.
pub fn choose(layout: &Layout) -> Result<Target> {
    targets(layout)?
        .into_iter()
        .max_by_key(|t| t.blocks)
        .ok_or_else(|| Error::new(Code::NotFound, "there is no room for a system outside iPadOS"))
}

/// Sizes of what is written, as the release states them.
#[derive(Debug, Clone, Copy)]
pub struct Sizes {
    pub image_expanded: u64,
    pub layer_expanded: u64,
}

/// The plan the device carries out (`install_layout.plan`, version 2).
/// `image`, `layer`, `entry`, `firmware` and `account` are passed through as
/// the release and the installer make them; `uuid` makes random UUIDs.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    layout: &Layout,
    target: &Target,
    sizes: Sizes,
    image: Value,
    layer: Value,
    entry: Value,
    firmware: Option<Value>,
    account: Option<Value>,
    uuid: &mut dyn FnMut() -> String,
) -> Result<Value> {
    if layout.lwvm() {
        return lwvm_plan(layout, target, sizes, image, layer, entry, firmware, account, uuid);
    }
    let block = layout.disk.sector;
    let align = MIB / block;
    let boot = layout.boot_partition()?;
    let (mut start, end) = (target.start, target.start + target.blocks);
    if matches!(target.kind, Kind::Replace(_)) {
        // A replaced partition's range is reused; new partitions start aligned.
        start = align_up(start, align);
    }
    let boot_plan = match boot {
        Some(b) => json!({"create": false, "partuuid": b.partuuid, "number": b.number}),
        None => {
            let blocks = BOOT_BYTES / block;
            let fs_uuid: String = uuid().replace('-', "")[..8].to_ascii_uppercase();
            let made = json!({"create": true, "start": start, "blocks": blocks, "partuuid": uuid(), "fs_uuid": fs_uuid});
            start += blocks;
            made
        }
    };
    let root_blocks = align_down(end, align).saturating_sub(start);
    if root_blocks * block < MIN_ROOT_BYTES {
        return Err(Error::new(Code::Format, "the target is too small for a root partition"));
    }
    let needed = (sizes.image_expanded + sizes.layer_expanded) as f64 * 1.25;
    if ((root_blocks * block) as f64) < needed {
        return Err(Error::new(Code::Format, "the target is too small for this system"));
    }
    let remove = match &target.kind {
        Kind::Replace(p) => json!({"number": p.number, "partuuid": p.partuuid, "start": p.start, "blocks": p.blocks}),
        Kind::Free => Value::Null,
    };
    let label: String = entry.get("id").and_then(Value::as_str).unwrap_or("linux").chars().take(16).collect();
    Ok(json!({
        "version": PLAN_VERSION,
        "disk": {"path": layout.disk.path, "sector": block, "table_sha256": layout.disk.table_sha256},
        "remove": remove,
        "boot": boot_plan,
        "root": {"start": start, "blocks": root_blocks, "partuuid": uuid(), "fs_uuid": uuid(), "fs": "ext4", "label": label},
        "image": image,
        "layer": layer,
        "entry": entry,
        "firmware": firmware,
        "account": account,
    }))
}

/// The boot entry of a release (`service.entry_for`).
pub fn entry(name: &str, cmdline: Option<&str>, devicetree: &str) -> Value {
    let mut id = String::new();
    for c in name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-');
    json!({
        "id": if id.is_empty() { "linux" } else { id },
        "title": name,
        "options": format!("{} fbcon=map:9 vt.global_cursor_default=0", cmdline.unwrap_or("rootwait")),
        "devicetree": devicetree,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const APFS: &str = "7c3457ef-0000-11aa-aa11-00306543ecac";
    const SECTOR: u64 = 4096;
    const G: u64 = GIB / SECTOR;

    fn part(number: u64, start: u64, blocks: u64, kind: &str) -> Partition {
        Partition { number, start, blocks, name: String::new(), kind: kind.into(), partuuid: format!("p{number}"), fs: None, fs_uuid: None, label: None, attributes: 0 }
    }

    fn layout(partitions: Vec<Partition>, free: Vec<Free>) -> Layout {
        Layout { disk: Disk { path: "/dev/nvme0n1".into(), sector: SECTOR, bytes: 128 * GIB, table_sha256: "t".into(), table: None, entries: None, chunk_bytes: None, header: None }, partitions, free }
    }

    fn uuids() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("0000000{n}-0000-4000-8000-000000000000")
        }
    }

    fn sizes() -> Sizes {
        Sizes { image_expanded: 3_318_181_888, layer_expanded: 26_440_929 }
    }

    /// LwVM disks against orchard's Python (`install_layout`): its targets
    /// and plans for a disk with free space and for one with a Linux
    /// partition to replace, on its tests' synthetic headers.
    #[test]
    fn lwvm_targets_and_plans_are_pythons() {
        let cases: Value = serde_json::from_str(include_str!("testdata-lwvm-plans.json")).unwrap();
        for name in ["free", "replace"] {
            let case = &cases[name];
            let layout = Layout::from_inspect(&case["layout"]).unwrap();
            assert!(layout.lwvm());
            let found = targets(&layout).unwrap();
            let listed: Vec<Value> = found
                .iter()
                .map(|t| json!({"kind": if t.kind == Kind::Free { "free" } else { "replace" }, "start": t.start, "blocks": t.blocks}))
                .collect();
            assert_eq!(json!(listed), case["options"], "{name}");
            assert!(!found[0].creates_boot);
            let sizes = Sizes { image_expanded: case["image"]["expanded_bytes"].as_u64().unwrap(), layer_expanded: case["layer"]["expanded_bytes"].as_u64().unwrap() };
            let made = plan(&layout, &found[0], sizes, case["image"].clone(), case["layer"].clone(), case["entry"].clone(), None, None, &mut uuids()).unwrap();
            assert_eq!(made, case["plan"], "{name}");
        }
    }

    /// What the install system answers for an LwVM disk: the header and
    /// the kernel's partitions (docs/install-protocol.md in orchard).
    fn lwvm_inspect() -> Value {
        let header = include_bytes!("testdata-lwvm-shrunk.bin");
        let snapshot = lwvm::parse(header, header, None).unwrap();
        let kernel: Vec<Value> = snapshot
            .partitions
            .iter()
            .map(|p| json!({"number": p.number, "start": p.extents[0].physical / 512, "sectors": p.bytes / 512, "fs": null, "fs_uuid": null, "label": null}))
            .collect();
        json!({
            "disk": {"path": "/dev/applesftl0", "sector": 4096, "bytes": snapshot.media_bytes, "table": "lwvm", "table_sha256": "t",
                     "chunk_bytes": snapshot.chunk_bytes, "header": header.iter().map(|b| format!("{b:02x}")).collect::<String>()},
            "kernel": kernel,
        })
    }

    #[test]
    fn an_lwvm_disk_gets_a_boot_partition_and_a_root_in_one_new_header() {
        let layout = Layout::from_inspect(&lwvm_inspect()).unwrap();
        assert!(layout.lwvm() && layout.disk.entries == Some(2) && layout.partitions.len() == 2);
        let chunk = layout.disk.chunk_bytes.unwrap() / 4096;
        let target = choose(&layout).unwrap();
        assert_eq!((target.kind.clone(), target.start, target.blocks), (Kind::Free, 601 * chunk, 352 * chunk));
        let sizes = Sizes { image_expanded: 1 << 30, layer_expanded: 1 << 20 };
        let entry = entry("Minimal", None, "s5l8942x-p105");
        let made = plan(&layout, &target, sizes, json!({"bytes": 1}), json!({"path": "/run/orchard/layer"}), entry, None, None, &mut uuids()).unwrap();
        // The boot partition takes the first whole chunks that hold 512 MiB, the root the rest.
        let boot_chunks = (LWVM_BOOT_BYTES / 4096).div_ceil(chunk);
        assert_eq!(made["boot"], json!({"create": true, "number": 3, "start": 601 * chunk, "blocks": boot_chunks * chunk,
                                        "partuuid": "00000001-0000-4000-8000-000000000000", "fs_uuid": "00000002"}));
        assert_eq!((made["root"]["number"].as_u64(), made["root"]["start"].as_u64(), made["root"]["blocks"].as_u64(), made["root"]["create"].as_bool()),
                   (Some(4), Some((601 + boot_chunks) * chunk), Some((352 - boot_chunks) * chunk), Some(true)));
        // The header the device is to write: the old one with exactly these two entries.
        let hex = made["disk"]["header"].as_str().unwrap();
        let header: Vec<u8> = (0..4096).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()).collect();
        let after = lwvm::parse(&header, &header, None).unwrap();
        assert_eq!(after.partitions.len(), 4);
        assert_eq!(after.partitions[..2], lwvm::parse(layout.disk.header.as_ref().unwrap(), layout.disk.header.as_ref().unwrap(), None).unwrap().partitions[..]);
        assert_eq!((after.partitions[2].kind.as_str(), after.partitions[2].name.as_str()), (XBOOTLDR, "Boot"));
        assert_eq!((after.partitions[3].kind.as_str(), after.partitions[3].partuuid.as_str()), (LINUX_DATA, made["root"]["partuuid"].as_str().unwrap()));
        assert_eq!(after.partitions[3].extents.len(), 1);
    }

    #[test]
    fn an_lwvm_partition_the_kernel_shows_elsewhere_stops_the_install() {
        let mut inspect = lwvm_inspect();
        inspect["kernel"][1]["start"] = json!(8);
        assert!(Layout::from_inspect(&inspect).unwrap_err().message.contains("lies elsewhere"));
    }

    #[test]
    fn an_encrypted_or_foreign_lwvm_partition_is_no_target() {
        let cases: Value = serde_json::from_str(include_str!("testdata-lwvm-plans.json")).unwrap();
        let mut layout = Layout::from_inspect(&cases["replace"]["layout"]).unwrap();
        for part in &mut layout.partitions {
            part.attributes = 0x1000_0000_0000;
        }
        assert!(targets(&layout).unwrap().is_empty());
        // All eight entries taken: free space cannot get one.
        let mut full = Layout::from_inspect(&cases["free"]["layout"]).unwrap();
        full.disk.entries = Some(8);
        assert!(targets(&full).unwrap().is_empty());
    }

    fn make(l: &Layout, t: &Target) -> Result<Value> {
        let e = entry("Omarchy A", None, "t8010-j71b");
        plan(l, t, sizes(), json!({"bytes": 1}), json!({"path": "/run/orchard/layer"}), e, None, None, &mut uuids())
    }

    #[test]
    fn fresh_split_installs_into_the_free_space_and_makes_boot() {
        let l = layout(vec![part(1, 6, 96 * G, APFS)], vec![Free { start: 96 * G + 6, blocks: 32 * G - 6 }]);
        let t = choose(&l).unwrap();
        assert_eq!(t.kind, Kind::Free);
        let p = make(&l, &t).unwrap();
        assert_eq!(p["boot"]["create"], true);
        assert_eq!(p["boot"]["fs_uuid"], "00000001");
        assert_eq!(p["remove"], Value::Null);
        assert_eq!(p["root"]["label"], "omarchy-a");
        assert_eq!(p["root"]["start"].as_u64().unwrap() % (MIB / SECTOR), 0, "aligned");
    }

    #[test]
    fn reinstall_replaces_the_root_and_reuses_boot() {
        let l = layout(
            vec![part(1, 6, 64 * G, APFS), part(2, 64 * G + 6, G, XBOOTLDR), part(3, 65 * G + 6, 63 * G - 6, ROOT_ARM64)],
            vec![],
        );
        let t = choose(&l).unwrap();
        assert!(matches!(&t.kind, Kind::Replace(p) if p.number == 3));
        let p = make(&l, &t).unwrap();
        assert_eq!(p["boot"], json!({"create": false, "partuuid": "p2", "number": 2}));
        assert_eq!(p["remove"]["number"], 3);
    }

    #[test]
    fn apfs_and_other_partitions_are_never_targets() {
        let l = layout(vec![part(1, 6, 128 * G, APFS)], vec![]);
        assert!(choose(&l).unwrap_err().to_string().contains("no room"));
    }

    #[test]
    fn two_boot_partitions_are_refused() {
        let l = layout(vec![part(1, 6, G, XBOOTLDR), part(2, G + 6, G, XBOOTLDR)], vec![Free { start: 2 * G + 6, blocks: 20 * G }]);
        assert!(targets(&l).is_err());
    }

    #[test]
    fn a_target_too_small_for_the_system_is_refused() {
        let l = layout(vec![part(1, 6, 100 * G, APFS)], vec![Free { start: 100 * G + 6, blocks: 9 * G + 300 }]);
        let t = choose(&l).unwrap();
        let big = Sizes { image_expanded: 7 * GIB, layer_expanded: 0 };
        let e = entry("Omarchy A", None, "x");
        let err = plan(&l, &t, big, json!({}), json!({}), e, None, None, &mut uuids()).unwrap_err();
        assert!(err.to_string().contains("too small"));
    }

    #[test]
    fn entry_ids_are_slugs() {
        assert_eq!(entry("Omarchy A", None, "d")["id"], "omarchy-a");
        assert_eq!(entry("!!!", Some("quiet"), "d")["options"], "quiet fbcon=map:9 vt.global_cursor_default=0");
        assert_eq!(entry("!!!", None, "d")["id"], "linux");
    }
}

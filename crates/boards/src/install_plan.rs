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
use serde_json::{json, Value};

pub const XBOOTLDR: &str = "bc13c2ff-59e6-4262-a352-b275fd6f7172";
pub const ROOT_ARM64: &str = "b921b045-1df0-41c3-af44-4c6f280d3fae";
pub const LINUX_DATA: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
pub const BOOT_BYTES: u64 = GIB;
pub const MIN_ROOT_BYTES: u64 = 8 * GIB;
pub const PLAN_VERSION: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Disk {
    pub path: String,
    pub sector: u64,
    #[serde(default)]
    pub bytes: u64,
    pub table_sha256: String,
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
        serde_json::from_value(value.clone()).map_err(|e| Error::new(Code::Format, format!("unreadable layout: {e}")))
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
        Partition { number, start, blocks, name: String::new(), kind: kind.into(), partuuid: format!("p{number}"), fs: None, fs_uuid: None, label: None }
    }

    fn layout(partitions: Vec<Partition>, free: Vec<Free>) -> Layout {
        Layout { disk: Disk { path: "/dev/nvme0n1".into(), sector: SECTOR, bytes: 128 * GIB, table_sha256: "t".into() }, partitions, free }
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

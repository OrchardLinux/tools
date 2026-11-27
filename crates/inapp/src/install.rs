//! The install, top to bottom: the partitions, the root image, the finished
//! root and the boot filesystem. It takes all the space after iOS: Linux
//! partitions there are erased and a boot and a root partition are made in
//! their place, by orchard's own plan (`install_plan`). iOS's partition and
//! everything before it are never written.
use crate::disk::Disk;
use crate::download::hex;
use crate::ext4::Ext4;
use crate::finish::{self, Kernel, System};
use crate::flow::Account;
use crate::image::{self, Map};
use orchard_boards::install_plan::{self, Free, Layout, Sizes, LINUX_DATA, ROOT_ARM64, XBOOTLDR};
use orchard_boards::release::Receipt;
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::{fat32, gpt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const APFS: &str = "7c3457ef-0000-11aa-aa11-00306543ecac";

/// Whether the installer may erase a partition of this type.
pub fn ours(kind: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    kind == ROOT_ARM64 || kind == LINUX_DATA || kind == XBOOTLDR
}

/// Everything an install is made from, all of it checked already.
pub struct Inputs<'a> {
    /// The system's name, as the boot entry shows it.
    pub name: &'a str,
    pub cmdline: Option<&'a str>,
    /// `[install.account]` of the release.
    pub account: Option<&'a Value>,
    pub secrets: &'a Account,
    /// The root image in the cache, with what the release states about it.
    pub image: &'a Path,
    pub image_receipt: &'a Receipt,
    pub map: &'a Map,
    pub layer_expanded: u64,
    pub kernel: Kernel<'a>,
    /// The board's device tree, without `.dtb`.
    pub devicetree: &'a str,
    /// (path below the root, content) of the board's firmware.
    pub firmware: &'a [(String, Vec<u8>)],
    pub timezone: Option<&'a str>,
    /// Where e2fsprogs' programs are.
    pub tools: &'a Path,
    /// A directory of the helper's own for what is in flight.
    pub work: &'a Path,
    /// Seconds since 1970.
    pub now: u64,
}

/// What the installer hears while it works.
pub enum Step<'a> {
    /// Percent of the root image written.
    Writing(u8),
    /// A finishing step started (the install system's names).
    Finishing(&'a str),
}

/// What an install left on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub root_partuuid: String,
    pub boot_partuuid: String,
    /// The boot entry, or `None` when the kernel has no device tree for the board.
    pub entry: Option<String>,
}

fn uuid4() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// iOS's partition and the numbers of the partitions after it, which must
/// all be the installer's to erase.
pub fn after_ios(table: &gpt::Table) -> Result<(gpt::Partition, Vec<u32>)> {
    let apfs: Vec<&gpt::Partition> = table.partitions.iter().filter(|p| gpt::guid_text(&p.kind) == APFS).collect();
    let [ios] = apfs[..] else {
        return Err(Error::new(Code::Unsupported, "the disk does not have exactly one APFS partition"));
    };
    let mut remove = Vec::new();
    for p in table.partitions.iter().filter(|p| p.first > ios.last) {
        if !ours(&gpt::guid_text(&p.kind)) {
            return Err(Error::new(Code::Unsupported, format!("partition {} after iOS is not a Linux partition", p.number)));
        }
        remove.push(p.number);
    }
    Ok((ios.clone(), remove))
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value.get(key).filter(|v| !v.is_null()).ok_or_else(|| Error::new(Code::Format, format!("the install plan lacks {key}")))
}

fn number(value: &Value, key: &str) -> Result<u64> {
    field(value, key)?.as_u64().ok_or_else(|| Error::new(Code::Format, format!("the install plan's {key} is not a number")))
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    field(value, key)?.as_str().ok_or_else(|| Error::new(Code::Format, format!("the install plan's {key} is not text")))
}

pub fn run(disk: &Disk, inputs: &Inputs, report: &dyn Fn(Step)) -> Result<Record> {
    let (primary, backup) = disk.tables()?;
    let (ios, remove) = after_ios(&primary)?;

    // The plan, on the table as it is once the old partitions are gone.
    let emptied = gpt::change(&primary, &backup, &remove, &[])?;
    let after = gpt::parse(&emptied.primary_header, &emptied.entries)?;
    let layout = Layout {
        disk: install_plan::Disk { path: disk.path.display().to_string(), sector: disk.sector, bytes: disk.bytes, table_sha256: hex(&Sha256::digest(primary.identity())), table: None, entries: None, chunk_bytes: None, header: None },
        partitions: after
            .partitions
            .iter()
            .map(|p| install_plan::Partition {
                number: p.number as u64,
                start: p.first,
                blocks: p.blocks(),
                name: p.name.clone(),
                kind: gpt::guid_text(&p.kind),
                partuuid: gpt::guid_text(&p.guid),
                fs: None,
                fs_uuid: None,
                label: None,
                attributes: 0,
            })
            .collect(),
        free: after.free().into_iter().filter(|&(start, _)| start > ios.last).map(|(start, blocks)| Free { start, blocks }).collect(),
    };
    let target = install_plan::choose(&layout)?;
    let entry = install_plan::entry(inputs.name, inputs.cmdline, &format!("{}.dtb", inputs.devicetree));
    let sizes = Sizes { image_expanded: inputs.image_receipt.expanded_bytes.unwrap_or(0), layer_expanded: inputs.layer_expanded };
    let plan = install_plan::plan(&layout, &target, sizes, json!({}), json!({}), entry.clone(), None, None, &mut uuid4)?;
    let (boot, root) = (field(&plan, "boot")?, field(&plan, "root")?);
    let label = string(root, "label")?;
    let new = |spec: &Value, kind: &str, name: &str| -> Result<gpt::New> {
        Ok(gpt::New { kind: gpt::guid_bytes(kind)?, guid: gpt::guid_bytes(string(spec, "partuuid")?)?, first: number(spec, "start")?, blocks: number(spec, "blocks")?, name: name.into() })
    };
    let (new_boot, new_root) = (new(boot, XBOOTLDR, "boot")?, new(root, ROOT_ARM64, label)?);
    if new_boot.first <= ios.last || new_root.first <= ios.last {
        return Err(Error::new(Code::Format, "the install plan reaches into iOS"));
    }
    let changed = gpt::change(&primary, &backup, &remove, &[new_boot.clone(), new_root.clone()])?;
    if changed.entries[(ios.number as usize - 1) * 128..ios.number as usize * 128] != primary.entries[(ios.number as usize - 1) * 128..ios.number as usize * 128] {
        return Err(Error::new(Code::Format, "the new partition table changes iOS's partition"));
    }
    disk.write_tables(&primary, &backup, &changed).context("the partition table")?;
    let boot_part = disk.part(changed.numbers[0], new_boot.blocks)?;
    let root_part = disk.part(changed.numbers[1], new_root.blocks)?;

    let total = inputs.map.restored_bytes().max(1);
    image::restore(inputs.image, inputs.image_receipt.bytes, &inputs.image_receipt.sha256, inputs.map, &root_part, &|done| {
        report(Step::Writing((done * 100 / total).min(100) as u8))
    })
    .context("writing the system")?;

    report(Step::Finishing("filesystem"));
    let fs = Ext4::new(inputs.tools, &root_part.tool_path, &inputs.work.join("ext4"))?;
    fs.identify(string(root, "fs_uuid")?, label).context("the root filesystem")?;
    fs.resize(new_root.blocks * disk.sector / 4096).context("the root filesystem")?;
    let (root_partuuid, boot_partuuid) = (string(root, "partuuid")?, string(boot, "partuuid")?);
    let system = System {
        entry: &entry,
        root_partuuid,
        boot_partuuid,
        account: inputs.account,
        secrets: inputs.secrets,
        timezone: inputs.timezone,
        today: inputs.now / 86_400,
        now: inputs.now,
    };
    let finished = finish::finish(&fs, &system, inputs.firmware, &inputs.kernel, &|name| report(Step::Finishing(name))).context("finishing the system")?;
    fs.check().context("the finished root filesystem")?;

    // A new boot filesystem with the entry's files.
    let volume = u32::from_str_radix(string(boot, "fs_uuid")?, 16).map_err(|_| Error::new(Code::Format, "the install plan's boot filesystem id is not hexadecimal"))?;
    let files: Vec<fat32::File> = finished.boot_files.iter().map(|(path, data)| fat32::File { path, data }).collect();
    let (days, seconds) = (inputs.now / 86_400, inputs.now % 86_400);
    let (year, month, day) = civil(days);
    let stamp = fat32::Stamp::new(year, month, day, (seconds / 3600) as u16, (seconds / 60 % 60) as u16);
    for (offset, bytes) in fat32::build(new_boot.blocks, disk.sector as u32, volume, b"BOOT       ", &files, stamp)? {
        boot_part.write_at(&bytes, offset).context("the boot filesystem")?;
    }
    report(Step::Finishing("sync"));
    boot_part.sync()?;
    root_part.sync()?;
    Ok(Record { root_partuuid: root_partuuid.into(), boot_partuuid: boot_partuuid.into(), entry: finished.entry })
}

/// (year, month, day) of a day count since 1970.
fn civil(days: u64) -> (u16, u16, u16) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as u16, month as u16, day as u16)
}

/// Where the helper keeps a file in flight.
pub fn work_dir(cache: &Path) -> PathBuf {
    cache.join("work")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext4::tests::host_tools;
    use crate::ext4::Change;
    use crate::fixture::tables;
    use std::os::unix::fs::FileExt;
    use std::process::Command;

    const SECTOR: u64 = 4096;
    const GIB: u64 = 1 << 30;

    #[test]
    fn days_become_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_783), (2026, 11, 26));
    }

    #[test]
    fn only_linux_partitions_after_ios_are_erased() {
        let (primary, _, entries) = tables(SECTOR, 1000, &[(APFS, 6, 500), (XBOOTLDR, 600, 50), (ROOT_ARM64, 700, 100)]);
        let (ios, remove) = after_ios(&gpt::parse(&primary, &entries).unwrap()).unwrap();
        assert_eq!((ios.number, remove), (1, vec![2, 3]));
        let (primary, _, entries) = tables(SECTOR, 1000, &[(APFS, 6, 500), ("c12a7328-f81f-11d2-ba4b-00a0c93ec93b", 600, 50)]);
        assert!(after_ios(&gpt::parse(&primary, &entries).unwrap()).is_err());
    }

    /// A whole install onto a disk kept in a sparse file: a disk of 16 GiB
    /// with iOS in its first 4 GiB and an old Linux partition after it, a
    /// small root image made here, a layer and an account.
    #[test]
    fn an_install_onto_a_disk_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let Some(tools) = host_tools() else {
            eprintln!("skipped: needs ios-installer/out/e2fsprogs-host");
            return;
        };
        // The root image: a 64 MiB ext4 with the account's files, as blocks.
        let Some(root) = crate::ext4::tests::image(dir.path(), 64) else {
            eprintln!("skipped: needs mke2fs");
            return;
        };
        let fs = Ext4::new(&tools, root.to_str().unwrap(), &dir.path().join("prepare")).unwrap();
        fs.apply(&[
            Change::file("/etc/passwd", "root:x:0:0::/root:/bin/sh\ntablet:x:1000:1000::/home/tablet:/bin/bash\n", 0o644),
            Change::file("/etc/shadow", "root:!:20000::::::\ntablet:!:20000:0:99999:7:::\n", 0o600),
        ])
        .unwrap();
        let raw = std::fs::read(&root).unwrap();
        let blocks = (64u64 << 20) / 4096;
        let stream = dir.path().join("root.blocks.zst");
        let packed = Command::new("zstd").args(["-q", "-f", "-o"]).arg(&stream).arg("-").stdin(std::process::Stdio::piped()).spawn();
        let Ok(mut packed) = packed else {
            eprintln!("skipped: needs zstd");
            return;
        };
        std::io::Write::write_all(&mut packed.stdin.take().unwrap(), &raw[..(blocks * 4096) as usize]).unwrap();
        assert!(packed.wait().unwrap().success());
        let image = std::fs::read(&stream).unwrap();
        let receipt = Receipt { url: String::new(), bytes: image.len() as u64, sha256: hex(&Sha256::digest(&image)), expanded_bytes: Some(64 << 20) };
        let map = Map::parse(format!("{{\"format\":1,\"block_size\":4096,\"ranges\":[[0,{blocks}]]}}").as_bytes()).unwrap();

        // The disk.
        let total = 16 * GIB / SECTOR;
        let ios_blocks = 4 * GIB / SECTOR;
        let (primary, backup, entries) = tables(SECTOR, total, &[(APFS, 6, ios_blocks), (LINUX_DATA, ios_blocks + 1000, 5000)]);
        let path = dir.path().join("disk");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(total * SECTOR).unwrap();
        file.write_all_at(&primary, SECTOR).unwrap();
        file.write_all_at(&entries, 2 * SECTOR).unwrap();
        file.write_all_at(&entries, (total - 5) * SECTOR).unwrap();
        file.write_all_at(&backup, (total - 1) * SECTOR).unwrap();
        file.write_all_at(b"iOS's data", 6 * SECTOR).unwrap();
        let disk = Disk::open(&path).unwrap();

        let layer = crate::finish::tests::layer();
        let account = json!({"user": "tablet", "password": true, "pin": true, "passwordless_sudo": false});
        let secrets = Account { password: "correct horse".into(), pin: "123456".into() };
        let firmware = vec![("lib/firmware/brcm/a.bin".to_string(), b"FW".to_vec())];
        let inputs = Inputs {
            name: "Omarchy A",
            cmdline: None,
            account: Some(&account),
            secrets: &secrets,
            image: &stream,
            image_receipt: &receipt,
            map: &map,
            layer_expanded: 1 << 20,
            kernel: Kernel::Layer(&layer),
            devicetree: "t8010-j71b",
            firmware: &firmware,
            timezone: None,
            tools: &tools,
            work: &dir.path().join("work"),
            now: 1_795_700_000,
        };
        let steps = std::cell::RefCell::new(Vec::new());
        let record = run(&disk, &inputs, &|step| {
            if let Step::Finishing(name) = step {
                steps.borrow_mut().push(name.to_owned());
            }
        })
        .unwrap();
        assert_eq!(*steps.borrow(), ["filesystem", "firmware", "account", "kernel", "entry", "sync"]);

        // The table: iOS as it was, the old partition gone, boot and root in its place.
        let (table, _) = disk.tables().unwrap();
        let kinds: Vec<String> = table.partitions.iter().map(|p| gpt::guid_text(&p.kind)).collect();
        assert_eq!(kinds, [APFS, XBOOTLDR, ROOT_ARM64]);
        assert_eq!(table.partitions[0].blocks(), ios_blocks);
        assert_eq!(table.partitions[1].blocks() * SECTOR, GIB);
        assert_eq!(gpt::guid_text(&table.partitions[2].guid), record.root_partuuid);
        assert_eq!(table.partitions[2].name, "omarchy-a");
        let mut head = [0u8; 10];
        std::fs::File::open(&path).unwrap().read_exact_at(&mut head, 6 * SECTOR).unwrap();
        assert_eq!(&head, b"iOS's data");

        // The root: grown to its partition, named, finished and clean.
        let root = disk.part(3, table.partitions[2].blocks()).unwrap();
        let fs = Ext4::new(&tools, &root.tool_path, &dir.path().join("verify")).unwrap();
        fs.check().unwrap();
        let fstab = String::from_utf8(fs.read("/etc/fstab").unwrap().unwrap()).unwrap();
        assert!(fstab.contains(&format!("PARTUUID={} / ext4", record.root_partuuid)) && fstab.contains(&format!("PARTUUID={} /boot vfat", record.boot_partuuid)));
        assert_eq!(fs.read("/usr/lib/modules/7.3.0-test/vmlinuz").unwrap().unwrap(), b"KERNEL");
        let superblock = root.read_at(1024, 1024).unwrap();
        let fs_blocks = u32::from_le_bytes(superblock[4..8].try_into().unwrap()) as u64;
        assert!(table.partitions[2].blocks() - fs_blocks < 8, "the filesystem fills its partition");
        assert_eq!(disk.describe(3, table.partitions[2].first), (Some("ext4"), Some("omarchy-a".into())));
        assert_eq!(disk.describe(2, table.partitions[1].first), (Some("FAT32"), Some("BOOT".into())));

        // The boot filesystem, as this computer's FAT checker reads it.
        let boot = disk.part(2, table.partitions[1].blocks()).unwrap();
        let first = boot.read_at(4096, 0).unwrap();
        assert_eq!(crate::disk::filesystem(&first), Some("FAT32"));
        assert_eq!(record.entry.as_deref(), Some(&format!("omarchy-a-{}-7.3.0-test", record.root_partuuid.split('-').next().unwrap())[..]));
    }
}

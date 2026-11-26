//! Drives the engine on this computer, as the helper does on a device: a
//! disk kept in a file (made here with iOS's partition and free space after
//! it), storage reports once a second, and the app's requests.
//!
//!     cargo run --release -p orchard-inapp --example drive -- DIR MACHINE [INDEX]
//!
//! DIR gets the disk, the cache and the log. MACHINE is Apple's product
//! type, such as iPad7,5. E2FSPROGS names the directory with e2fsprogs'
//! programs (default: ios-installer/out/e2fsprogs-host).
use orchard_formats::gpt;
use orchard_inapp::disk::Disk;
use orchard_inapp::engine::{Config, Engine};
use serde_json::{json, Value};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SECTOR: u64 = 4096;
const APFS: &str = "7c3457ef-0000-11aa-aa11-00306543ecac";

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

/// A 64 GiB disk whose first half is iOS's.
fn make_disk(path: &Path) {
    let total = (64u64 << 30) / SECTOR;
    let mut entries = vec![0u8; 128 * 128];
    entries[..16].copy_from_slice(&gpt::guid_bytes(APFS).unwrap());
    entries[16..32].copy_from_slice(&[7; 16]);
    entries[32..40].copy_from_slice(&6u64.to_le_bytes());
    entries[40..48].copy_from_slice(&(total / 2 - 1).to_le_bytes());
    let header = |my: u64, other: u64, entries_lba: u64| {
        let mut h = vec![0u8; SECTOR as usize];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&6u64.to_le_bytes());
        h[48..56].copy_from_slice(&(total - 6).to_le_bytes());
        h[56..72].copy_from_slice(&[0xd1; 16]);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32(&entries).to_le_bytes());
        let sum = crc32(&h[..92]);
        h[16..20].copy_from_slice(&sum.to_le_bytes());
        h
    };
    let file = std::fs::File::create(path).unwrap();
    file.set_len(total * SECTOR).unwrap();
    file.write_all_at(&header(1, total - 1, 2), SECTOR).unwrap();
    file.write_all_at(&entries, 2 * SECTOR).unwrap();
    file.write_all_at(&entries, (total - 5) * SECTOR).unwrap();
    file.write_all_at(&header(total - 1, 1, total - 5), (total - 1) * SECTOR).unwrap();
}

/// The disk as the resize engine would report it.
fn snapshot(disk: &Path) -> Value {
    let (table, _) = Disk::open(disk).unwrap().tables().unwrap();
    let partitions: Vec<Value> = table
        .partitions
        .iter()
        .map(|p| {
            json!({"id": gpt::guid_text(&p.guid), "name": p.name, "bsdName": format!("disk0s{}", p.number),
                   "firstLBA": p.first, "lastLBA": p.last, "typeGUID": gpt::guid_text(&p.kind).to_uppercase()})
        })
        .collect();
    let ios = &table.partitions[0];
    json!({"layout": {"revision": format!("{:08x}", crc32(&table.entries)), "sectorSize": SECTOR, "lastUsableLBA": table.last_usable,
                      "partitions": partitions, "backingPartitionID": gpt::guid_text(&ios.guid),
                      "allocatedBytes": 20_000_000_000u64, "minimumContainerBytes": 22_000_000_000u64, "issue": null},
           "operation": null})
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(dir), Some(machine)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: drive DIR MACHINE [INDEX]");
        std::process::exit(2);
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let disk = dir.join("disk");
    if !disk.is_file() {
        make_disk(&disk);
    }
    let tools = std::env::var("E2FSPROGS").map(PathBuf::from).unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ios-installer/out/e2fsprogs-host"));
    let config = Config {
        cache: dir.join("cache"),
        disk: disk.clone(),
        tools,
        system: Some("Omarchy A".into()),
        index: args.get(3).cloned().unwrap_or_else(|| "https://github.com/OrchardLinux/omarchy-a/releases/latest/download/index.toml".into()),
        machine: machine.clone(),
        kind: if machine.starts_with("iPad") { "iPad" } else { "iPhone" }.into(),
        os: if machine.starts_with("iPad") { "iPadOS" } else { "iOS" }.into(),
        timezone: Some("Europe/Berlin".into()),
        allow_unsupported: std::env::var_os("ALLOW_UNSUPPORTED").is_some(),
    };
    let engine = Engine::start(config, Box::new(|effect| eprintln!("effect for the helper: {effect}")));
    let ask = |request: Value| {
        let answer = engine.request(&request);
        if let Some(error) = answer.get("error") {
            eprintln!("refused: {error}");
        }
        answer["ok"].clone()
    };
    let mut shown = String::new();
    let mut kept = false;
    loop {
        engine.storage(&snapshot(&disk));
        let view = ask(json!({"view": {}}));
        let line = json!([view["screen"], view["split"], view["account"], view["installing"], view["installed"], view["failed"]]).to_string();
        if line != shown {
            println!("{line}");
            shown = line;
        }
        match view["screen"].as_str() {
            Some("welcome") if view["welcome"]["checked"] == true => {
                ask(json!({"continue": {}}));
            }
            Some("split") if !kept => {
                ask(json!({"keep": {}}));
                kept = true;
            }
            Some("account") if view["account"]["download"] == "ready" => {
                ask(json!({"install": {"password": "correct horse", "pin": "123456"}}));
            }
            Some("installed") => break,
            Some("failed") | Some("unsupported") => std::process::exit(1),
            _ => {}
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

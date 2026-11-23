//! m1n1's boot manager over its proxy (orchard's m1n1 patches 0007, 0008,
//! 0016, 0017): storage up, the boot entries as m1n1 reads them from the
//! device's boot partition (docs/boot-contract.md), and starting one. The
//! host never reads the disk. Mirrors orchard's `transports/m1n1.py`.
use crate::m1n1::Proxy;
use crate::{Error, Kind};
use std::time::Duration;

const HEAPBLOCK_ALLOC: u64 = 0x600;
const HEAPBLOCK_SET_LIMIT: u64 = 0x604;
const STORAGE_INIT: u64 = 0xf08;
const BOOT_ENTRIES: u64 = 0x1500;
const BOOT_ENTRIES_FREE: u64 = 0x1501;
const BOOT_START: u64 = 0x1502;
/// The newest boot contract this host speaks. The launcher must start systems
/// installed by any older installer, so it reads every contract up to this
/// one: a new contract adds a case here, it never drops an old one.
pub const CONTRACT: u32 = 1;
/// Room left to m1n1's own heap above the block; the host's own after it.
const M1N1_HEAP: u64 = 128 << 20;
const HOST_HEAP: u64 = 16 << 20;
const TIMEOUT: Duration = Duration::from_secs(10);
/// Loading an entry (kernel, initrd) from storage takes a while.
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// One boot entry as m1n1 lists it: its id, its BLS keys and why it can't
/// boot (empty when it can).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub keys: Vec<(String, String)>,
    pub problems: Vec<String>,
}

impl Entry {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    pub fn title(&self) -> &str {
        self.get("title").unwrap_or(&self.id)
    }
    pub fn default(&self) -> bool {
        self.get("default") == Some("yes")
    }
}

fn failed(detail: impl Into<String>) -> Error {
    Error { action: "m1n1 boot manager", kind: Kind::Unexpected, detail: detail.into() }
}

/// The text of BOOT_ENTRIES: `contract N`, then `entry ID`, key lines and
/// `problem …` lines; an `error …` line is the reason none could be read.
pub fn parse(text: &str) -> Result<Vec<Entry>, Error> {
    let mut lines = text.lines();
    let contract = lines.next().and_then(|l| l.strip_prefix("contract ")).and_then(|n| n.trim().parse::<u32>().ok());
    match contract {
        Some(1..=CONTRACT) => {}
        Some(n) if n > CONTRACT => {
            return Err(failed(format!("This system needs a newer Orchard (boot contract {n}, this one reads up to {CONTRACT}).")))
        }
        _ => return Err(failed("the loader's boot entries have no boot contract")),
    }
    let mut entries: Vec<Entry> = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "error" => return Err(failed(value.to_owned())),
            "entry" => entries.push(Entry { id: value.to_owned(), keys: Vec::new(), problems: Vec::new() }),
            _ => {
                let Some(last) = entries.last_mut() else {
                    return Err(failed(format!("unexpected boot entry line {line:?}")));
                };
                if key == "problem" {
                    last.problems.push(value.to_owned());
                } else {
                    last.keys.push((key.to_owned(), value.to_owned()));
                }
            }
        }
    }
    Ok(entries)
}

/// The proxy with a host heap set up (as m1n1's proxyclient does).
pub struct BootManager {
    proxy: Proxy,
    heap: u64,
    top: u64,
}

impl BootManager {
    pub fn new(mut proxy: Proxy) -> Result<BootManager, Error> {
        let heap = proxy.call(HEAPBLOCK_ALLOC, &[0], TIMEOUT)? + M1N1_HEAP;
        proxy.call(HEAPBLOCK_SET_LIMIT, &[heap], TIMEOUT)?;
        Ok(BootManager { proxy, heap, top: heap + HOST_HEAP })
    }

    fn alloc(&mut self, align: u64, size: u64) -> Result<u64, Error> {
        let at = (self.heap + align - 1) & !(align - 1);
        if at + size > self.top {
            return Err(failed("the host heap in m1n1 is exhausted"));
        }
        self.heap = at + size;
        Ok(at)
    }

    fn string(&mut self, text: &str) -> Result<u64, Error> {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        let at = self.alloc(8, data.len() as u64)?;
        self.proxy.write(at, &data, TIMEOUT)?;
        Ok(at)
    }

    /// Brings the device's storage up; refused by a loader without storage support.
    pub fn storage_init(&mut self) -> Result<(), Error> {
        if self.proxy.call(STORAGE_INIT, &[], Duration::from_secs(30))? == 0 {
            return Err(failed("this loader has no storage support for the device"));
        }
        Ok(())
    }

    /// The boot entries on the device, in BLS order (needs storage_init).
    pub fn entries(&mut self) -> Result<Vec<Entry>, Error> {
        let out = self.alloc(8, 16)?;
        let failed_flag = self.proxy.call(BOOT_ENTRIES, &[out, 0], TIMEOUT)? as i64;
        let reply = self.proxy.read(out, 16, TIMEOUT)?;
        let addr = u64::from_le_bytes(reply[..8].try_into().unwrap());
        let size = u64::from_le_bytes(reply[8..].try_into().unwrap()) as usize;
        if failed_flag != 0 || addr == 0 {
            return Err(failed("m1n1 could not list the boot entries"));
        }
        let text = if size > 0 { self.proxy.read(addr, size, TIMEOUT)? } else { Vec::new() };
        let _ = self.proxy.call(BOOT_ENTRIES_FREE, &[out], TIMEOUT);
        parse(&String::from_utf8_lossy(&text))
    }

    /// Loads the entry from storage and boots it; m1n1 answers before the
    /// jump. On failure it stays in the proxy, the reason on its console.
    pub fn start(&mut self, id: &str) -> Result<(), Error> {
        let name = self.string(id)?;
        let _ = self.proxy.take_console();
        let failed_flag = self.proxy.call(BOOT_START, &[name, 0, 0], START_TIMEOUT)? as i64;
        if failed_flag != 0 {
            let console = String::from_utf8_lossy(&self.proxy.take_console()).into_owned();
            let reasons: Vec<&str> = console
                .lines()
                .filter_map(|l| l.strip_prefix("bls: ").or_else(|| l.strip_prefix("bootmgr: ")))
                .collect();
            let why = if reasons.is_empty() { "see m1n1's console".to_owned() } else { reasons[reasons.len().saturating_sub(2)..].join("; ") };
            return Err(failed(format!("m1n1 could not start the entry: {why}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_parse_with_keys_and_problems() {
        let text = "contract 1\nentry omarchy-a-1\ntitle Omarchy A\nversion 6.12.4\ndefault yes\n\nentry old\ntitle Old\nproblem kernel missing\n";
        let e = parse(text).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].title(), e[0].get("version"), e[0].default()), ("Omarchy A", Some("6.12.4"), true));
        assert_eq!(e[1].problems, vec!["kernel missing".to_string()]);
    }

    #[test]
    fn another_contract_or_an_error_line_is_refused() {
        assert!(parse("contract 2\n").is_err());
        assert!(parse("contract 1\nerror no boot partition\n").is_err());
        assert!(parse("contract 1\n").unwrap().is_empty());
    }

    #[test]
    fn a_newer_contract_asks_for_a_newer_orchard() {
        let e = parse("contract 99\nentry a\ntitle A\n").unwrap_err();
        assert!(e.detail.contains("needs a newer Orchard"), "{}", e.detail);
        assert!(parse("entry a\n").is_err());
        assert_eq!(parse("contract 1\nentry a\ntitle A\n").unwrap()[0].title(), "A");
    }
}

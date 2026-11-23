//! Orchard's tools (Pongo, m1n1, board data): always from orchard's newest
//! release, checked against its SHA256SUMS. The installer and the launcher
//! both take them from here. Starting never waits for the network: the last
//! set fetched (kept in the cache) is used right away while the newest is
//! looked up in the background, for the next start. Only the very first start
//! waits for it.
use crate::release::Receipt;
use orchard_cache::http::Http;
use orchard_cache::source::Source;
use orchard_cache::{Code, Error, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const LATEST: &str = "https://github.com/OrchardLinux/orchard/releases/latest/download";
const TIMEOUT: Duration = Duration::from_secs(20);
/// The last set fetched, for starting without the network.
const LAST: &str = "orchard-tools.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub pongo: Receipt,
    pub m1n1: Receipt,
    pub board_data: Receipt,
}

/// The tools to start with: the last set fetched, at once (refreshing them in
/// the background for the next start), or, the first time, the newest.
/// `fetch_file` downloads a refreshed set into the store ahead of time.
pub fn latest(cache: &Path, fetch_file: impl Fn(&Receipt) + Send + 'static) -> Result<Tools> {
    if let Some(known) = recall(cache) {
        let (cache, held) = (cache.to_owned(), known.clone());
        std::thread::spawn(move || {
            if let Ok(newest) = fetch() {
                if newest != held {
                    for r in [&newest.pongo, &newest.m1n1, &newest.board_data] {
                        fetch_file(r);
                    }
                }
                remember(&cache, &newest);
            }
        });
        return Ok(known);
    }
    let tools = fetch().map_err(|e| e.context("Orchard's tools (needs the internet once)"))?;
    remember(cache, &tools);
    Ok(tools)
}

fn fetch() -> Result<Tools> {
    let mut sums = Http::probe(&format!("{LATEST}/SHA256SUMS"), TIMEOUT)?;
    let size = sums.size() as usize;
    let text = String::from_utf8(sums.read_at(0, size)?).map_err(|_| Error::new(Code::Format, "SHA256SUMS is not UTF-8"))?;
    let pin = |name: &str| -> Result<Receipt> {
        let sha256 = text
            .lines()
            .find_map(|l| l.split_once("  ").filter(|(_, n)| n.trim_start_matches("./") == name).map(|(h, _)| h.to_owned()))
            .ok_or_else(|| Error::new(Code::NotFound, format!("orchard's release has no {name}")))?;
        let url = format!("{LATEST}/{name}");
        let bytes = Http::probe(&url, TIMEOUT)?.size();
        Ok(Receipt { url, bytes, sha256, expanded_bytes: None })
    };
    Ok(Tools { pongo: pin("pongo.bin")?, m1n1: pin("m1n1.bin")?, board_data: pin("board-data.tar.gz")? })
}

fn to_json(r: &Receipt) -> Value {
    json!({"url": r.url, "bytes": r.bytes, "sha256": r.sha256})
}

fn from_json(v: &Value) -> Option<Receipt> {
    Some(Receipt { url: v["url"].as_str()?.to_owned(), bytes: v["bytes"].as_u64()?, sha256: v["sha256"].as_str()?.to_owned(), expanded_bytes: None })
}

fn remember(cache: &Path, t: &Tools) {
    let v = json!({"pongo": to_json(&t.pongo), "m1n1": to_json(&t.m1n1), "board_data": to_json(&t.board_data)});
    let _ = std::fs::create_dir_all(cache);
    let _ = std::fs::write(cache.join(LAST), serde_json::to_vec_pretty(&v).expect("tools"));
}

fn recall(cache: &Path) -> Option<Tools> {
    let v: Value = serde_json::from_slice(&std::fs::read(cache.join(LAST)).ok()?).ok()?;
    Some(Tools { pongo: from_json(&v["pongo"])?, m1n1: from_json(&v["m1n1"])?, board_data: from_json(&v["board_data"])? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_set_is_recalled() {
        let dir = std::env::temp_dir().join(format!("orchard-tools-{}", std::process::id()));
        let r = |n: &str| Receipt { url: format!("{LATEST}/{n}"), bytes: 3, sha256: "ab".repeat(32), expanded_bytes: None };
        let tools = Tools { pongo: r("pongo.bin"), m1n1: r("m1n1.bin"), board_data: r("board-data.tar.gz") };
        assert_eq!(recall(&dir), None);
        remember(&dir, &tools);
        assert_eq!(recall(&dir), Some(tools));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The newest release has the three tools (network; run with --ignored).
    #[test]
    #[ignore]
    fn the_newest_release_has_them() {
        let t = fetch().unwrap();
        assert!(t.pongo.bytes > 0 && t.m1n1.bytes > 0 && t.board_data.bytes > 0);
    }
}

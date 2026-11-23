//! One companion connection held like the installer holds it (ping every 2 s,
//! log every 30 s) for N seconds; prints every failure with its time.
use orchard_runtime::companion::Client;
use std::time::{Duration, Instant};
fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let cache = std::env::temp_dir().join("orchard-installer-cache");
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(cache.join("overlays/current.json")).unwrap()).unwrap();
    let token = record["token"].as_str().unwrap().to_owned();
    let udid = orchard_usb::list().unwrap().into_iter().find_map(|d| d.identity.udid).expect("an iPad in normal mode");
    let socket = std::path::Path::new(orchard_runtime::usbmux::SOCKET);
    let start = Instant::now();
    let t = || format!("[{:6.1}s]", start.elapsed().as_secs_f32());
    let (mut pings, mut failures) = (0u32, 0u32);
    let mut client: Option<Client> = None;
    let mut last_log = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if client.is_none() {
            let c0 = Instant::now();
            match Client::connect(socket, &udid, &token) {
                Ok((c, _)) => { println!("{} connected in {:.1}s", t(), c0.elapsed().as_secs_f32()); client = Some(c); }
                Err(e) => { failures += 1; println!("{} connect failed after {:.1}s: {e}", t(), c0.elapsed().as_secs_f32()); std::thread::sleep(Duration::from_secs(2)); continue; }
            }
        }
        let c = client.as_mut().unwrap();
        let p0 = Instant::now();
        match c.ping() {
            Ok(()) => { pings += 1; if p0.elapsed() > Duration::from_millis(500) { println!("{} slow ping {:.1}s", t(), p0.elapsed().as_secs_f32()); } }
            Err(e) => { failures += 1; println!("{} ping failed after {:.1}s: {e}", t(), p0.elapsed().as_secs_f32()); client = None; continue; }
        }
        if last_log.elapsed() >= Duration::from_secs(30) {
            last_log = Instant::now();
            match c.log() { Ok(l) => println!("{} log ok ({} bytes)", t(), l.len()), Err(e) => { failures += 1; println!("{} log failed: {e}", t()); client = None; continue; } }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    println!("{} done: {pings} pings, {failures} failures", t());
}

//! Feasibility check for an automatic replug on macOS, outside the installer:
//! runs palera1n (-p: stop after Pongo) in a pseudo-terminal (live lines) on
//! an iPad in DFU mode, prints the iPad's USB state every half second, and
//! when palera1n sits after "Checkmate?" without "Found download mode device"
//! for 3 s, re-enumerates the hub the iPad was on (IOKit
//! USBDeviceReEnumerate on the hub: its ports are detected again, a software
//! replug), up to 3 times 5 s apart. The iPad itself is off USB then.
//! Usage: replug_probe <palera1n> <pongo.bin>
use nusb::MaybeFuture;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn apple() -> Vec<nusb::DeviceInfo> {
    nusb::list_devices().wait().map(|l| l.filter(|d| d.vendor_id() == 0x05ac).collect()).unwrap_or_default()
}

fn describe(d: &nusb::DeviceInfo) -> String {
    let mode = match d.product_id() {
        0x1227 => "DFU",
        0x1281 => "recovery",
        0x4141 => "pongo",
        _ => "other",
    };
    format!("{:04x} {mode} {:?}", d.product_id(), d.serial_number().map(|s| s.chars().take(40).collect::<String>()))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (palera1n, pongo) = (&args[1], &args[2]);
    let start = Instant::now();
    let t = || format!("[{:6.1}s]", start.elapsed().as_secs_f32());
    // `script` gives palera1n a terminal, so it prints each line at once.
    let mut child = Command::new("/usr/bin/script")
        .args(["-q", "/dev/null", palera1n.as_str(), "-p", "-k", pongo.as_str()])
        .env("RA1N_CLI", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("palera1n");
    let (tx, rx) = mpsc::channel::<String>();
    for pipe in [Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>, Box::new(child.stderr.take().unwrap())] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    let strip = |s: &str| {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if c == '\x1b' { esc = true; continue; }
            if esc { if c.is_ascii_alphabetic() { esc = false; } continue; }
            out.push(c);
        }
        out
    };
    let (mut checkmate, mut returned, mut resets, mut last_reset) = (None::<Instant>, false, 0, None::<Instant>);
    let mut last_usb = String::new();
    // The location of the hub the iPad hangs on, from its own (each port one nibble).
    let mut hub_loc: Option<u32> = None;
    // The iPad's own location (its last nibble is its port on that hub).
    let mut hub_port: u32 = 0;
    loop {
        while let Ok(line) = rx.try_recv() {
            let line = strip(&line);
            if line.contains("<Log>") || line.contains("<Error>") || line.contains("<Warning>") {
                println!("{} palera1n: {}", t(), line.split("<").nth(1).map(|s| format!("<{s}")).unwrap_or(line.clone()));
            }
            if line.contains("Checkmate?") { checkmate = Some(Instant::now()); }
            if line.contains("Found download mode device") { returned = true; }
        }
        if let Some(d) = apple().iter().find(|d| d.product_id() == 0x1227) {
            let loc = d.location_id();
            let mut parent = loc;
            for shift in (0..32).step_by(4) {
                if (loc >> shift) & 0xf != 0 {
                    parent = loc & !(0xf << shift);
                    break;
                }
            }
            if hub_loc != Some(parent) {
                println!("{} iPad at {loc:08x}, hub at {parent:08x}", t());
                hub_loc = Some(parent);
                // The port: the nibble that was cleared to get the hub.
                hub_port = (0..32).step_by(4).map(|sh| (loc >> sh) & 0xf).find(|n| *n != 0).unwrap_or(0);
            }
        }
        let usb: Vec<String> = apple().iter().map(describe).collect();
        let usb = if usb.is_empty() { "no Apple device".to_string() } else { usb.join(", ") };
        if usb != last_usb {
            println!("{} usb: {usb}", t());
            last_usb = usb;
        }
        let stalled = checkmate.is_some_and(|c| c.elapsed() >= Duration::from_secs(3)) && !returned;
        let due = last_reset.is_none_or(|r| r.elapsed() >= Duration::from_secs(5));
        if stalled && due && resets < 3 {
            resets += 1;
            last_reset = Some(Instant::now());
            // Power off the iPad's hub port and on again (uhubctl: the hub's
            // per-port power switching; for a USB 3 hub also the matching port).
            let port = (hub_port & 0xf).to_string();
            let hub = format!("0-{}", (hub_loc.unwrap_or(0) >> 20) & 0xf);
            println!("{} reset #{resets}: uhubctl -a cycle -l {hub} -p {port} -d 1", t());
            let mut cmd = Command::new("uhubctl").args(["-a", "cycle", "-l", &hub, "-p", &port, "-d", "1"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("uhubctl");
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                if let Ok(Some(st)) = cmd.try_wait() {
                    let out = cmd.wait_with_output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)).unwrap_or_default();
                    println!("{} reset #{resets}: uhubctl exited {st}\n{out}", t());
                    break;
                }
                if Instant::now() > deadline {
                    let _ = cmd.kill();
                    println!("{} reset #{resets}: uhubctl did not finish in 8 s (killed)", t());
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            std::thread::sleep(Duration::from_millis(300));
            while let Ok(line) = rx.try_recv() { println!("{} palera1n: {}", t(), strip(&line)); }
            println!("{} palera1n exited: {status}; resets: {resets}; returned by itself: {}", t(), returned && resets == 0);
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

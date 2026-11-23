//! Asks a running install system's receiver for its state (the read-only
//! `Info` request), with the time it took (development probe).
//!   cargo run -p orchard-usb --example install_info
use orchard_usb::{install::Session, System};
use std::time::Instant;

fn main() {
    let devices = orchard_usb::list().expect("listing USB devices");
    let Some(device) = devices.iter().find(|d| d.system == Some(System::Install)) else {
        eprintln!("no install system attached");
        std::process::exit(1);
    };
    println!("found {device:?}");
    let started = Instant::now();
    let mut session = match Session::open(device) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open failed after {:?}: {e:?}", started.elapsed());
            std::process::exit(1);
        }
    };
    println!("opened after {:?}", started.elapsed());
    session.on_event(|e| println!("event: {e:?}"));
    match session.info() {
        Ok(info) => println!("info after {:?}: {info:?}", started.elapsed()),
        Err(e) => println!("info failed after {:?}: {e:?}", started.elapsed()),
    }
}

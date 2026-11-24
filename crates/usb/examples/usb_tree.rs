//! Lists USB devices with their port chain and bus (read-only).
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]
use nusb::MaybeFuture;
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only: it reads USB location IDs");
}

#[cfg(target_os = "macos")]
fn main() {
    for d in nusb::list_devices().wait().unwrap() {
        println!("{:04x}:{:04x} ports {:?} loc {:08x} {:?} {:?}", d.vendor_id(), d.product_id(), d.port_chain(), d.location_id(), d.manufacturer_string(), d.product_string());
    }
}

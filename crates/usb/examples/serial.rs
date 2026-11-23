//! Lists the Apple USB devices attached, with their product ids and serial
//! numbers (development probe).
//!   cargo run -p orchard-usb --example serial
use nusb::MaybeFuture;

fn main() {
    for d in nusb::list_devices().wait().unwrap().filter(|d| d.vendor_id() == 0x05ac) {
        println!("{:04x} {:?}", d.product_id(), d.serial_number());
    }
}

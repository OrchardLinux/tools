//! Re-enumerates the attached iPad running iPadOS (a USB port reset, as a
//! replug would), to see whether that frees a stalled usbmux pipe.
//! Usage: usb_reset <udid prefix>
use nusb::MaybeFuture;
fn main() {
    let want = std::env::args().nth(1).expect("udid prefix");
    for info in nusb::list_devices().wait().unwrap() {
        if info.vendor_id() != 0x05ac { continue; }
        let serial = info.serial_number().unwrap_or_default().to_owned();
        if !serial.to_lowercase().starts_with(&want.to_lowercase()) { continue; }
        println!("device {:04x}:{:04x} serial {serial}", info.vendor_id(), info.product_id());
        match info.open().wait() {
            Ok(dev) => println!("reset: {:?}", dev.reset().wait()),
            Err(e) => println!("open failed: {e}"),
        }
    }
}

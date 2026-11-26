//! Times usbmuxd's answers: `usbmux_probe UDID PORT`.
use orchard_runtime::usbmux;
use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let socket = Path::new(usbmux::SOCKET);
    let t = Instant::now();
    match usbmux::devices(socket) {
        Ok(list) => eprintln!("{:.2}s devices: {} ({} on USB)", t.elapsed().as_secs_f64(), list.len(), list.iter().filter(|d| d.usb).count()),
        Err(e) => eprintln!("{:.2}s devices failed: {e}", t.elapsed().as_secs_f64()),
    }
    let t = Instant::now();
    match usbmux::connect(socket, &args[1], args[2].parse().unwrap()) {
        Ok(_) => eprintln!("{:.2}s connected", t.elapsed().as_secs_f64()),
        Err(e) => eprintln!("{:.2}s connect failed: {e}", t.elapsed().as_secs_f64()),
    }
}

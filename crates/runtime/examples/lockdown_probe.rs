//! Asks lockdown (read-only) who the attached iPad in normal mode is.
fn main() {
    for d in orchard_usb::list().unwrap() {
        if let Some(udid) = d.identity.udid.clone() {
            let t = std::time::Instant::now();
            let r = orchard_runtime::lockdown::ask(std::path::Path::new(orchard_runtime::usbmux::SOCKET), &udid);
            println!("{udid}: {:?} after {:.1}s", r.map(|a| format!("{a:?}")).map_err(|e| e.to_string()), t.elapsed().as_secs_f32());
        }
    }
}

//! Prints the interfaces of attached USB devices (read-only).
use nusb::MaybeFuture;
fn main() {
    for info in nusb::list_devices().wait().unwrap() {
        println!("{:04x}:{:04x} {:?} {:?}", info.vendor_id(), info.product_id(), info.manufacturer_string(), info.product_string());
        for i in info.interfaces() {
            println!("  listed interface {} class {:02x}/{:02x}/{:02x} {:?}", i.interface_number(), i.class(), i.subclass(), i.protocol(), i.interface_string());
        }
        if info.vendor_id() == 0x05ac || info.vendor_id() == 0x1d6b {
            match info.open().wait() {
                Ok(d) => {
                    for c in d.configurations() {
                        println!("  config {}", c.configuration_value());
                        for s in c.interface_alt_settings() {
                            let eps: Vec<String> = s.endpoints().map(|e| format!("{:02x}:{:?}", e.address(), e.transfer_type())).collect();
                            println!("    if {} alt {} class {:02x}/{:02x}/{:02x} eps {:?}", s.interface_number(), s.alternate_setting(), s.class(), s.subclass(), s.protocol(), eps);
                        }
                    }
                    println!("  active {:?}", d.active_configuration().map(|c| c.configuration_value()));
                }
                Err(e) => println!("  open error {e}"),
            }
        }
    }
}

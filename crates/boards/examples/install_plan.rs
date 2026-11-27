//! The install plan for a disk as the install system reports it: reads
//! `Inspect`'s answer on standard input and prints the plan `orchard
//! install` would send, for the largest target. For trying the installer
//! against the receiver's test mode without a device.
//! `install_plan IMAGE_JSON LAYER_JSON ENTRY_JSON < inspect.json`
use orchard_boards::install_plan::{choose, plan, Layout, Sizes};
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let read = |text: &str| -> Value { serde_json::from_str(text).unwrap() };
    let inspect: Value = serde_json::from_reader(std::io::stdin()).unwrap();
    let layout = Layout::from_inspect(&inspect).unwrap();
    let (image, layer, entry) = (read(&args[1]), read(&args[2]), read(&args[3]));
    let sizes = Sizes { image_expanded: image["expanded_bytes"].as_u64().unwrap(), layer_expanded: layer["expanded_bytes"].as_u64().unwrap() };
    let mut n = 0u32;
    let mut uuid = move || {
        n += 1;
        format!("{n:08x}-0000-4000-8000-00000000{n:04x}")
    };
    let made = plan(&layout, &choose(&layout).unwrap(), sizes, image, layer, entry, None, None, &mut uuid).unwrap();
    println!("{made}");
}

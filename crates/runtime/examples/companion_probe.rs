//! Read-only check of the companion on an attached iPad: hello, ping, layout.
use orchard_runtime::companion::Client;
fn main() {
    let cache = std::env::temp_dir().join("orchard-installer-cache");
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(cache.join("overlays/current.json")).unwrap()).unwrap();
    let token = record["token"].as_str().unwrap();
    println!("overlay {}", record["path"]);
    for d in orchard_usb::list().unwrap() {
        let Some(udid) = d.identity.udid.clone() else { continue };
        println!("device {:?} {udid}", d.mode);
        match Client::connect(std::path::Path::new(orchard_runtime::usbmux::SOCKET), &udid, token) {
            Ok((mut c, hello)) => {
                println!("hello {hello:?}");
                println!("ping {:?}", c.ping().map_err(|e| e.to_string()));
                if let Ok(raw) = c.snapshot_raw() {
                    let mut op = raw["operation"].clone();
                    if let Some(o) = op.as_object_mut() { o.remove("plan"); }
                    println!("operation (without plan): {op}");
                    println!("plan steps: {}", raw["operation"]["plan"]["steps"]);
                    println!("partitions now: {}", raw["layout"]["partitions"].as_array().map(|a| a.iter().map(|p| format!("{} {}..{}", p["name"], p["firstLBA"], p["lastLBA"])).collect::<Vec<_>>().join(", ")).unwrap_or_default());
                }
                match c.snapshot() {
                    Ok(s) => println!("layout {:?} operation {:?}", s.layout.flow_layout(), s.operation.map(|o| (o.status, o.message))),
                    Err(e) => println!("snapshot error {e}"),
                }
            }
            Err(e) => println!("connect error {e}"),
        }
    }
}

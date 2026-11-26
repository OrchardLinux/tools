//! Puts Resize APFS into a Dopamine IPA as `orchard2 resize --dopamine`
//! does, without signing: for comparing with resize-apfs's `embed.py`.
//! `dopamine_prepare DOPAMINE.ipa ACCESS_DIR OUT_DIR`
use orchard_runtime::access_dopamine::prepare;
use orchard_runtime::resize_app::Access;
use std::path::Path;

fn files(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, root, out);
        } else {
            out.push((path.strip_prefix(root).unwrap().to_string_lossy().into_owned(), std::fs::read(&path).unwrap()));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(&args[2]);
    let mut resources = Vec::new();
    files(&dir.join("Resources"), &dir.join("Resources"), &mut resources);
    let access = Access { info: std::fs::read(dir.join("Info.plist")).unwrap(), library: std::fs::read(dir.join("ResizeAccess.dylib")).unwrap(), resources };
    let prepared = prepare(std::fs::read(&args[1]).unwrap(), &access, Path::new(&args[3])).unwrap();
    // With a fourth argument, also the IPA a signing tool is given.
    if let Some(ipa) = args.get(4) {
        std::fs::write(ipa, prepared.ipa().unwrap()).unwrap();
    }
    println!("{}", prepared.app.display());
}

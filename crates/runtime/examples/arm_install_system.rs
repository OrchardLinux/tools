//! What `orchard install` sends a 32-bit board to start the install system,
//! made from a published release without a device: m1n1's 32-bit build with
//! the system behind it as iBoot's kernel image (the `ibss` route), and
//! Cascadia's bundle (the `autogo` route).
//! `arm_install_system INDEX_URL BOARD [DATA_DIR]`
use orchard_boards::catalog::Catalog;
use orchard_boards::data::loaders;
use orchard_boards::install_env;
use orchard_cache::store::Store;
use orchard_formats::{cascadia, m1n1};
use orchard_runtime::exec::Executor;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (index, board) = (&args[1], &args[2]);
    let cache = orchard_cache::store::default_dir().unwrap();
    let (events, _log) = std::sync::mpsc::channel();
    let executor = Executor::new(args.get(3).map(std::path::Path::new), &cache, events).with_index(index);
    let rel = executor.release_for(board).unwrap();
    println!("{} {} for {board}: 32-bit manifest: {}", rel.name, rel.version, executor.is_arm(board));
    let layer = std::fs::read(executor.download(&rel.platform_for(board).unwrap().layer).unwrap()).unwrap();
    let tree = executor.devicetree(board).unwrap();
    let boot = install_env::boot_files(&layer, &format!("{tree}.dtb")).unwrap();
    let tools = std::fs::read(executor.download(&rel.install_tools).unwrap()).unwrap();
    let add_on = install_env::add_on(&tools, &install_env::Device { ecid: 0x1234 }).unwrap();
    println!("kernel {} bytes, initrd {}, tree {}, add-on {}", boot.kernel.len(), boot.initrd.len(), boot.tree.len(), add_on.len());

    let store = Store::open(Some(&cache)).unwrap();
    let record = loaders::resolve(&store).unwrap();
    let macho = loaders::file(&store, &record, "m1n1", "m1n1-armv7.macho").unwrap();
    let behind = install_env::payload(&macho, None, &boot, &add_on).unwrap();
    let image = m1n1::kernel_image_arm(&behind).unwrap();
    println!("ibss route: m1n1 {} bytes, with the system {} bytes, as iBoot's kernel image {} bytes", macho.len(), behind.len(), image.len());

    let catalog = Catalog::open(&executor.data().unwrap()).unwrap();
    let profile = catalog.board(board).unwrap().cascadia().unwrap();
    let loader = std::fs::read(executor.download(&rel.cascadia[board.as_str()]).unwrap()).unwrap();
    let mut initramfs = boot.initrd.clone();
    initramfs.resize(initramfs.len().next_multiple_of(4), 0);
    initramfs.extend_from_slice(&add_on);
    let bundle = cascadia::bundle(&loader, &boot.kernel, &boot.tree, &initramfs, &profile, Some("F4KXXXXXXXXX")).unwrap();
    println!("autogo route: bundle {} bytes, loader {} bytes", bundle.image.len(), bundle.loader().len());
    // With a directory as fourth argument: the parts, for comparing with orchard's own packer.
    if let Some(out) = args.get(4) {
        let out = std::path::Path::new(out);
        for (name, data) in [("loader.bin", &loader), ("zImage", &boot.kernel), ("board.dtb", &boot.tree), ("initramfs", &initramfs), ("bundle.bin", &bundle.image)] {
            std::fs::write(out.join(name), data).unwrap();
        }
    }
}

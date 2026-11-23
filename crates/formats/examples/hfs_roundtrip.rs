//! Rebuilds the HFS+ images embedded in a file (palera1n's binpack and its
//! loader.dmg) with hfs_write and udif::write, and checks they read back
//! entry for entry. Usage: hfs_roundtrip <palera1n executable> <output dir>
//! Then check the output with `hdiutil verify` and `fsck_hfs -fn`.
use orchard_formats::hfs::{Entry, Volume};
use orchard_formats::{hfs_write, udif};
use std::path::Path;

fn rewrite(image: &[u8], label: &str, out: &Path) -> Vec<Entry> {
    let disk = udif::read(image).unwrap();
    let mut vol = Volume::open(disk.as_slice(), 0).unwrap();
    let name = vol.name().unwrap();
    let entries = vol.entries().unwrap();
    let h = &disk[1024..1536];
    let mut finder = [0u8; 32];
    finder.copy_from_slice(&h[80..112]);
    let opts = hfs_write::Options {
        name: name.clone(), block_size: 4096,
        created: u32::from_be_bytes(h[16..20].try_into().unwrap()),
        modified: u32::from_be_bytes(h[20..24].try_into().unwrap()),
        finder, min_size: disk.len() as u64, slack: 0,
    };
    let new_disk = hfs_write::write(&entries, &opts).unwrap();
    let new_image = udif::write(&new_disk, &udif::WriteOptions { name: "whole disk (Apple_HFSX : 0)", segment_id: [0x42; 16] }).unwrap();
    std::fs::write(out, &new_image).unwrap();
    let back = Volume::open(udif::read(&new_image).unwrap().as_slice(), 0).unwrap().entries().unwrap();
    assert_eq!(back, entries, "{label}: entries differ after the round trip");
    println!("{label}: '{name}' {} entries, {} → {} bytes image, disk {} → {}", entries.len(), image.len(), new_image.len(), disk.len(), new_disk.len());
    entries
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let exe = std::fs::read(&a[1]).unwrap();
    let out = Path::new(&a[2]);
    let images = udif::embedded(&exe);
    println!("{} embedded images", images.len());
    for (i, img) in images.iter().enumerate() {
        let disk = udif::read(img).unwrap();
        let Ok(mut vol) = Volume::open(disk.as_slice(), 0) else { println!("image {i}: not HFS"); continue };
        let Ok(loader) = vol.read("loader.dmg") else { println!("image {i}: no loader.dmg"); continue };
        let outer = rewrite(img, "outer", &out.join("outer.dmg"));
        rewrite(&loader, "loader.dmg", &out.join("loader.dmg"));
        let links = outer.iter().filter(|e| matches!(e.kind, orchard_formats::hfs::Kind::Symlink(_))).count();
        println!("symlinks {links}");
    }
}

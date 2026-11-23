//! Packed images compared with what orchard's Python assembler produced.
use orchard_formats::m1n1::{pack, Payload, LOGO_SIZE};
use sha2::{Digest, Sha256};

fn data(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/data/m1n1/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const BOOTARGS: &str = "console=tty0 rdinit=/init";

#[test]
fn one_device_tree() {
    let (a, kernel, initramfs) = (data("a.dtb"), data("kernel.bin"), data("initramfs.bin"));
    let payload = Payload {
        bootargs: Some(BOOTARGS),
        dtbs: &[&a],
        kernel: &kernel,
        initramfs: Some(&initramfs),
        ..Payload::default()
    };
    let image = pack(&data("loader.bin"), &payload).unwrap();
    assert_eq!(
        sha256(&image),
        "5aa0ff8d7cfe82d1b61f4c7dac23e150140d9049e1c7dac794761dff15945792"
    );
}

#[test]
fn two_device_trees_and_a_logo() {
    let mut logo = b"m1n1_logo_256128".to_vec();
    logo.extend((0..LOGO_SIZE - 16).map(|i| i as u8));
    let (a, b) = (data("a.dtb"), data("b.dtb"));
    let (kernel, initramfs) = (data("kernel.bin"), data("initramfs.bin"));
    let payload = Payload {
        logo: Some(&logo),
        bootargs: Some(BOOTARGS),
        dtbs: &[&b, &a],
        kernel: &kernel,
        initramfs: Some(&initramfs),
    };
    let image = pack(&data("loader.bin"), &payload).unwrap();
    assert_eq!(sha256(&image), MULTIPLE_LOGO);
}
const MULTIPLE_LOGO: &str = "74ba149d449f50242573fcf87a48f393ae8c71dce3786336ca653d4a917ff4db";

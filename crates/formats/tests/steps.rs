//! Recipe steps against orchard's Python: the inputs in `data/steps/` are
//! made by `make.py` there, which also prints what Python's extractors give
//! for them. The values here are those.
use orchard_formats::kernelcache::{self, Kernelcache};
use orchard_formats::z2fw::{self, Profile, N1};
use orchard_formats::{bluetool, sgx};
use sha2::{Digest, Sha256};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn input(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/steps")
        .join(name);
    std::fs::read(path).unwrap()
}

const BOARD: N1 = N1 {
    calibration_address: 0x1000_8000,
    calibration_size: 1024,
    prox_address: 0x1000_8600,
    prox_size: 128,
};

#[test]
fn the_n1_controllers_packets() {
    let mtprops = input("n1.mtprops");
    let profile = |name: &str| Profile::from_mtprops(&mtprops, name).unwrap();
    assert_eq!(
        sha256(&z2fw::from_z2_packet_n1(&profile("One,1"), &BOARD).unwrap()),
        "29fa23f469e51315bf9ea8c0508a407e2fa8f3a3e21bbf7a03d99e2d22417f1f"
    );
    assert_eq!(
        sha256(&z2fw::from_z2_packets_n1(&profile("Two,1"), &BOARD).unwrap()),
        "c15383c1d1ca93b289e5546b569e921d78b15f2e5a5c4d398d8da3a288522a56"
    );
    // What Python refuses: a firmware config, one packet where a list is
    // asked for and the other way round, ranges that are not whole words,
    // that overlap each other or the firmware, or that are too large.
    assert!(z2fw::from_z2_packet_n1(&profile("Config,1"), &BOARD).is_err());
    assert!(z2fw::from_z2_packets_n1(&profile("One,1"), &BOARD).is_err());
    assert!(z2fw::from_z2_packet_n1(&profile("Two,1"), &BOARD).is_err());
    let with = |change: fn(&mut N1)| {
        let mut board = BOARD;
        change(&mut board);
        z2fw::from_z2_packet_n1(&profile("One,1"), &board)
    };
    assert!(with(|b| b.prox_size = 130).is_err());
    assert!(with(|b| b.calibration_address += 2).is_err());
    assert!(with(|b| b.prox_address = b.calibration_address + 512).is_err());
    assert!(with(|b| b.calibration_address = 32).is_err());
    assert!(with(|b| b.prox_size = 0x40004).is_err());
    assert!(with(|b| b.prox_address = 0).is_err());
}

#[test]
fn a_regulatory_table_at_its_address() {
    let tool = input("bluetool.macho");
    let table = bluetool::regulatory(&tool, 0x1_0000_1080, 210).unwrap();
    assert_eq!(
        sha256(table),
        "12c1603dc94f87c1e00de392bc3491bf8243f4b1fa2c7f62e806c91b23d234c1"
    );
    // One command: 1 to 255 bytes, in one segment's file-backed bytes.
    assert!(bluetool::regulatory(&tool, 0x1_0000_1080, 0).is_err());
    assert!(bluetool::regulatory(&tool, 0x1_0000_1080, 256).is_err());
    assert!(bluetool::regulatory(&tool, 0x1_0000_11f0, 32).is_err());
    assert!(bluetool::regulatory(&tool, 0x1_0000_0ff0, 32).is_err());
    assert!(bluetool::regulatory(&tool, 0x2_0000_0000, 16).is_err());
    // An address at the end of the address space is refused, not added up.
    assert!(bluetool::regulatory(&tool, u64::MAX - 8, 16).is_err());
    assert!(bluetool::regulatory(&tool[..0x450], 0x1_0000_1080, 210).is_err());
}

#[test]
fn the_sgx543_container() {
    let kernel = input("kernel32.macho");
    assert_eq!(
        sha256(&sgx::extract(&kernel, "12H321").unwrap()),
        "e0fcf6e079f000dce2161c5644c69a683ccd1d2cca465deb76fae8033e8dd791"
    );
    // The build is part of the container, so another build is another file.
    assert_ne!(
        sha256(&sgx::extract(&kernel, "12H322").unwrap()),
        "e0fcf6e079f000dce2161c5644c69a683ccd1d2cca465deb76fae8033e8dd791"
    );
    assert!(sgx::extract(&kernel, "").is_err());
    assert!(sgx::extract(&kernel, "seventeen-letters").is_err());
    // A 64-bit kernel, and one without the driver.
    assert!(sgx::extract(&input("merged.kernel"), "12H321").is_err());
    let mut without = kernel.clone();
    let at = without
        .windows(12)
        .position(|w| w == b"SGXDriver543")
        .unwrap();
    without[at] = b'X';
    assert!(sgx::extract(&without, "12H321").is_err());
}

#[test]
fn smartio_of_a_kext_merged_into_the_kernel() {
    let kernel = input("merged.kernel");
    let cache = Kernelcache::new(&kernel).unwrap();
    let bundle = "com.apple.driver.AppleS5L8960XSmartIO";
    assert_eq!(
        sha256(kernelcache::smartio(&cache, bundle).unwrap()),
        "695fb69684bbdf7f0df73d0cbcf84b46104edfe6504fd3a1dcbd340f628f8d8a"
    );
    // The kext has no sections of its own to ask for.
    assert_eq!(cache.kext_or_merged(bundle).unwrap(), None);
    assert!(cache.kext(bundle).is_err());
    // A kext the kernel does not hold, and one the list does not know.
    assert!(kernelcache::smartio(&cache, "com.apple.driver.AppleT7000SmartIO").is_err());
    assert!(kernelcache::smartio(&cache, "com.apple.driver.Other").is_err());
    assert_eq!(kernelcache::SMARTIO_KEXTS.len(), 5);
}

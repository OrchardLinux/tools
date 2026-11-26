//! Host-only checks; no USB access.
use clap::Parser;

fn parses(args: &[&str]) -> bool {
    crate::args::Cli::try_parse_from([&["orchard2", "run"], args].concat()).is_ok()
}

#[test]
fn a_kernel_comes_with_its_device_tree_and_initramfs() {
    let linux = ["--kernel", "Image.gz", "--dtb", "board.dtb", "--initramfs", "initramfs.cpio.gz"];
    assert!(parses(&linux));
    for flag in ["--kernel", "--dtb", "--initramfs"] {
        let mut missing = linux.to_vec();
        let at = missing.iter().position(|arg| *arg == flag).unwrap();
        missing.drain(at..at + 2);
        assert!(!parses(&missing), "without {flag}");
    }
}

#[test]
fn without_a_kernel_m1n1_alone_is_started() {
    assert!(parses(&[]));
    assert!(parses(&["--ibss", "--m1n1", "m1n1.macho"]));
    assert!(!parses(&["--bootargs", "quiet"]));
    assert!(!parses(&["--proxy"]));
}

#[test]
fn the_boot_route_is_named_once() {
    assert!(parses(&["--boot", "checkra1n"]));
    assert!(parses(&["--palera1n"]));
    assert!(!parses(&["--ibss", "--palera1n"]));
    assert!(!parses(&["--ibss", "--boot", "ibss"]));
}

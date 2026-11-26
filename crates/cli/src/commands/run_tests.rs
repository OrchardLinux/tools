//! Host-only checks; no USB access.
use super::*;
use clap::Parser;

fn args(given: &[&str]) -> Args {
    match crate::args::Cli::try_parse_from([&["orchard2", "run"], given].concat()).map(|cli| cli.command) {
        Ok(crate::args::Command::Run(args)) => args,
        _ => panic!("{given:?} does not parse"),
    }
}

fn parses(given: &[&str]) -> bool {
    crate::args::Cli::try_parse_from([&["orchard2", "run"], given].concat()).is_ok()
}

#[test]
fn a_kernel_comes_with_its_device_tree_and_initramfs() {
    let linux = ["--kernel", "Image.gz", "--dtb", "a.dtb", "--dtb", "b.dtb", "--initramfs", "initramfs.cpio.gz"];
    let got = wanted(&args(&linux), None).unwrap().unwrap();
    assert_eq!(got.dtbs, [PathBuf::from("a.dtb"), PathBuf::from("b.dtb")]);
    for flag in ["--kernel", "--initramfs"] {
        let mut missing = linux.to_vec();
        let at = missing.iter().position(|arg| *arg == flag).unwrap();
        missing.drain(at..at + 2);
        assert!(wanted(&args(&missing), None).is_err(), "without {flag}");
    }
    assert!(wanted(&args(&["--kernel", "Image.gz", "--initramfs", "i"]), None).is_err());
}

#[test]
fn without_a_kernel_m1n1_alone_is_started() {
    assert_eq!(wanted(&args(&[]), None).unwrap(), None);
    assert_eq!(wanted(&args(&["--ibss", "--m1n1", "m1n1.macho"]), None).unwrap(), None);
    assert!(wanted(&args(&["--bootargs", "quiet"]), None).is_err());
    assert!(wanted(&args(&["--proxy"]), None).is_err());
}

#[test]
fn the_file_gives_what_the_options_leave_out() {
    let file = || -> Shortcut {
        toml::from_str("kernel = \"/k/Image.gz\"\ndtb = [\"/k/a.dtb\"]\ninitramfs = \"initramfs.cpio.gz\"\nbootargs = \"quiet\"\n").unwrap()
    };
    let at = Path::new("/p/out/apple-16k/run.toml");
    let got = wanted(&args(&["/p"]), Some((file(), at))).unwrap().unwrap();
    assert_eq!(got.kernel, PathBuf::from("/k/Image.gz"));
    // A relative path counts from the file.
    assert_eq!(got.initramfs, PathBuf::from("/p/out/apple-16k/initramfs.cpio.gz"));
    assert_eq!(got.bootargs.as_deref(), Some("quiet"));
    let got = wanted(&args(&["/p", "--bootargs", "debug", "--dtb", "mine.dtb"]), Some((file(), at))).unwrap().unwrap();
    assert_eq!(got.bootargs.as_deref(), Some("debug"));
    assert_eq!(got.dtbs, [PathBuf::from("mine.dtb")]);
    assert!(toml::from_str::<Shortcut>("kernle = \"x\"").is_err());
}

#[test]
fn the_boot_route_is_named_once() {
    assert!(parses(&["--boot", "checkra1n"]));
    assert!(parses(&["--palera1n"]));
    assert!(!parses(&["--ibss", "--palera1n"]));
    assert!(!parses(&["--ibss", "--boot", "ibss"]));
}

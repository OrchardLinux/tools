//! Host-only checks; no USB access.
use super::*;
use clap::Parser;

fn args(given: &[&str]) -> Args {
    match crate::args::Cli::try_parse_from([&["orchard", "run"], given].concat()).map(|cli| cli.command) {
        Ok(crate::args::Command::Run(args)) => args,
        _ => panic!("{given:?} does not parse"),
    }
}

fn parses(given: &[&str]) -> bool {
    crate::args::Cli::try_parse_from([&["orchard", "run"], given].concat()).is_ok()
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
fn bare_starts_m1n1_alone_and_takes_no_linux() {
    assert_eq!(wanted(&args(&["--bare"]), None).unwrap(), None);
    assert_eq!(wanted(&args(&["--bare", "--ibss", "--m1n1", "m1n1.macho"]), None).unwrap(), None);
    for linux in [&["--kernel", "k"][..], &["--bootargs", "quiet"], &["--proxy"], &["--no-build"]] {
        assert!(!parses(&[&["--bare"], linux].concat()), "{linux:?}");
    }
}

#[test]
fn a_project_is_the_default_and_takes_the_builds_options() {
    assert_eq!(args(&[]).build.directory(), Path::new("."));
    let given = args(&["payload", "--device", "j71b", "--no-build", "--checkra1n"]);
    assert_eq!(given.build.directory(), Path::new("payload"));
    assert!(given.no_build);
}

#[test]
fn a_kernel_is_a_tree_to_build_or_an_image_to_boot() {
    let dir = tempfile::tempdir().unwrap();
    let (tree, image) = (dir.path().join("linux"), dir.path().join("Image.gz"));
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(&image, b"").unwrap();
    let (tree, image) = (tree.to_str().unwrap(), image.to_str().unwrap());
    // A directory is the build's option: the project is built from it,
    // under the option's older name too.
    for flag in ["--kernel", "--kernel-tree"] {
        let given = args(&["payload", flag, tree]);
        assert_eq!(given.build.kernel_image(), None, "{flag}");
        assert_eq!(wanted(&given, None).unwrap(), None, "{flag}");
    }
    assert!(!parses(&["--kernel", tree, "--kernel-dir", "staged"]));
    // The build's file still names the kernel that is booted.
    let file: Shortcut = toml::from_str("kernel = \"/k/Image.gz\"\ndtb = [\"/k/a.dtb\"]\ninitramfs = \"i\"\n").unwrap();
    let got = wanted(&args(&["payload", "--kernel", tree]), Some((file, Path::new("/p/run.toml")))).unwrap().unwrap();
    assert_eq!(got.kernel, PathBuf::from("/k/Image.gz"));
    // A file is the kernel to boot, and needs what goes with it.
    let given = args(&["--kernel", image, "--dtb", "a.dtb", "--initramfs", "i"]);
    assert_eq!(given.build.kernel_image(), Some(Path::new(image)));
    assert_eq!(wanted(&given, None).unwrap().unwrap().kernel, PathBuf::from(image));
    assert!(wanted(&args(&["--kernel", image]), None).is_err());
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

#[test]
fn a_missing_directory_and_a_directory_without_a_project_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    assert!(nothing_to_run(&dir.path().join("missing"), true).is_err());
    let empty = nothing_to_run(dir.path(), false).unwrap_err();
    assert!(empty.message.starts_with("nothing to run in "), "{}", empty.message);
    assert!(nothing_to_run(dir.path(), true).is_ok());
    std::fs::write(dir.path().join("init"), "").unwrap();
    assert!(nothing_to_run(dir.path(), false).is_ok());
}

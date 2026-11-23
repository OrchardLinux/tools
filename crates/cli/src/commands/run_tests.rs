//! Host-only checks; no USB access.
#[test]
fn run_requires_explicit_linux_outputs() {
    use clap::Parser;
    let args = [
        "orchard2",
        "run",
        "--kernel",
        "Image.gz",
        "--dtb",
        "j71b.dtb",
        "--initramfs",
        "initramfs.cpio.gz",
    ];
    assert!(crate::args::Cli::try_parse_from(args).is_ok());
    for flag in ["--kernel", "--dtb", "--initramfs"] {
        let mut missing = args.to_vec();
        let at = missing.iter().position(|arg| *arg == flag).unwrap();
        missing.drain(at..at + 2);
        assert!(crate::args::Cli::try_parse_from(missing).is_err());
    }
    assert!(crate::args::Cli::try_parse_from(["orchard2", "run", "hardware-check"]).is_err());
}

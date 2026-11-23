//! The install environment: the one image Pongo starts so the host can
//! install a system over USB. m1n1, with the platform layer's kernel, the
//! board's device tree and an initramfs: the layer's initrd plus an add-on
//! with the release's install tools and what belongs to this one device.
//!
//! Everything the device runs comes from the release: the kernel, initrd and
//! device tree from the platform layer, and the install hook (`run`), its
//! DHCP configuration and the static programs from `install-tools.tar.gz`.
//! The add-on only adds the device's ECID. The host talks to the device over
//! the install environment's raw USB channel, so it needs no keys.
//! Mirrors orchard's `install_direct.py` (`layer_boot_files`, `add_on`,
//! `payload`).
use crate::pack::{self, Linux};
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::cpio::{self, Node};
use orchard_formats::tar;
use std::io::Read;

/// The kernel command line of the install environment.
pub const BOOT_ARGS: &str = "fbcon=map:9 vt.global_cursor_default=0";

/// What `install-tools.tar.gz` must hold, and the mode each gets on the
/// device (orchard's `install_direct.add_on`: the start script and DHCP
/// setup, PROGRAMS, the USB RECEIVER and what it runs to FINISH an install).
/// The graphical installer installs only over the receiver, so it and its
/// finishing files are required here (orchard's Python falls back to its
/// checkout's copies; this has none).
const TOOLS: [(&str, u32); 9] = [
    ("run", 0o644),
    ("udhcpd.conf", 0o644),
    ("orchard-write", 0o755),
    ("dropbear", 0o755),
    ("resize2fs", 0o755),
    ("tune2fs", 0o755),
    ("orchard-install-usb", 0o755),
    ("install_device.py", 0o644),
    ("boot-entries", 0o644),
];

/// Unpacked archives larger than this are refused. Layers are about 26 MB.
const MAX_UNPACKED: u64 = 512 << 20;

/// The files the install environment boots, out of the platform layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootFiles {
    pub kernel: Vec<u8>,
    pub initrd: Vec<u8>,
    pub tree: Vec<u8>,
}

/// What the add-on carries for this one device.
#[derive(Debug, Clone, Copy)]
pub struct Device {
    pub ecid: u64,
}

fn gunzip(gz: &[u8], what: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .take(MAX_UNPACKED + 1)
        .read_to_end(&mut out)
        .map_err(|e| Error::new(Code::Format, format!("{what} is not a gzip file: {e}")))?;
    if out.len() as u64 > MAX_UNPACKED {
        return Err(Error::new(Code::Format, format!("{what} unpacks to more than {} MB", MAX_UNPACKED >> 20)));
    }
    Ok(out)
}

/// The kernel, initrd and the board's device tree from a platform layer
/// (`platform-*.tar.gz`): `usr/lib/modules/RELEASE/{vmlinuz,initrd,dtb/DTB}`.
pub fn boot_files(layer: &[u8], devicetree: &str) -> Result<BootFiles> {
    let tar = gunzip(layer, "the platform layer")?;
    let archive = tar::read(&tar).context("the platform layer")?;
    let (mut kernel, mut initrd, mut tree) = (None, None, None);
    for file in &archive.files {
        let name = file.path.trim_start_matches("./");
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() < 5 || parts[..3] != ["usr", "lib", "modules"] {
            continue;
        }
        if name.ends_with("/vmlinuz") {
            kernel = Some(file.data.to_vec());
        } else if name.ends_with("/initrd") {
            initrd = Some(file.data.to_vec());
        } else if name.ends_with(&format!("/dtb/{devicetree}")) {
            tree = Some(file.data.to_vec());
        }
    }
    let missing: Vec<&str> = [("kernel", kernel.is_none()), ("initrd", initrd.is_none()), (devicetree, tree.is_none())]
        .into_iter()
        .filter_map(|(n, m)| m.then_some(n))
        .collect();
    if !missing.is_empty() {
        return Err(Error::new(Code::NotFound, format!("the platform layer lacks {}", missing.join(", "))));
    }
    Ok(BootFiles { kernel: kernel.unwrap(), initrd: initrd.unwrap(), tree: tree.unwrap() })
}

/// The add-on initramfs: the install tools under `/install`, with this
/// device's ECID beside them.
pub fn add_on(install_tools: &[u8], device: &Device) -> Result<Vec<u8>> {
    let tar = gunzip(install_tools, "install-tools.tar.gz")?;
    let archive = tar::read(&tar).context("install-tools.tar.gz")?;
    let find = |name: &str| archive.files.iter().find(|f| f.path.trim_start_matches("./") == name).map(|f| f.data);
    if find("run").is_none() {
        return Err(Error::new(
            Code::NotFound,
            "this release predates the graphical installer: its install tools have no install hook",
        ));
    }
    let mut paths = Vec::new();
    for (name, mode) in TOOLS {
        let data = find(name).ok_or_else(|| {
            let what = if name == "orchard-install-usb" { " (the USB receiver the graphical installer installs through)" } else { "" };
            Error::new(Code::NotFound, format!("install-tools.tar.gz lacks {name}{what}"))
        })?;
        paths.push((format!("install/{name}"), mode, data));
    }
    let ecid = format!("{:x}\n", device.ecid);
    // Dropbear looks root up: every Orchard boot keeps SSH and serial for
    // debugging, the install system too (no keys from here; it starts in the
    // background with a generated host key and nothing waits for it).
    let mut nodes = vec![
        Node::Directory { path: "install", mode: 0o755 },
        Node::Directory { path: "root", mode: 0o700 },
        Node::Directory { path: "etc", mode: 0o755 },
        Node::File { path: "etc/passwd", mode: 0o644, data: b"root:x:0:0:root:/root:/bin/sh\n" },
    ];
    nodes.extend(paths.iter().map(|(p, mode, data)| Node::File { path: p, mode: *mode, data }));
    nodes.push(Node::File { path: "install/ecid", mode: 0o644, data: ecid.as_bytes() });
    Ok(cpio::write_nodes(&nodes)?)
}

/// The device's peripheral firmware as the install takes it (orchard's
/// "private mixin", `firmware.pack_mixin`): a gzipped tar of
/// `lib/firmware/…` files, mode 0600, and `mixin.json` naming each with its
/// size and SHA-256. `files` are (destination, bytes); destinations must be
/// plain paths below `lib/firmware/`.
pub fn firmware_mixin(profile: &str, files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let mut listed = serde_json::Map::new();
    for (dest, data) in files {
        let plain = dest.starts_with("lib/firmware/")
            && !dest.contains('\\')
            && dest.split('/').all(|p| !p.is_empty() && p != "." && p != "..");
        if !plain {
            return Err(Error::new(Code::Format, format!("not a firmware data path: {dest}")));
        }
        let sha: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        listed.insert(dest.clone(), serde_json::json!({"bytes": data.len(), "sha256": sha}));
    }
    // Sorted keys, as Python's json.dumps(sort_keys=True).
    let metadata = serde_json::json!({"files": listed, "profile": profile, "schema": 1}).to_string();
    let mut entries: Vec<(&str, &[u8])> = files.iter().map(|(d, b)| (d.as_str(), b.as_slice())).collect();
    entries.push(("mixin.json", metadata.as_bytes()));
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let tar_entries: Vec<tar::Entry> = entries.iter().map(|(p, d)| tar::Entry { path: p, mode: 0o600, data: d }).collect();
    let tarball = tar::write(&tar_entries)?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tarball)?;
    Ok(gz.finish()?)
}

/// The image Pongo starts: m1n1 with the logo, the kernel command line, the
/// board's device tree, the kernel and the initrd with the add-on behind it.
pub fn payload(m1n1: &[u8], logo: Option<&[u8]>, boot: &BootFiles, add_on: &[u8]) -> Result<Vec<u8>> {
    let mut initramfs = boot.initrd.clone();
    initramfs.resize(initramfs.len().next_multiple_of(4), 0);
    initramfs.extend_from_slice(add_on);
    let dtbs = [boot.tree.as_slice()];
    let linux = Linux { kernel: &boot.kernel, dtbs: &dtbs, initramfs: Some(&initramfs), bootargs: Some(BOOT_ARGS), logo };
    pack::m1n1(m1n1, Some(&linux))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A gzipped ustar archive of `files`, enough for `formats::tar::read`.
    fn targz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, data) in files {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..107].copy_from_slice(b"0000755");
            h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
            h[136..147].copy_from_slice(b"00000000000");
            h[156] = b'0';
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
            tar.extend_from_slice(&h);
            tar.extend_from_slice(data);
            tar.resize(tar.len().next_multiple_of(512), 0);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn tools(with_run: bool) -> Vec<u8> {
        let mut files: Vec<(&str, &[u8])> =
            vec![
                ("udhcpd.conf", b"start 172.16.42.2\n"),
                ("orchard-write", b"W"),
                ("dropbear", b"D"),
                ("resize2fs", b"R"),
                ("tune2fs", b"T"),
                ("orchard-install-usb", b"U"),
                ("install_device.py", b"# finish\n"),
                ("boot-entries", b"# entries\n"),
            ];
        if with_run {
            files.push(("run", b"# install hook\n"));
        }
        targz(&files)
    }

    fn device() -> Device {
        Device { ecid: 0x0011_2233_4455_6677 }
    }

    #[test]
    fn boot_files_come_from_the_layers_modules_directory() {
        let layer = targz(&[
            ("usr/lib/modules/7.3.0/vmlinuz", b"KERNEL"),
            ("usr/lib/modules/7.3.0/initrd", b"INITRD"),
            ("usr/lib/modules/7.3.0/dtb/j71b.dtb", b"TREE"),
            ("usr/lib/modules/7.3.0/dtb/j72b.dtb", b"OTHER"),
        ]);
        let files = boot_files(&layer, "j71b.dtb").unwrap();
        assert_eq!((files.kernel.as_slice(), files.initrd.as_slice(), files.tree.as_slice()), (&b"KERNEL"[..], &b"INITRD"[..], &b"TREE"[..]));
        let err = boot_files(&layer, "n71.dtb").unwrap_err().to_string();
        assert!(err.contains("n71.dtb"), "{err}");
    }

    #[test]
    fn add_on_carries_the_tools_and_this_devices_data() {
        let cpio = add_on(&tools(true), &device()).unwrap();
        let text = String::from_utf8_lossy(&cpio);
        for name in [
            "install/run",
            "install/udhcpd.conf",
            "install/orchard-write",
            "install/dropbear",
            "install/orchard-install-usb",
            "install/install_device.py",
            "install/boot-entries",
            "install/ecid",
        ] {
            assert!(text.contains(name), "{name}");
        }
        assert!(text.contains("etc/passwd") && text.contains("root:x:0:0:root:/root:/bin/sh\n"), "dropbear looks root up");
        for name in ["authorized_keys", "host_key"] {
            assert!(!text.contains(name), "no keys from the host: {name}");
        }
        assert!(text.contains("11223344556677\n"), "the ECID without leading zeros, as the hook reads it");
    }

    #[test]
    fn an_older_release_without_the_install_hook_is_refused() {
        let err = add_on(&tools(false), &device()).unwrap_err().to_string();
        assert!(err.contains("predates the graphical installer"), "{err}");
    }

    #[test]
    fn firmware_mixin_lists_every_file_and_refuses_other_paths() {
        let files = vec![("lib/firmware/brcm/a.bin".to_string(), b"AA".to_vec())];
        let gz = firmware_mixin("j71b", &files).unwrap();
        let tar = gunzip(&gz, "mixin").unwrap();
        let archive = tar::read(&tar).unwrap();
        let names: Vec<&str> = archive.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["lib/firmware/brcm/a.bin", "mixin.json"]);
        let meta: serde_json::Value = serde_json::from_slice(archive.files[1].data).unwrap();
        assert_eq!(meta["files"]["lib/firmware/brcm/a.bin"]["bytes"], 2);
        assert_eq!(meta["profile"], "j71b");
        assert!(firmware_mixin("j71b", &[("etc/passwd".into(), vec![])]).is_err());
        assert!(firmware_mixin("j71b", &[("lib/firmware/../x".into(), vec![])]).is_err());
    }

    #[test]
    fn payload_puts_the_add_on_behind_the_aligned_initrd() {
        // The smallest device tree header the packer accepts.
        let mut tree = vec![0xd0, 0x0d, 0xfe, 0xed];
        tree.extend_from_slice(&40u32.to_be_bytes());
        tree.resize(40, 0);
        let boot = BootFiles { kernel: b"KERNEL".to_vec(), initrd: b"INI".to_vec(), tree };
        let image = payload(b"M1N1", None, &boot, b"ADDON").unwrap();
        let tag = b"m1n1_initramfs";
        let at = image.windows(tag.len()).position(|w| w == tag).expect("initramfs tag") + tag.len();
        let len = u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(&image[at + 4..at + 4 + len], b"INI\0ADDON");
        assert!(String::from_utf8_lossy(&image).contains(BOOT_ARGS));
    }
}

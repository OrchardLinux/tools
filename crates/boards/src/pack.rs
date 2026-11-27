//! Packing what a loader is sent: work that needs no device, from the
//! board, the device's serial number and the files a chain was built into.
use crate::boot_firmware::Images;
use crate::catalog::{Board, Chip};
use orchard_cache::{Code, Context, Error, Result};
use orchard_formats::{cascadia, kernelcache, m1n1};

/// What a file given to `load` is, which decides the loader it goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// A plain image, such as m1n1's binary: Pongo starts it as it is.
    Image,
    /// m1n1's Mach-O build, which a patched iBoot starts as a restore kernel.
    MachO,
    /// Cascadia's loader, which starts the Linux it is bundled with.
    Cascadia,
}

impl Form {
    pub fn of(file: &[u8]) -> Form {
        if cascadia::is_loader(file) {
            Form::Cascadia
        } else if m1n1::is_macho(file) {
            Form::MachO
        } else {
            Form::Image
        }
    }

    /// The file as a loader's input is named in messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Form::Image => "a plain image",
            Form::MachO => "a Mach-O m1n1",
            Form::Cascadia => "Cascadia's loader",
        }
    }
}

/// The Linux a loader is to start, as the caller has it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Linux<'a> {
    pub kernel: &'a [u8],
    pub dtbs: &'a [&'a [u8]],
    pub initramfs: Option<&'a [u8]>,
    pub bootargs: Option<&'a str>,
    pub logo: Option<&'a [u8]>,
}

/// m1n1, with what it is to boot behind it where there is something.
pub fn m1n1(m1n1: &[u8], linux: Option<&Linux>) -> Result<Vec<u8>> {
    let Some(linux) = linux else {
        return Ok(m1n1.to_vec());
    };
    let payload = m1n1::Payload {
        logo: linux.logo,
        bootargs: linux.bootargs,
        dtbs: linux.dtbs,
        kernel: linux.kernel,
        initramfs: linux.initramfs,
    };
    m1n1::pack(m1n1, &payload).context("packing the image")
}

/// Whether the chip loads an iBEC after the iBSS. iBoot on these chips
/// also starts a kernel through the monitor stub.
pub fn two_stage(chip: &Chip) -> bool {
    chip.stages.iter().any(|stage| stage == "iBEC")
}

/// One of a chain's built files, by the name `fw build` gives it.
pub fn file<'a>(images: &'a Images, name: &str) -> Result<&'a [u8]> {
    let found = images.iter().find(|(n, _)| n == name);
    found.map(|(_, bytes)| bytes.as_slice()).ok_or_else(|| {
        Error::new(
            Code::NotFound,
            format!("the chain's boot firmware has no {name}"),
        )
    })
}

/// The files a patched iBoot is sent to start m1n1 as if it were a restore
/// kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    pub devicetree: Vec<u8>,
    pub trustcache: Vec<u8>,
    pub kernelcache: Vec<u8>,
}

/// From the chain's built files (`devicetree.img4`, `trustcache.img4` and
/// the manifest they are signed with) and m1n1's Mach-O; the monitor stub
/// goes in on the chips that load an iBEC.
pub fn restore(chip: &Chip, images: &Images, macho: &[u8], stub: &[u8]) -> Result<Restore> {
    let stub = two_stage(chip).then_some(stub);
    Ok(Restore {
        devicetree: file(images, "devicetree.img4")?.to_vec(),
        trustcache: file(images, "trustcache.img4")?.to_vec(),
        kernelcache: kernelcache::restore(macho, stub, file(images, "im4m.der")?)
            .context("wrapping m1n1 as a kernelcache")?,
    })
}

/// The bundle Cascadia's loader starts Linux from on `board`: `loader` as
/// built, the zImage, the board's one device tree and the initramfs, at the
/// addresses the board's boot data gives, with the device's serial number
/// in the tree where it is known.
pub fn bundle(
    board: &Board,
    loader: &[u8],
    linux: &Linux,
    serial: Option<&str>,
) -> Result<cascadia::Bundle> {
    let invalid = |message: &str| Err(Error::new(Code::InvalidInput, message));
    let [dtb] = linux.dtbs else {
        return invalid("Cascadia's loader takes one device tree, the board's");
    };
    let Some(initramfs) = linux.initramfs else {
        return invalid("Cascadia's loader takes an initramfs: give --initramfs");
    };
    if linux.bootargs.is_some() || linux.logo.is_some() {
        return invalid(
            "Cascadia's loader takes no --bootargs or --logo: the kernel's arguments are the device tree's",
        );
    }
    let profile = board.cascadia()?;
    cascadia::bundle(loader, linux.kernel, dtb, initramfs, &profile, serial)
        .context("packing the bundle")
}

/// The digest of a chain's built files, by which a device prepared with
/// them is told from one prepared with others.
pub fn digest(images: &Images) -> String {
    let mut text = String::new();
    for (name, bytes) in images {
        text.push_str(name);
        text.push(' ');
        text.push_str(&orchard_cache::store::sha256(bytes));
        text.push('\n');
    }
    orchard_cache::store::sha256(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard_formats::der::{self, IA5_STRING, SEQUENCE};

    fn chip(stages: &[&str]) -> Chip {
        Chip {
            cpid: 0,
            name: "TEST".into(),
            soc: "soc".into(),
            architecture: "arm64".into(),
            stages: stages.iter().map(|s| (*s).into()).collect(),
            image_format: "im4p".into(),
            page_size: 16384,
            patcher: String::new(),
            entry: String::new(),
        }
    }

    fn images() -> Images {
        let im4m = der::write(SEQUENCE, &der::write(IA5_STRING, b"IM4M"));
        vec![
            ("devicetree.img4".into(), b"device tree".to_vec()),
            ("trustcache.img4".into(), b"trust".to_vec()),
            ("im4m.der".into(), im4m),
        ]
    }

    #[test]
    fn a_file_is_told_by_its_start() {
        assert_eq!(Form::of(b"plain"), Form::Image);
        let macho = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 1, 9];
        assert_eq!(Form::of(&macho), Form::MachO);
    }

    #[test]
    fn the_stub_rides_along_only_where_the_chip_loads_an_ibec() {
        let one = restore(&chip(&["iBSS"]), &images(), &[7; 300], b"stub").unwrap();
        let two = restore(&chip(&["iBSS", "iBEC"]), &images(), &[7; 300], b"stub").unwrap();
        assert_eq!(one.devicetree, b"device tree");
        assert_eq!(two.trustcache, b"trust");
        assert!(two.kernelcache.len() > one.kernelcache.len());
        let mut lacking = images();
        lacking.retain(|(name, _)| name != "trustcache.img4");
        let e = restore(&chip(&["iBSS"]), &lacking, &[7; 300], b"").unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        assert_eq!(
            e.message,
            "the chain's boot firmware has no trustcache.img4"
        );
    }

    #[test]
    fn a_digest_changes_with_any_file() {
        let mut other = images();
        other[1].1 = b"other trust".to_vec();
        assert_ne!(digest(&images()), digest(&other));
        assert_eq!(digest(&images()), digest(&images()));
    }

    #[test]
    fn an_image_without_linux_is_the_file() {
        assert_eq!(m1n1(b"m1n1", None).unwrap(), b"m1n1");
        let linux = Linux {
            kernel: b"kernel",
            ..Linux::default()
        };
        assert!(m1n1(b"m1n1", Some(&linux)).is_err());
    }
}

//! GPU firmware from real kernelcaches, compared with what orchard's Python
//! extractor (`orchard.apple.agx`) produced from the same files. Apple's
//! kernelcaches are not in the tree: point `ORCHARD_TEST_KERNELCACHES` at
//! decoded kernel Mach-Os, separated by `:`, to run these. Each is checked
//! against the row of the table below that its SHA-256 names; others are
//! an error.
use orchard_formats::kernelcache::{self, Kernelcache};
use sha2::{Digest, Sha256};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A decoded kernel and what Python extracted from it.
struct Known {
    kernel: &'static str,
    /// The LDR candidates, in kernel order.
    g4p_ldrs: &'static [&'static str],
    g4p_borders: &'static [&'static str],
    g5p: Option<&'static str>,
}

const G3_LDR: &str = "44816730098c99124190c8e4933ddbac6b1b50eb86555e886da567a24dc0eb88";
const G3_BORDER: &str = "785c7290e93bab010d5676ddedbdbae27147bf1997106fe532b05889536fd024";

const BORDER: &str = "233ffc1d7db27d9f16db17e23a9ce33082b5a7b72061af3da510bca3c94aff3e";

const KNOWN: &[Known] = &[
    // iPod touch 6 (n102ap), iOS 12.4 (16G77)
    Known {
        kernel: "1d996c85affa1f37a07c6454dd5d3a383d4855206d113c9599200d79b6467ff8",
        g4p_ldrs: &["0b60def67290391b4c6aa913d9a78a8105d6d9b03738175b8e5732689db1e5ba"],
        g4p_borders: &[BORDER],
        g5p: None,
    },
    // iPod touch 6 (n102ap), iOS 12.5.8 (16H88)
    Known {
        kernel: "605d1dfd5175afe6e823c7a80d47c30d72ac29f57ac9b4d3dcd0e4416e0948d9",
        g4p_ldrs: &["5f99a8ab88ddd34f8f32c11adb21f6b2410bece3d23a9aee560ca80fe8e45f55"],
        g4p_borders: &[BORDER],
        g5p: None,
    },
    // iPad Air (j71ap) and iPad mini 2 (j85ap), iOS 12.5.8 (16H88): the A7's
    // one LDR is AGXFirmwareKextG3's (agx_g3_ldrs()), and its one border
    // table AGXG3's _noastc one. The A7 recipes pin both from the J85's.
    Known {
        kernel: "17d4a27096a30a063392160cc8fd2a73d5c6ae9ee4c1880ebe7e8eb84e8ccdac",
        g4p_ldrs: &[G3_LDR],
        g4p_borders: &[G3_BORDER],
        g5p: None,
    },
    Known {
        kernel: "d173a651d02329a02d2f23c2859e1e54308a5e94da86ba729777efeb3503da08",
        g4p_ldrs: &[G3_LDR],
        g4p_borders: &[G3_BORDER],
        g5p: None,
    },
    // iPad 5 (iPad6,11), iPadOS 16.7.10 (20H350): its one LDR is the G5P
    // firmware, walked to the end of its last block.
    Known {
        kernel: "c6b95fcf0efe52ee9258454d01d14adc9b0cb922d94c54a04a4ce11d1ae81e0a",
        g4p_ldrs: &["abb90d2ea623d15687469f6d7ca2c51c12353d37ce8e429a138dc7cfc5256d8d"],
        g4p_borders: &[],
        g5p: Some(kernelcache::AGX_G5P_SHA256),
    },
];

fn hashes(candidates: Vec<&[u8]>) -> Vec<String> {
    candidates.into_iter().map(sha256).collect()
}

#[test]
fn gpu_firmware_matches_python() {
    let Ok(paths) = std::env::var("ORCHARD_TEST_KERNELCACHES") else {
        eprintln!("ORCHARD_TEST_KERNELCACHES is not set; nothing to compare");
        return;
    };
    for path in paths.split(':').filter(|p| !p.is_empty()) {
        let kernel = std::fs::read(path).unwrap();
        let digest = sha256(&kernel);
        let known = KNOWN
            .iter()
            .find(|k| k.kernel == digest)
            .unwrap_or_else(|| panic!("{path}: no Python result for kernel {digest}"));
        assert_eq!(
            hashes(kernelcache::agx_g4p_ldrs(&kernel)),
            known.g4p_ldrs,
            "{path}"
        );
        assert_eq!(
            hashes(kernelcache::agx_g4p_borders(&kernel)),
            known.g4p_borders,
            "{path}"
        );
        assert_eq!(
            hashes(kernelcache::agx_g3_ldrs(&kernel)),
            known.g4p_ldrs,
            "{path}"
        );
        let g5p = kernelcache::agx_g5p(&Kernelcache::new(&kernel).unwrap());
        assert_eq!(g5p.ok().map(sha256).as_deref(), known.g5p, "{path}");
    }
}

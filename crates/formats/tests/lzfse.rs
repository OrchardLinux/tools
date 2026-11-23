use orchard_formats::lzfse;
use sha2::{Digest, Sha256};

fn check(name: &str, size: usize, sha256: &str) {
    let path = format!(
        "{}/tests/data/lzfse/{name}.lzfse",
        env!("CARGO_MANIFEST_DIR")
    );
    let input = std::fs::read(path).unwrap();
    let out = lzfse::decode(&input, size).unwrap();
    let digest: String = Sha256::digest(&out)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(digest, sha256, "{name}");
    assert!(
        lzfse::decode(&input, size - 1).is_err(),
        "{name} with a smaller size"
    );
    assert_eq!(lzfse::decode_limited(&input, size).unwrap(), out);
}

#[test]
fn lzvn_block() {
    check(
        "lzvn",
        900,
        "8ba29c4adcd46ace6abf44e519e74ecb540b1c5dbea5a959e2f85a89bac870eb",
    );
}

#[test]
fn fse_v1_and_v2_blocks() {
    let same = "43323abfd56b2f1c69e2b4feafb268fb8f37206c4b8731677194bcccb8ae45b6";
    check("v1", 123_576, same);
    check("v2", 123_576, same);
}

#[test]
fn several_blocks() {
    check(
        "multi",
        278_528,
        "02e1c09bbe4da5186ba6b77c3db50d84916ca534b491b937589ba76bced66e3f",
    );
}

#[test]
fn damaged_streams_fail() {
    let path = format!(
        "{}/tests/data/lzfse/multi.lzfse",
        env!("CARGO_MANIFEST_DIR")
    );
    let input = std::fs::read(path).unwrap();
    assert!(lzfse::decode(&input[..input.len() - 1], 278_528).is_err());
    for at in (16..input.len()).step_by(997) {
        let mut damaged = input.clone();
        damaged[at] ^= 0x55;
        if let Ok(out) = lzfse::decode(&damaged, 278_528) {
            assert_eq!(out.len(), 278_528);
        }
    }
}

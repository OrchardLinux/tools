//! LwVM against orchard's Python (`lwvm.py`): the headers in `tests/data`
//! are its tests' synthetic ones, and `lwvm.json` holds what it answers for
//! them (parse, free space, a new Linux entry).
use orchard_formats::lwvm::{self, Snapshot};
use serde_json::{json, Value};

const FULL: &[u8] = include_bytes!("data/lwvm-full.bin");
const SHRUNK: &[u8] = include_bytes!("data/lwvm-shrunk.bin");
const ALLOCATED: &[u8] = include_bytes!("data/lwvm-allocated.bin");

fn expected() -> Value {
    serde_json::from_str(include_str!("data/lwvm.json")).unwrap()
}

fn partition(p: &lwvm::Partition) -> Value {
    json!({
        "number": p.number, "type": p.kind, "partuuid": p.partuuid, "begin": p.begin, "end": p.end,
        "bytes": p.bytes, "attributes": p.attributes, "name": p.name,
        "extents": p.extents.iter().map(|e| json!({"logical": e.logical, "physical": e.physical, "bytes": e.bytes})).collect::<Vec<_>>(),
    })
}

fn snapshot(s: &Snapshot) -> Value {
    json!({
        "media_bytes": s.media_bytes, "chunk_bytes": s.chunk_bytes, "backup_offset": s.backup_offset,
        "partitions": s.partitions.iter().map(partition).collect::<Vec<_>>(),
    })
}

fn pairs(runs: &[(u64, u64)]) -> Value {
    json!(runs.iter().map(|&(a, b)| json!([a, b])).collect::<Vec<_>>())
}

#[test]
fn headers_read_as_python_reads_them() {
    for (name, header) in [("full", FULL), ("shrunk", SHRUNK)] {
        let want = &expected()[name];
        let read = lwvm::parse(header, header, want["media_bytes"].as_u64()).unwrap();
        let mut theirs = want["snapshot"].clone();
        // Python also records the header's hash, which is not part of the reading.
        theirs.as_object_mut().unwrap().remove("header_sha256");
        assert_eq!(snapshot(&read), theirs, "{name}");
        assert_eq!(pairs(&lwvm::free_runs(header, &read)), want["free_runs"], "{name}");
        assert_eq!(json!([lwvm::virtual_room(&read).0, lwvm::virtual_room(&read).1]), want["virtual_room"], "{name}");
        assert_eq!(pairs(&lwvm::free_space(header, &read)), want["free_space"], "{name}");
    }
}

#[test]
fn a_new_linux_entry_is_the_header_python_writes() {
    let want = &expected()["allocate"];
    let (header, made) = lwvm::allocate_free(
        SHRUNK,
        SHRUNK,
        want["uuid"].as_str().unwrap(),
        want["first"].as_u64().unwrap(),
        want["chunks"].as_u64().unwrap(),
    )
    .unwrap();
    assert!(header == ALLOCATED, "the new header differs from Python's");
    assert_eq!(partition(&made), want["partition"]);
}

#[test]
fn what_is_not_a_sound_header_or_free_space_is_refused() {
    let mut torn = FULL.to_vec();
    torn[0x300] ^= 1;
    assert!(lwvm::parse(&torn, &torn, None).is_err(), "a changed header keeps its old checksum");
    assert!(lwvm::parse(FULL, &torn, None).is_err(), "the copies differ");
    assert!(lwvm::parse(FULL, FULL, Some(1 << 20)).is_err(), "another medium size");
    let id = "44444444-5555-4666-8777-888888888888";
    // No free chunk on the full medium; chunk 1 of the shrunk one is taken.
    assert!(lwvm::allocate_free(FULL, FULL, id, 601, 1).is_err());
    assert!(lwvm::allocate_free(SHRUNK, SHRUNK, id, 1, 4).is_err());
    assert!(lwvm::allocate_free(SHRUNK, SHRUNK, id, 601, 353).is_err());
    let taken = lwvm::parse(SHRUNK, SHRUNK, None).unwrap().partitions[0].partuuid.clone();
    assert!(lwvm::allocate_free(SHRUNK, SHRUNK, &taken, 601, 1).is_err());
}

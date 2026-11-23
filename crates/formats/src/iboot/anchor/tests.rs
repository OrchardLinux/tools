use super::*;

fn put(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn pair(data: &mut [u8], at: usize, target: usize) {
    put(data, at, encode_adrp(at, target).unwrap());
    put(data, at + 4, 0x9100_0042 | ((target as u32 & 0xfff) << 10));
}

/// Generated instructions and strings, with two relocatable layouts and one
/// page-tail slot. Every byte outside the relevant shapes is 0xcc.
fn sample(shift: usize) -> Vec<u8> {
    let mut data = vec![0xcc; shift + 0x1100];
    let guard = shift + 0x100;
    let xref = shift + 0x200;
    let anchor = shift + 0x900;
    data[anchor..anchor + ANCHOR.len()].copy_from_slice(ANCHOR);
    pair(&mut data, xref, anchor);
    put(
        &mut data,
        guard,
        0x5400_0001 | (((xref - 8 - guard) as u32 / 4) << 5),
    );
    put(&mut data, guard + 4, MOV_X0_X20);
    put(&mut data, guard + 0x20, FRAME_RESTORE);
    for (i, instruction) in [
        MOV_X0_X20,
        0xaa15_03e3,
        0x9400_0001,
        0x3400_03a0,
        0x5100_7b75,
        0x1400_0001,
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut data, shift + 0x300 + i * 4, instruction);
    }
    put(&mut data, shift + 0x380, 0x9100_83e8);
    put(&mut data, shift + 0x384, 0x9100_c119);
    data[shift + 0x87f..shift + 0x883].copy_from_slice(b"\0%s\0");
    pair(&mut data, shift + 0x500, shift + 0x880);
    put(&mut data, shift + 0x508, 0x9100_03e0);
    put(&mut data, shift + 0x50c, 0x5280_8001);
    put(&mut data, shift + 0x510, 0x9400_0001);
    data[shift + 0xf00..shift + 0x1000].fill(0);
    data
}

#[test]
fn matches_the_reference_patch_words_and_literal_on_relocated_images() {
    for shift in [0, 0x1000] {
        let original = sample(shift);
        let args = "-v wdt=-1 serial=3";
        let actual = patch(&original, args).unwrap();
        let mut expected = original.clone();
        put(&mut expected, shift + 0x104, 0xd280_0000);
        put(&mut expected, shift + 0x30c, 0x1400_001d);
        put(&mut expected, shift + 0x500, 0x9000_0002);
        put(&mut expected, shift + 0x504, 0x913c_4042);
        expected[shift + 0xf10..shift + 0xf10 + args.len()].copy_from_slice(args.as_bytes());
        assert_eq!(actual, expected);
        assert_eq!(word(&actual, shift + 0x100), word(&original, shift + 0x100));
        assert!(
            patch(&actual, args).is_err(),
            "an already-patched image is refused"
        );
    }
}

#[test]
fn signed_pc_relative_addresses_and_instruction_bounds() {
    assert_eq!(encode_adrp(0x1500, 0x880).unwrap(), 0xf0ff_ffe2);
    assert_eq!(adrp_page(0xf0ff_ffe2, 0x1500), Some(0));
    assert_eq!(adrp_page(0xf0ff_ffe2, 0x500), None);
    assert_eq!(encode_branch(0x200, 0x100).unwrap(), 0x17ff_ffc0);
    assert_eq!(branch19(0x54ff_f801, 0x200), Some(0x100));
    assert_eq!(branch19(0x54ff_f801, 0), None);
    assert!(encode_branch(0, 3).is_err());
    assert!(encode_branch(0, 1 << 27).is_err());
    assert!(encode_adrp(0, 1usize << 32).is_err());
    assert_eq!(add_immediate(0x9140_0442), 4096);
    assert_eq!(word(&[0; 3], 0), None);
    assert_eq!(word(&[0; 4], usize::MAX), None);
}

#[test]
fn boot_argument_pointer_can_move_back_a_page_and_use_a_frame_pointer_destination() {
    let mut data = sample(0);
    data.resize(0x1800, 0xcc);
    data[0x500..0x514].fill(0xcc);
    pair(&mut data, 0x1500, 0x880);
    put(&mut data, 0x1508, 0xd100_03a0);
    put(&mut data, 0x150c, 0x5280_8001);
    put(&mut data, 0x1510, 0x9400_0001);
    let actual = patch(&data, "-v").unwrap();
    assert_eq!(word(&actual, 0x1500), Some(0xf0ff_ffe2));
    assert_eq!(word(&actual, 0x1504), Some(0x913c_4042));
    assert_eq!(&actual[0xf10..0xf13], b"-v\0");
}

#[test]
fn refuses_missing_duplicate_or_nonterminated_asn1_anchors() {
    for change in [0, 1, 2] {
        let mut data = sample(0);
        match change {
            0 => data[0x900] ^= 1,
            1 => data[0xa00..0xa00 + ANCHOR.len()].copy_from_slice(ANCHOR),
            _ => data[0x900 + ANCHOR.len() - 1] = 1,
        }
        assert_eq!(
            patch(&data, "-v"),
            Err(Error("cascadia-anchor needs exactly one ASN1 string"))
        );
    }
}

#[test]
fn rejects_broken_validation_guards_and_ambiguous_sites() {
    for (offset, replacement) in [
        (0x100, 0x5400_0000),
        (0x104, 0),
        (0x120, 0),
        (0x200, 0),
        (0x204, 0x9100_0062),
    ] {
        let mut data = sample(0);
        put(&mut data, offset, replacement);
        assert_eq!(
            patch(&data, "-v"),
            Err(Error(
                "cascadia-anchor needs exactly one validation-result site"
            ))
        );
    }
    let mut data = sample(0);
    put(&mut data, 0x140, 0x5400_0001 | (((0x1f8 - 0x140) / 4) << 5));
    put(&mut data, 0x144, MOV_X0_X20);
    put(&mut data, 0x160, FRAME_RESTORE);
    assert!(patch(&data, "-v").is_err());
}

#[test]
fn requires_the_whole_authenticator_flow_and_success_target() {
    for offset in [0x300, 0x304, 0x308, 0x30c, 0x310, 0x314, 0x380, 0x384] {
        let mut data = sample(0);
        put(&mut data, offset, 0);
        assert_eq!(
            patch(&data, "-v"),
            Err(Error(
                "cascadia-anchor needs exactly one manifest-auth gate"
            ))
        );
    }
    for branch in [0x3400_0000, 0x34ff_ffe0, 0x347f_ffe0] {
        let mut data = sample(0);
        put(&mut data, 0x30c, branch);
        assert!(patch(&data, "-v").is_err());
    }
    let mut data = sample(0);
    let flow = data[0x300..0x318].to_vec();
    data[0x600..0x618].copy_from_slice(&flow);
    put(&mut data, 0x680, 0x9100_83e8);
    put(&mut data, 0x684, 0x9100_c119);
    assert!(patch(&data, "-v").is_err());
}

#[test]
fn requires_one_complete_bootargs_call_with_an_isolated_format_string() {
    for offset in [0x500, 0x504, 0x508, 0x50c, 0x510] {
        let mut data = sample(0);
        put(&mut data, offset, 0);
        assert!(patch(&data, "-v").is_err());
    }
    for offset in [0x87f, 0x880, 0x881, 0x882] {
        let mut data = sample(0);
        data[offset] = b'x';
        assert!(patch(&data, "-v").is_err());
    }
    let mut data = sample(0);
    let call = data[0x500..0x514].to_vec();
    data[0x600..0x614].copy_from_slice(&call);
    assert!(patch(&data, "-v").is_err());
}

#[test]
fn literal_space_matches_page_tail_and_fallback_rules() {
    let mut data = vec![0xcc; 0x2100];
    data[0xf00..0x1000].fill(0);
    assert_eq!(literal_slot(&data, 20).unwrap(), 0xf10);
    data[0x1f00..0x2000].fill(0);
    assert!(literal_slot(&data, 20).is_err());
    data[0x1ef0..0x1f00].fill(0);
    assert_eq!(literal_slot(&data, 20).unwrap(), 0x1f00);
    data.fill(0xcc);
    data[0x180..0x200].fill(0);
    data[0x2a0..0x300].fill(0);
    assert_eq!(
        literal_slot(&data, 20).unwrap(),
        0x2b0,
        "smallest fitting padding first"
    );
    assert_eq!(literal_slot(&data, 100).unwrap(), 0x190);
    assert!(literal_slot(&data, 128).is_err());
    assert_eq!(slot_in(&(0..15), 7), Some(8));
    assert_eq!(slot_in(&(0..15), 8), None);
    assert!(literal_slot(&[], 1).is_err());
    assert!(literal_slot(&[0; 17], 1).is_err());
}

#[test]
fn rejects_invalid_literals_and_truncated_images_without_changing_input() {
    let data = sample(0);
    for args in ["", "%s", "-v\0", "é"] {
        assert!(patch(&data, args).is_err());
    }
    assert!(validate_args(&"a".repeat(1024)).is_err());
    validate_args(&"a".repeat(1023)).unwrap();
    for end in 0..0xf13 {
        assert!(patch(&data[..end], "-v").is_err(), "truncated at {end:#x}");
    }
    assert_eq!(data, sample(0));
}

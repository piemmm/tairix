use super::*;

#[test]
fn a_32_bit_vector_loads_each_entry_from_the_table_after_it() {
    assert_eq!(
        jump_through(4 * 48),
        Ok(0xE59F_F0B8),
        "chunk 1's, as RISC OS 5.30 writes it"
    );
    assert_eq!(jump_through(4 * 187), Ok(0xE59F_F2E4), "chunk 2's");
    assert_eq!(
        jump_through(4 * 40),
        Ok(0xE59F_F098),
        "a stub asking for 40"
    );
    assert_eq!(jump_through(4), Ok(0xE51F_F004), "one entry reaches back");
}

#[test]
fn a_26_bit_branch_wraps_to_the_host_pages_from_application_space() {
    let word = branch_26(0x8000, 0x03F0_0010).expect("a branch reaches");
    assert_eq!(word >> 24, 0xEA, "a branch, always");
    let offset = i64::from(((word & 0x00FF_FFFF) << 8).cast_signed() >> 6);
    let target = (0x8000 + 8 + offset).rem_euclid(1 << 26);
    assert_eq!(target, 0x03F0_0010);
    assert!(offset < 0, "back, through the wrap");
    assert_eq!(branch_26(0x03F0_0000, 0x03F0_0040), Ok(0xEA00_000E));
}

#[test]
fn the_root_stack_is_r6_s_top_half_in_kilobytes_nought_the_default() {
    assert_eq!(
        [0, 0x1_0000, 0x1_0001, 0xFFFF, 0x4_0000].map(stack_bytes),
        [0x4000, 0x400, 0x400, 0x4000, 0x1000],
        "as RISC OS 5.30 sized them"
    );
}

#[test]
fn spans_are_signed_as_the_library_compares_them() {
    assert_eq!(
        span(0, u32::MAX),
        None,
        "R4 nought, R5 minus one: no statics"
    );
    assert_eq!(span(0x9000, 0x9040), Some((0x9000, 0x40)));
    assert_eq!(room(0x1000, 0x800), 0, "a limit below the start");
    assert_eq!(bytes_of(signed_span(0x9040, 0x9000)), 0);
}

//! The information SWIs from guest code: `OS_ReadSysInfo`,
//! `OS_PlatformFeatures`, `OS_ReadMemMapInfo`, `OS_ValidateAddress` and
//! `OS_Memory`'s amounts.

use tairix_riscos_arm::psr;

use super::dma::{COMPLETE, DMA_WRITES};
use super::DMA;
use crate::memory::layout::{APPLICATION, RISC_OS_5, SHARED};
use crate::memory::Bits;
use crate::os::Regs;
use crate::swi::os::{
    MEMORY, PLATFORM_FEATURES, READ_MEM_MAP_INFO, READ_SYS_INFO, VALIDATE_ADDRESS,
};
use crate::swi::X;
use crate::testkit::{with_application, Session, DATA};

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn call(s: &mut Session, swi: u32, low: &[u32]) -> Regs {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    s.user_swi(X | swi, r).0
}

fn string(s: &mut Session, at: u32) -> alloc::vec::Vec<u8> {
    s.kernel
        .os()
        .read_until(at, 64, |b| b == 0)
        .expect("a string")
        .0
}

/// `OS_Memory` 19 and its flags, a transfer writing to RAM and one over, are
/// RISC OS's.
#[test]
fn the_dma_reason_and_its_flags_are_risc_os_s() {
    assert_eq!([DMA, DMA_WRITES, COMPLETE], [0x13, 1 << 9, 1 << 10]);
}

#[test]
fn read_sys_info_names_the_release_and_refuses_the_reasons_it_has_not() {
    let mut s = session();
    let regs = call(&mut s, READ_SYS_INFO, &[9, 0]);
    assert_eq!(string(&mut s, regs.r[0]), crate::release::OS_NAME);
    let regs = call(&mut s, READ_SYS_INFO, &[9, 7]);
    assert_eq!(string(&mut s, regs.r[0]), crate::release::PLATFORM);
    assert_eq!(
        call(&mut s, READ_SYS_INFO, &[9, 1]).r[0],
        0,
        "no part number"
    );
    let regs = call(&mut s, READ_SYS_INFO, &[8]);
    assert_eq!(
        regs.r[..3],
        [5, 0, 0x7F],
        "a HAL machine making aligned accesses"
    );
    let regs = call(&mut s, READ_SYS_INFO, &[2, 9, 9, 9, 9]);
    assert_eq!(
        regs.r[..5],
        [0xFFFF_FF00, 0, 0, 0, 0],
        "no chips, no machine id"
    );
    let regs = call(&mut s, READ_SYS_INFO, &[11]);
    assert!(regs.flag(psr::V), "no debug channel");
    assert_eq!(s.peek(regs.r[0]), 0xC61);
    for reason in [10, 16, 0x8000_0000] {
        let regs = call(&mut s, READ_SYS_INFO, &[reason]);
        assert!(regs.flag(psr::V), "{reason}");
        assert_eq!(s.peek(regs.r[0]), 0x1EC);
    }
}

#[test]
fn read_sys_info_13_reads_a_key_handler_s_flags_only_where_it_has_them() {
    let mut s = session();
    let top = APPLICATION + crate::testkit::SLOT_BYTES;
    let header = top - 8;
    s.poke(header, &[0, 0x70]);
    let regs = call(&mut s, READ_SYS_INFO, &[13, header]);
    assert!(
        !regs.flag(psr::V),
        "no flags word, none read past the memory"
    );
    assert_eq!(regs.r[0], 1);
    let flagged = DATA;
    s.poke(flagged, &[0, 0x70 | 1 << 31, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(
        call(&mut s, READ_SYS_INFO, &[13, flagged]).r[0],
        1,
        "wide keys it knows"
    );
    s.poke(flagged + 32, &[3]);
    let regs = call(&mut s, READ_SYS_INFO, &[13, flagged]);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1F4, "a flag it does not");
}

#[test]
fn read_sys_info_6_reads_one_kernel_value_or_a_list() {
    let mut s = session();
    let regs = call(&mut s, READ_SYS_INFO, &[6, 0, 2]);
    assert_eq!(regs.r[2], 0x8000, "the application base");
    s.poke(DATA, &[2, 24, 9999, u32::MAX]);
    let regs = call(&mut s, READ_SYS_INFO, &[6, DATA, DATA + 0x40]);
    assert!(!regs.flag(psr::V));
    assert_eq!(
        [
            s.peek(DATA + 0x40),
            s.peek(DATA + 0x44),
            s.peek(DATA + 0x48)
        ],
        [0x8000, 96, 0],
        "a value no kernel holds reads zero"
    );
}

#[test]
fn platform_features_answer_reason_0_and_34_and_refuse_others() {
    let mut s = session();
    let regs = call(&mut s, PLATFORM_FEATURES, &[0]);
    assert!(!regs.flag(psr::V));
    let all = call(&mut s, PLATFORM_FEATURES, &[34, u32::MAX]);
    assert!(!all.flag(psr::V));
    for feature in [0, 14, 31, 40] {
        let one = call(&mut s, PLATFORM_FEATURES, &[34, feature]).r[0];
        let expected = (all.r[(feature / 32) as usize] >> (feature % 32)) & 1;
        assert_eq!(one, expected, "feature {feature} as the list has it");
    }
    let regs = call(&mut s, PLATFORM_FEATURES, &[99]);
    assert!(regs.flag(psr::V));
}

#[test]
fn memory_map_info_and_validation_describe_the_application() {
    let mut s = session();
    let regs = call(&mut s, READ_MEM_MAP_INFO, &[]);
    assert_eq!(regs.r[0], 0x1000);
    assert!(regs.r[1] > 0);
    let regs = call(
        &mut s,
        VALIDATE_ADDRESS,
        &[APPLICATION, APPLICATION + 0x1000],
    );
    assert!(!regs.flag(psr::C), "application space is valid");
    let regs = call(&mut s, VALIDATE_ADDRESS, &[0x7000_0000, 0x7000_1000]);
    assert!(regs.flag(psr::C), "nothing is mapped there");
}

#[test]
fn os_memory_8_counts_the_ram_pages() {
    let mut s = session();
    let regs = call(&mut s, MEMORY, &[8 | (1 << 8)]);
    assert!(!regs.flag(psr::V));
    assert!(regs.r[1] > 0, "some pages of RAM");
    assert_eq!(regs.r[2], 0x1000, "of a page each");
}

#[test]
fn read_sys_info_1_reads_the_configured_display_as_configure_keeps_it() {
    let mut s = session();
    let cmos = |s: &mut Session, logical: u32, value: u32| {
        call(s, 0x06, &[162, logical, value]);
    };
    // Mode nought is a mode: automatic is its own bit, in &C3.
    cmos(&mut s, 0xC4, 0);
    cmos(&mut s, 0xC3, 0);
    // Monitor type 3, sync 0.
    cmos(&mut s, 0x85, 3 << 2);
    assert_eq!(call(&mut s, READ_SYS_INFO, &[1]).r[..3], [0, 3, 0]);
    // A sync of 2 and a monitor type of 16 have no meaning, so are as
    // automatic as Auto, which no driver describes here.
    cmos(&mut s, 0x85, (16 << 2) | 0x80);
    cmos(&mut s, 0xC3, 1 << 4);
    assert_eq!(call(&mut s, READ_SYS_INFO, &[1]).r[..3], [28, 3, 0]);
}

#[test]
fn os_memory_20_shows_and_hides_the_compatibility_page_and_16_says_which() {
    let mut s = session();
    let area = |s: &mut Session, number: u32| call(s, MEMORY, &[16 | number << 8]).r[1..4].to_vec();
    assert_eq!(
        call(&mut s, MEMORY, &[20, u32::MAX]).r[1],
        1,
        "shown at start"
    );
    assert_eq!(area(&mut s, 16), [0, 0x1000, 0x1000]);
    assert_eq!(area(&mut s, 12)[0], 0xFFFF_0000, "zero page is relocated");
    assert_eq!(call(&mut s, MEMORY, &[20, 0]).r[1], 0);
    assert_eq!(area(&mut s, 16), [0, 0x1000, 0]);
    assert_eq!(
        call(&mut s, MEMORY, &[24, 0, 0x100]).r[1] & 0x11,
        0,
        "nothing at zero to read"
    );
    assert_eq!(call(&mut s, MEMORY, &[20, 1]).r[1], 1);
    assert_eq!(
        call(&mut s, MEMORY, &[24, 0, 0x100]).r[1] & 1,
        1,
        "read again"
    );
    let refused = call(&mut s, MEMORY, &[20, 2]);
    assert!(refused.flag(psr::V));
    assert_eq!(s.peek(refused.r[0]), 0x1EA);
    let mut old = with_application(&SHARED, Bits::Bit26);
    assert_eq!(
        call(&mut old, MEMORY, &[20, 0]).r[1],
        u32::MAX,
        "zero page is at zero: there is no compatibility page"
    );
    assert_eq!(area(&mut old, 12)[0], 0);
    assert_eq!(area(&mut old, 16), [0, 0, 0]);
}

#[test]
fn platform_features_33_describes_the_profile_s_caches() {
    let mut s = session();
    assert_eq!(
        call(&mut s, PLATFORM_FEATURES, &[33, 0]).r[..5],
        [3, 64, 0x8000, 64, 0x8000]
    );
    assert_eq!(call(&mut s, PLATFORM_FEATURES, &[33, 1]).r[..5], [0; 5]);
    let mut old = with_application(&SHARED, Bits::Bit26);
    assert_eq!(
        call(&mut old, PLATFORM_FEATURES, &[33, 0]).r[..5],
        [3, 32, 0x4000, 32, 0x4000],
        "the SA-110's, as RISC OS 5 reports them on one"
    );
    assert_eq!(call(&mut old, PLATFORM_FEATURES, &[32]).r[..2], [0, 0x100]);
}

/// A representative A32 encoding of each instruction group
/// `OS_PlatformFeatures` 34 reports, by its feature bit.
const GROUPS: &[(u32, &[u32])] = &[
    (0, &[0xF3B0_0300]),
    (1, &[0xE7C0_001F]),
    (2, &[0xE120_0070]),
    (3, &[0xE12F_FF30]),
    (4, &[0xE12F_FF10]),
    (5, &[0xF57F_F01F, 0xE1D0_0F9F]),
    (6, &[0xE16F_0F10]),
    (7, &[0xE100_0040]),
    (8, &[0xF57F_F05F]),
    (9, &[0xE160_006E]),
    (10, &[0xE140_0070]),
    (12, &[0xE190_0C9F]),
    (16, &[0xE1C0_00D0]),
    (17, &[0xE1B0_0F9F]),
    (18, &[0xE190_0F9F]),
    (19, &[0xE0F0_00B0]),
    (20, &[0xE1D0_00B0]),
    (21, &[0xE1D0_00D0]),
    (23, &[0xE060_0090]),
    (24, &[0xE300_0000]),
    (25, &[0xE10F_0000]),
    (28, &[0xE320_F000]),
    (29, &[0xE680_0010]),
    (30, &[0xF5D0_F000]),
    (31, &[0xF590_F000]),
    (32, &[0xF4D0_F000]),
    (35, &[0xE100_0050]),
    (36, &[0xE6FF_0F30]),
    (37, &[0xE6BF_0F30]),
    (38, &[0xE320_F005]),
    (39, &[0xF200_0C40]),
    (40, &[0xF300_0C40]),
    (41, &[0xE160_0070]),
    (42, &[0xE100_0080]),
    (43, &[0xE700_0010]),
    (44, &[0xE0C0_0090]),
    (45, &[0xF890_0A00]),
    (46, &[0xE6A0_0010]),
    (47, &[0xE100_0090]),
    (49, &[0xE6A0_0070]),
    (50, &[0xE68F_0070]),
    (51, &[0xE6AF_0070]),
    (54, &[0xE730_F010]),
    (55, &[0xE040_0090]),
    (56, &[0xE080_0090]),
    (57, &[0xE320_F002]),
];

#[test]
fn platform_features_34_reports_what_the_profile_s_decoder_takes() {
    use tairix_riscos_arm::profile::{BIT26, BIT32};
    for (layout, bits, isa) in [
        (&RISC_OS_5, Bits::Bit32, BIT32.isa),
        (&SHARED, Bits::Bit26, BIT26.isa),
    ] {
        let mut s = with_application(layout, bits);
        let page = call(&mut s, PLATFORM_FEATURES, &[34, u32::MAX]);
        for &(bit, encodings) in GROUPS {
            let reported = (page.r[(bit / 32) as usize] >> (bit % 32)) & 1 != 0;
            // The group's instruction, not another the same word is on an
            // older core: an ARMv4 hint is an MSR, a 1111 condition never.
            let decoded = encodings.iter().all(|&word| {
                let here = tairix_riscos_arm::a32::decode(word, isa);
                here.is_some()
                    && here
                        == tairix_riscos_arm::a32::decode(word, tairix_riscos_arm::isa::Isa::ALL)
            });
            assert_eq!(reported, decoded, "{bits:?} feature {bit}");
        }
    }
}

#[test]
fn platform_features_34_on_the_26bit_profile_is_risc_os_5_s_on_an_sa110() {
    let mut s = with_application(&SHARED, Bits::Bit26);
    let page = call(&mut s, PLATFORM_FEATURES, &[34, u32::MAX]);
    // Recorded under RPCEmu's SA-110, less halfword transfers, which the
    // RiscPC's bus lacks and this profile has.
    let halfwords = 1 << 20;
    assert_eq!(
        page.r[..4],
        [0x0E60_4000 | halfwords, 0x8530_9000, 0, 0],
        "{:08X?}",
        &page.r[..4]
    );
}

#[test]
fn os_memory_19_outputs_the_regions_joined_and_marks_a_dma_write_s_partial_lines() {
    use crate::testkit::CODE;
    let mut s = session();
    let (input, output) = (CODE, CODE + 0x20);
    // Input: LDMIA R9!,{R0,R1}; ADD R0,R0,R12; MOV R2,#0. Output:
    // SUB R0,R0,R12; STMIA R9!,{R0,R2,R3}. Each MOV PC,R14.
    s.poke(input, &[0xE8B9_0003, 0xE080_000C, 0xE3A0_2000, 0xE1A0_F00E]);
    s.poke(output, &[0xE040_000C, 0xE8A9_000D, 0xE1A0_F00E]);
    let (regions, out, buffer) = (DATA, DATA + 0x100, DATA + 0x1000);
    s.poke(regions, &[0, 16, 16, 16, 64, 8, 0, 0]);
    let regs = call(&mut s, MEMORY, &[DMA, buffer, regions, input, out, output]);
    assert!(!regs.flag(psr::V));
    assert_eq!((regs.r[2] - regions, regs.r[4] - out), (32, 24));
    let written: alloc::vec::Vec<u32> = (0..6).map(|i| s.peek(out + 4 * i)).collect();
    assert_eq!(written, [0, 32, 0, 64, 8, 0], "the first two joined");
    s.poke(regions, &[0, 16, 0, 0]);
    let regs = call(
        &mut s,
        MEMORY,
        &[DMA | DMA_WRITES, buffer, regions, input, out, output],
    );
    assert_eq!(regs.r[4] - out, 12);
    assert_eq!(
        [s.peek(out), s.peek(out + 4), s.peek(out + 8)],
        [0, 16, 1],
        "too short for a whole cache line: bounced"
    );
    let finished = DMA | DMA_WRITES | COMPLETE;
    let regs = call(&mut s, MEMORY, &[finished, buffer, regions, input, out, 0]);
    assert_eq!(
        (regs.r[2] - regions, regs.r[4]),
        (16, out),
        "finished: nothing output"
    );
    let refused = call(
        &mut s,
        MEMORY,
        &[DMA | 1 << 12, buffer, regions, input, out, output],
    );
    assert!(refused.flag(psr::V));
    assert_eq!(s.peek(refused.r[0]), 0x1EA);
}

//! The RAM disc's area: made at reset as RISC OS 5 makes it, resized a page
//! list and its pages at a time, and refused while the disc holds anything.

use tairix_riscos_arm::psr;

use super::{Occupant, AREA};
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::swi::os::{CHANGE_DYNAMIC_AREA, DYNAMIC_AREA, READ_DYNAMIC_AREA};
use crate::swi::X;
use crate::testkit::{with_application, Session};

fn registers(first: &[u32]) -> [u32; 13] {
    let mut registers = [0; 13];
    registers[..first.len()].copy_from_slice(first);
    registers
}

/// The area's size and page-list length, as `OS_DynamicArea` 2 gives them.
fn sized(s: &mut Session) -> (u32, u32) {
    let (info, _) = s.user_swi(X | DYNAMIC_AREA, registers(&[2, AREA]));
    assert!(!info.flag(psr::V), "area 5 exists");
    (info.r[2], info.r[5])
}

fn change(s: &mut Session, bytes: i32) -> (bool, u32) {
    let (changed, _) = s.user_swi(
        X | CHANGE_DYNAMIC_AREA,
        registers(&[AREA, bytes.cast_unsigned()]),
    );
    (changed.flag(psr::V), changed.r[1])
}

#[test]
fn the_kernel_makes_area_5_as_risc_os_5_makes_it() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let (info, _) = s.user_swi(X | DYNAMIC_AREA, registers(&[2, AREA]));
    assert!(!info.flag(psr::V));
    assert_eq!(info.r[4], 0x0010_0102, "a pool of named pages, privileged");
    assert_eq!(
        (info.r[2], info.r[5]),
        (0, 0),
        "as large as CMOS says: none"
    );
    assert_ne!(info.r[6], 0, "the kernel's handler");
    assert_eq!(info.r[7], info.r[3], "its workspace the area's base");
    assert_eq!(s.read(info.r[8], 9), b"RAM disc\0");
}

#[test]
fn the_pool_resizes_its_page_list_and_its_pages_together() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert_eq!(change(&mut s, 0x4_0000), (false, 0x4_0000));
    assert_eq!(sized(&mut s), (0x4_0000, 0x4_0000));
    let (read, _) = s.user_swi(X | READ_DYNAMIC_AREA, registers(&[0x85]));
    assert_eq!((read.r[1], read.r[2]), (0x4_0000, 0x4_0000));
    assert_eq!(change(&mut s, -0x1_0000), (false, 0x1_0000));
    assert_eq!(sized(&mut s), (0x3_0000, 0x3_0000));
    assert_eq!(
        change(&mut s, -0x10_0000),
        (false, 0x3_0000),
        "no more than it holds"
    );
    assert_eq!(sized(&mut s), (0, 0));
    assert_eq!(change(&mut s, 0), (false, 0), "nothing asked, nothing done");
}

#[test]
fn a_disc_holding_anything_refuses_every_resize() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert_eq!(change(&mut s, 0x2_0000), (false, 0x2_0000));
    s.kernel
        .os()
        .hold_extension(Occupant { occupied: |_| true })
        .expect("held");
    for bytes in [0x1_0000, -0x1_0000] {
        let (failed, moved) = change(&mut s, bytes);
        assert!(failed, "{bytes:#X} refused");
        assert_eq!(moved, 0, "{bytes:#X} moved nothing");
    }
    assert_eq!(sized(&mut s), (0x2_0000, 0x2_0000));
    s.kernel
        .os()
        .hold_extension(Occupant {
            occupied: |_| false,
        })
        .expect("held");
    assert_eq!(
        change(&mut s, -0x2_0000),
        (false, 0x2_0000),
        "empty, it shrinks"
    );
}

#[test]
fn the_refusal_is_risc_os_5s_number() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.kernel
        .os()
        .hold_extension(Occupant { occupied: |_| true })
        .expect("held");
    let (changed, _) = s.user_swi(X | CHANGE_DYNAMIC_AREA, registers(&[AREA, 0x1000]));
    assert!(changed.flag(psr::V));
    assert_eq!(s.peek(changed.r[0]), 0x1C3);
}

#[test]
fn a_page_shown_in_the_window_does_not_stop_the_pool_shrinking() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert_eq!(change(&mut s, 0x2_0000), (false, 0x2_0000));
    let list = 0x9000;
    s.poke(list, &[0, 0, 2, 31, 31, 2]);
    let (mapped, _) = s.user_swi(X | DYNAMIC_AREA, registers(&[22, AREA, list, 2]));
    assert!(!mapped.flag(psr::V), "the first and last pages shown");
    assert_eq!(
        change(&mut s, -0x2_0000),
        (false, 0x2_0000),
        "unmapped as they go"
    );
    assert_eq!(sized(&mut s), (0, 0));
    assert_eq!(
        change(&mut s, 0x1000),
        (false, 0x1000),
        "and it grows again"
    );
}

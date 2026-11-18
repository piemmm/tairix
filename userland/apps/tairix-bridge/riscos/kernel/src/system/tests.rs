//! `OS_Heap` from guest code: its reasons over a heap in application
//! space, and its errors.

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::Regs;
use crate::swi::os::HEAP;
use crate::swi::X;
use crate::testkit::{with_application, Session, DATA};

fn heap(s: &mut Session, low: &[u32]) -> Regs {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    s.user_swi(X | HEAP, r).0
}

#[test]
fn a_heap_in_application_space_hands_out_and_takes_back_blocks() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert!(!heap(&mut s, &[0, DATA, 0, 0x1000]).flag(psr::V));
    let regs = heap(&mut s, &[1, DATA]);
    assert_eq!((regs.r[2], regs.r[3]), (0x1000 - 16 - 4, 0x1000 - 16));
    let regs = heap(&mut s, &[2, DATA, 0, 100]);
    assert!(!regs.flag(psr::V));
    let block = regs.r[2];
    assert_eq!(
        heap(&mut s, &[6, DATA, block]).r[3],
        104,
        "(100 + 7) to a word"
    );
    let regs = heap(&mut s, &[4, DATA, block, 50]);
    assert!(!regs.flag(psr::V));
    let block = regs.r[2];
    assert!(!heap(&mut s, &[3, DATA, block]).flag(psr::V));
    let regs = heap(&mut s, &[3, DATA, block]);
    assert!(regs.flag(psr::V), "freed twice");
    assert_eq!(s.peek(regs.r[0]), 0x185);
    let regs = heap(&mut s, &[5, DATA, 0, 0x100]);
    assert_eq!(regs.r[3], 0x100);
}

#[test]
fn a_bad_reason_and_a_bad_descriptor_are_refused() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let regs = heap(&mut s, &[9, DATA]);
    assert_eq!(s.peek(regs.r[0]), 0x180);
    s.poke(DATA, &[0, 0, 0, 0]);
    let regs = heap(&mut s, &[1, DATA]);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x182);
    let regs = heap(&mut s, &[0, DATA, 0, 20]);
    assert_eq!(s.peek(regs.r[0]), 0x181, "too small to be a heap");
}

#[test]
fn a_claim_that_fails_leaves_r2_zero_as_genuine_risc_os_5_does() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert!(!heap(&mut s, &[0, DATA, 0, 0x400]).flag(psr::V));
    for refused in [
        [8, DATA, 3, 16, 0, 0],
        [7, DATA, 3, 16, 0, 0],
        [8, DATA, 8, 0x1000, 0, 0],
        [2, DATA, 77, 0x1000, 0, 0],
    ] {
        let regs = heap(&mut s, &refused);
        assert!(regs.flag(psr::V), "{refused:?}");
        assert_eq!((s.peek(regs.r[0]), regs.r[2]), (0x184, 0), "{refused:?}");
    }
}

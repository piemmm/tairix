//! `OS_AMBControl` from guest code: reason 9 answered, the rest refused.

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::testkit::{with_application, Session};

const X: u32 = 0x2_0000;
const AMB_CONTROL: u32 = 0x70;

fn amb(s: &mut Session, low: &[u32]) -> crate::os::Regs {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    s.user_swi(X | AMB_CONTROL, r).0
}

#[test]
fn reason_9_succeeds_and_changes_nothing() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let regs = amb(&mut s, &[9, 0x11, 0x22, 0x33]);
    assert!(!regs.flag(psr::V));
    assert_eq!(regs.r[..4], [9, 0x11, 0x22, 0x33]);
}

#[test]
fn every_other_reason_is_one_it_does_not_know() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    for reason in [0, 1, 2, 3, 4, 5, 8, 10, 99, 0x109] {
        let regs = amb(&mut s, &[reason, 2]);
        assert!(regs.flag(psr::V), "{reason}");
        assert_eq!(s.peek(regs.r[0]), 0x180, "{reason}: no such reason code");
    }
}

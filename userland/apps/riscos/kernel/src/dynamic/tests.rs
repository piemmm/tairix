//! Dynamic areas through their SWIs, with a handler in guest code.

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::testkit::asm::{self, Condition};
use crate::testkit::{with_application, CODE, DATA};

const X: u32 = 0x2_0000;
const CHANGE_DYNAMIC_AREA: u32 = 0x2A;
const DYNAMIC_AREA: u32 = 0x66;

#[test]
fn a_shrink_handler_s_message_is_the_refusal_s_text() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let message = DATA + 0x600;
    s.write(message, b"Busy\0");
    s.write(DATA + 0x700, b"Refuser\0");
    s.poke(
        CODE,
        &[
            asm::cmp_imm(0, 2),
            asm::when(Condition::Ne, asm::mov_pc_lr()),
            asm::mov(0, message),
            asm::set_flags(psr::V),
            asm::mov_pc_lr(),
        ],
    );
    let (made, _) = s.user_swi(
        X | DYNAMIC_AREA,
        [
            0,
            u32::MAX,
            0x2000,
            u32::MAX,
            0,
            0x10_0000,
            CODE,
            0,
            DATA + 0x700,
            0,
            0,
            0,
            0,
        ],
    );
    assert!(!made.flag(psr::V), "made");
    let (shrunk, _) = s.user_swi(
        X | CHANGE_DYNAMIC_AREA,
        [
            made.r[1],
            (-0x1000i32).cast_unsigned(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    );
    assert!(shrunk.flag(psr::V), "refused");
    assert_eq!(s.peek(shrunk.r[0]), 0x1C1);
    assert_eq!(s.error_text(shrunk.r[0]), b"Busy");
}

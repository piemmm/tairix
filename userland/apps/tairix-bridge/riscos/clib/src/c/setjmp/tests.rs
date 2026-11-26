use alloc::vec::Vec;

use super::*;
use crate::c::testing::{Client, FREE, RETURN, STATICS};
use crate::kernel::statics::at as kernel_at;

const ENV: u32 = FREE + 0x100;
const ELSEWHERE: u32 = FREE + 0x200;
const ONE: [u32; 3] = [0x3FFF, 0x8000_0000, 0];
const TWO: [u32; 3] = [0x4000, 0x8000_0000, 0];

fn registers(r0: u32, r1: u32, base: u32, sp: u32, lr: u32) -> [u32; 15] {
    let mut registers = [0; 15];
    registers[..2].copy_from_slice(&[r0, r1]);
    for (n, slot) in (0..).zip(registers[4..10].iter_mut()) {
        *slot = base + n;
    }
    registers[10] = crate::stack_chunk::limit_of(crate::c::testing::CHUNK);
    registers[11] = 0x0F0F;
    registers[13] = sp;
    registers[14] = lr;
    registers
}

#[test]
fn setjmp_keeps_r4_to_fp_sp_and_the_return_then_f4_to_f7_in_extended_form() {
    let mut client = Client::new();
    let given = registers(ENV, 0, 0x40, 0xE000, ELSEWHERE);
    client.running_with(&given);
    for n in 4..8 {
        client
            .session
            .kernel
            .os()
            .set_fpa_extended(n, if n % 2 == 0 { ONE } else { TWO })
            .expect("set");
    }
    let (done, after, pc) = client.subroutine(setjmp);
    assert!(done.is_ok());
    assert_eq!((after[0], pc), (0, ELSEWHERE), "nought, back to the caller");
    let words: Vec<u32> = (0..22).map(|n| client.word(ENV + 4 * n)).collect();
    assert_eq!(words[..8], given[4..12], "R4 to FP");
    assert_eq!(words[8..10], given[13..], "SP and the return");
    assert_eq!(words[10..], [ONE, TWO, ONE, TWO].concat());
}

#[test]
fn longjmp_goes_on_from_setjmp_s_return_with_its_value_or_one() {
    let mut client = Client::new();
    client.running_with(&registers(ENV, 0, 0x40, 0xE000, ELSEWHERE));
    client
        .session
        .kernel
        .os()
        .set_fpa_extended(4, ONE)
        .expect("set");
    let (kept, _, _) = client.subroutine(setjmp);
    assert!(kept.is_ok(), "the environment kept");
    for (value, answered) in [(7, 7), (0, 1), (u32::MAX, u32::MAX)] {
        client.running_with(&registers(ENV, value, 0x90, 0xD000, RETURN));
        client
            .session
            .kernel
            .os()
            .set_fpa_extended(4, TWO)
            .expect("set");
        client.put(
            STATICS + kernel_at::TRAP_SEARCH + trap::state::ACTIVE,
            &1_u32.to_le_bytes(),
        );
        let (done, after, pc) = client.subroutine(longjmp);
        assert!(done.is_ok());
        assert_eq!(after[0], answered, "longjmp(env, {value})");
        let kept = registers(ENV, 0, 0x40, 0xE000, ELSEWHERE);
        assert_eq!(after[4..12], kept[4..12], "R4 to FP as kept");
        assert_eq!(after[13..], kept[13..], "SP and the return as kept");
        assert_eq!(pc, ELSEWHERE, "setjmp's return");
        assert_eq!(client.session.kernel.os().fpa_extended(4), Ok(ONE));
        assert_eq!(
            client.word(STATICS + kernel_at::TRAP_SEARCH + trap::state::ACTIVE),
            0,
            "the trap over"
        );
    }
}

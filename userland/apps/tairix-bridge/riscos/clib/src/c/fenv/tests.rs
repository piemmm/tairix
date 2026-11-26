//! The status register's answers as the g2 probe recorded RISC OS 5.30's,
//! with the client's FPSR as a C program starts with it: invalid operation,
//! division by zero and overflow trapped.

use tairix_riscos_kernel::FRESH_FPSR as START;

use super::*;
use crate::c::testing::{Client, FREE};

const ENVIRONMENT: u32 = FREE;
const FLAG: u32 = FREE + 0x10;

fn client() -> Client {
    let mut client = Client::new();
    client.session.kernel.os().set_fpsr(START);
    client
}

fn fpsr_of(client: &mut Client) -> u32 {
    client.session.kernel.os().fpsr()
}

#[test]
fn rounding_is_only_ever_to_nearest_and_setting_it_answers_what_was_asked() {
    let mut client = client();
    for asked in [0, 1, 2, 3, 0x40_0000, 0x80_0000, 0xC0_0000, u32::MAX] {
        assert_eq!(client.answer(fesetround, &[asked]), asked);
        assert_eq!(client.answer(fegetround, &[]), 0);
    }
    assert_eq!(fpsr_of(&mut client), START, "nothing changed");
}

#[test]
fn raising_is_an_operation_s_raising_trapped_where_enabled() {
    let mut client = client();
    for (excepts, after, trapped) in [
        (0x10, START | 0x10, false),
        (0x08, START | 0x18, false),
        (0x04, START, true),
        (0x02, START, true),
        (0x01, START, true),
        (0x1F, START, true),
    ] {
        assert_eq!(client.answer(feclearexcept, &[0x1F]), 0);
        let outcome = client.outcome(feraiseexcept, &[excepts]);
        assert_eq!(outcome.is_err(), trapped, "{excepts:#X}");
        if let Ok(regs) = outcome {
            assert_eq!(regs.r[0], 0);
        }
        assert_eq!(
            fpsr_of(&mut client),
            after,
            "{excepts:#X}: an underflow inexact too"
        );
        assert_eq!(client.answer(fetestexcept, &[0x1F]), after & 0x1F);
        assert_eq!(client.answer(fetestexcept, &[u32::MAX]), after & 0x1F);
    }
    assert_eq!(client.answer(feclearexcept, &[0x1F]), 0);
    assert_eq!(
        client.answer(feraiseexcept, &[0x20]),
        0x20,
        "no such exception"
    );
    assert_eq!(fpsr_of(&mut client), START);
}

#[test]
fn flags_are_one_word_read_and_written_by_mask() {
    let mut client = client();
    client.put(FLAG, &[0x55; 8]);
    assert_eq!(client.answer(fegetexceptflag, &[FLAG, 0x1F]), 0);
    assert_eq!(
        [client.word(FLAG), client.word(FLAG + 4)],
        [0, 0x5555_5555],
        "one word"
    );
    client.put(FLAG, &0x1F_u32.to_le_bytes());
    assert_eq!(client.answer(fesetexceptflag, &[FLAG, 0x1F]), 0);
    assert_eq!(fpsr_of(&mut client), START | 0x1F, "set, nothing trapped");
    assert_eq!(client.answer(fesetexceptflag, &[FLAG, 3]), 0);
    assert_eq!(fpsr_of(&mut client), START | 0x1F);
}

#[test]
fn an_environment_is_the_status_held_updated_and_set_again() {
    let mut client = client();
    client.put(ENVIRONMENT, &[0x77; 16]);
    assert_eq!(
        client.answer(fegetenv, &[ENVIRONMENT]),
        ENVIRONMENT,
        "the pointer answered"
    );
    assert_eq!(
        [client.word(ENVIRONMENT), client.word(ENVIRONMENT + 4)],
        [START, 0x7777_7777]
    );
    client.session.kernel.os().set_fpsr(START | 0x1F);
    let held = ENVIRONMENT + 8;
    assert_eq!(client.answer(feholdexcept, &[held]), 0);
    assert_eq!(client.word(held), START | 0x1F);
    assert_eq!(fpsr_of(&mut client), 0x0100_0000, "the system id alone");
    assert_eq!(client.answer(feraiseexcept, &[1]), 0, "held, so flagged");
    assert_eq!(fpsr_of(&mut client), 0x0100_0001);
    assert!(
        client.outcome(feupdateenv, &[held]).is_err(),
        "the held invalid operation trapped"
    );
    assert_eq!(
        fpsr_of(&mut client),
        START | 0x1F,
        "the environment set before it"
    );
    assert_eq!(client.answer(fesetenv, &[ENVIRONMENT]), 0);
    assert_eq!(fpsr_of(&mut client), START);
    assert!(
        client.outcome(fesetenv, &[u32::MAX]).is_err(),
        "read from where nothing is"
    );
    assert_eq!(fpsr_of(&mut client), START);
}

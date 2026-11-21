//! The filing vectors with nothing claiming them: every name names nothing,
//! no handle is open, the queries the command line makes answer none, and
//! what only a filing system could do is refused.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::Regs;
use crate::swi::os::{fs_control, BGET, BPUT, CLAIM, FS_CONTROL, GBPB};
use crate::swi::os::{ARGS, CHANGE_ENVIRONMENT, FILE, FIND, GET_ENV};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA};
use crate::vectors;

const NAME: u32 = DATA;

fn session() -> Session {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.write(NAME, b"Nowhere.Nothing\0");
    s
}

/// The call's registers, or its error's number and text.
fn call(s: &mut Session, swi: u32, r: [u32; 4]) -> Result<Regs, (u32, Vec<u8>)> {
    let (regs, _) = s.user_swi(X | swi, [r[0], r[1], r[2], r[3], 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    if regs.flag(psr::V) {
        let number = s.peek(regs.r[0]);
        Err((number, s.error_text(regs.r[0])))
    } else {
        Ok(regs)
    }
}

fn number(result: Result<Regs, (u32, Vec<u8>)>) -> Result<u32, u32> {
    result.map(|regs| regs.r[0]).map_err(|(number, _)| number)
}

#[test]
fn a_name_names_nothing_to_read_and_nothing_to_write() {
    let mut s = session();
    for reason in [5, 6, 13, 15, 17, 20, 21, 22, 23] {
        assert_eq!(
            number(call(&mut s, FILE, [reason, NAME, 0, 0])),
            Ok(0),
            "{reason}"
        );
    }
    for reason in [0, 1, 7, 8, 10, 11, 12, 16, 18, 24, 255] {
        assert_eq!(
            number(call(&mut s, FILE, [reason, NAME, 0, 0])),
            Err(0xD6),
            "{reason}"
        );
    }
    let (_, text) = call(&mut s, FILE, [255, NAME, 0, 0]).unwrap_err();
    assert!(
        text.windows(15).any(|w| w == b"Nowhere.Nothing"),
        "the name quoted"
    );
    assert_eq!(number(call(&mut s, FILE, [25, NAME, 0, 0])), Err(0x402));
}

#[test]
fn a_made_error_names_the_object_as_its_kind() {
    let mut s = session();
    for (kind, error) in [(0, 0xD6), (1, 0x41C), (2, 0xA8), (3, 0xA8), (0x100, 0xD6)] {
        assert_eq!(
            number(call(&mut s, FILE, [19, NAME, kind, 0])),
            Err(error),
            "{kind}"
        );
    }
    assert!(
        call(&mut s, FILE, [19, NAME, 7, 0]).is_ok(),
        "no kind, no error"
    );
}

#[test]
fn an_open_finds_nothing_and_a_close_nothing_open() {
    let mut s = session();
    for mode in [0x40, 0xC0, 0x43, 0xC1] {
        assert_eq!(
            number(call(&mut s, FIND, [mode, NAME, 0, 0])),
            Ok(0),
            "{mode:#x}"
        );
    }
    for mode in [0x48, 0x4C, 0xC8, 0x80, 0x88] {
        assert_eq!(
            number(call(&mut s, FIND, [mode, NAME, 0, 0])),
            Err(0xD6),
            "{mode:#x}"
        );
    }
    assert_eq!(number(call(&mut s, FIND, [0x10, NAME, 0, 0])), Err(0x405));
    assert!(
        call(&mut s, FIND, [0, 0, 0, 0]).is_ok(),
        "closing everything closes nothing"
    );
    assert_eq!(number(call(&mut s, FIND, [0, 250, 0, 0])), Err(0xDE));
}

#[test]
fn no_handle_is_open() {
    let mut s = session();
    assert_eq!(number(call(&mut s, BGET, [0, 250, 0, 0])), Err(0xDE));
    assert_eq!(number(call(&mut s, BPUT, [65, 250, 0, 0])), Err(0xDE));
    for reason in [0, 1, 2, 5, 8, 255] {
        assert_eq!(
            number(call(&mut s, ARGS, [reason, 250, 0, 0])),
            Err(0xDE),
            "{reason}"
        );
    }
    assert_eq!(number(call(&mut s, ARGS, [254, 250, 0, 0])), Ok(0x800));
    assert_eq!(
        number(call(&mut s, ARGS, [0, 0, 0, 0])),
        Ok(0),
        "no filing system"
    );
    assert!(
        call(&mut s, ARGS, [255, 0, 0, 0]).is_ok(),
        "nothing to flush"
    );
    assert_eq!(number(call(&mut s, ARGS, [100, 250, 0, 0])), Err(0x403));
    for reason in [1, 4] {
        assert_eq!(number(call(&mut s, GBPB, [reason, 250, 0, 0])), Err(0xDE));
    }
    assert_eq!(number(call(&mut s, GBPB, [5, 0, 0, 0])), Err(0xF8));
    assert_eq!(number(call(&mut s, GBPB, [9, NAME, 0, 0])), Err(0xD6));
    assert_eq!(number(call(&mut s, GBPB, [100, 0, 0, 0])), Err(0x404));
}

#[test]
fn the_filing_queries_answer_none_and_the_rest_find_nothing() {
    let mut s = session();
    let prefix = call(&mut s, FS_CONTROL, [11, NAME, 0, 7]).unwrap();
    assert_eq!((prefix.r[1], prefix.r[2], prefix.r[3]), (NAME, u32::MAX, 0));
    let present = call(&mut s, FS_CONTROL, [13, NAME, 1, 0]).unwrap();
    assert_eq!(present.r[2], 0, "no filing system present");
    for reason in [fs_control::TEMPORARY_MODULE, fs_control::SECONDARY_MODULE] {
        let base = call(&mut s, FS_CONTROL, [reason, 7, 7, 0]).unwrap();
        assert_eq!((base.r[1], base.r[2]), (0, 0), "{reason}");
    }
    for reason in [
        fs_control::START_APPLICATION,
        fs_control::BOOT,
        fs_control::RESTORE,
        fs_control::CLOSE_ALL,
    ] {
        assert!(
            call(&mut s, FS_CONTROL, [reason, 0, 0, 0]).is_ok(),
            "{reason}"
        );
    }
    for reason in [0, 4, 5, 9, 25, 32, 57] {
        assert_eq!(
            number(call(&mut s, FS_CONTROL, [reason, NAME, 0, 0])),
            Err(0xD6),
            "{reason}"
        );
    }
    for reason in [12, 14, 18, 37, 58] {
        assert_eq!(
            number(call(&mut s, FS_CONTROL, [reason, NAME, 0, 0])),
            Err(0xF8),
            "{reason}"
        );
    }
    let handle = call(&mut s, FS_CONTROL, [21, 250, 7, 0]).unwrap();
    assert_eq!(
        (handle.r[1], handle.r[2]),
        (0, 0),
        "no handle converts to nought"
    );
    assert_eq!(
        number(call(&mut s, FS_CONTROL, [26, NAME, 0, 0])),
        Err(0xFD),
        "a copy to no name is a bad string"
    );
    for reason in [3, 29, 34, 59, 0xFF] {
        assert_eq!(
            number(call(&mut s, FS_CONTROL, [reason, 0, 0, 0])),
            Err(0x401),
            "{reason}"
        );
    }
}

#[test]
fn an_unknown_command_is_a_file_not_found() {
    let mut s = session();
    s.write(NAME, b"NoSuchCommand\0");
    let (number, text) = call(&mut s, 0x05, [NAME, 0, 0, 0]).unwrap_err();
    assert_eq!(number, 0xD6);
    assert!(text.windows(13).any(|w| w == b"NoSuchCommand"), "{text:?}");
}

#[test]
fn a_new_application_is_told_set_up_and_left_no_handler_of_the_old() {
    use crate::env::{CURRENT_ACTIVE_OBJECT, ESCAPE};
    let mut s = session();
    s.poke(CODE, &[asm::mov_pc_lr()]);
    call(&mut s, CHANGE_ENVIRONMENT, [ESCAPE, CODE, 0, 0]).unwrap();
    let tail = DATA + 0x40;
    let name = DATA + 0x80;
    s.write(tail, b"its tail\r");
    s.write(name, b"Program\0");
    call(
        &mut s,
        FS_CONTROL,
        [fs_control::START_APPLICATION, tail, 0xABC0, name],
    )
    .unwrap();
    let env = call(&mut s, GET_ENV, [0, 0, 0, 0]).unwrap();
    assert_eq!(s.read(env.r[0], 17), b"Program its tail\0");
    let object = call(&mut s, CHANGE_ENVIRONMENT, [CURRENT_ACTIVE_OBJECT, 0, 0, 0]).unwrap();
    assert_eq!(object.r[1], 0xABC0);
    let escape = call(&mut s, CHANGE_ENVIRONMENT, [ESCAPE, 0, 0, 0]).unwrap();
    assert_ne!(escape.r[1], CODE, "the old program's handler is gone");

    s.poke(CODE + 0x10, &[asm::mov(0, 0), asm::claim()]);
    call(&mut s, CLAIM, [vectors::UPCALL.0, CODE + 0x10, 0, 0]).unwrap();
    assert_eq!(
        number(call(
            &mut s,
            FS_CONTROL,
            [fs_control::START_APPLICATION, tail, 0, name]
        )),
        Err(0x600),
        "UpCallV refused it"
    );
}

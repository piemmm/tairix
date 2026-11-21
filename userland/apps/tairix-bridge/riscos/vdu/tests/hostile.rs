//! What hostile programs were found doing to the drivers, each now answered
//! as a value: selectors naming no screen, a module answering a service with
//! a wild list, a copy cursor left behind by a smaller window, a cursor
//! wrapped round the coordinates, and characters as large as VDU 5 allows.

use tairix_riscos_filing::session::{session, Session, Setup};
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{BYTE, PLOT, READ_C, READ_MODE_VARIABLE, SCREEN_MODE, WRITE_C};
use tairix_riscos_kernel::swi::X;
use tairix_riscos_vdu::display;

const SELECTOR_AT: u32 = 0x9000;
/// `OS_Module`, and its reasons to claim a block and insert a module there.
const MODULE: u32 = 0x1E;
const CLAIM: u32 = 6;
const INSERT: u32 = 10;

/// A 32-bit guest module whose service handler claims
/// `Service_ModeExtension` with a list at the top of memory.
fn wild_module() -> Vec<u8> {
    let header = [0, 0, 0, 0x34, 0x4C, 0x4C, 0, 0, 0, 0, 0, 0, 0x48];
    let handler = [
        0xE331_0050, // TEQ R1, #&50
        0x11A0_F00E, // MOVNE PC, R14
        0xE3A0_1000, // MOV R1, #0
        0xE3E0_4003, // MVN R4, #3
        0xE1A0_F00E, // MOV PC, R14
    ];
    let flags = [1];
    let mut image: Vec<u8> = header
        .iter()
        .chain(&handler)
        .chain(&flags)
        .flat_map(|w: &u32| w.to_le_bytes())
        .collect();
    image.extend_from_slice(b"Wild\0\0\0\0");
    image
}

fn boot(keys: &'static [u8]) -> Session {
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: &tairix_riscos_vdu::MODULES,
        bytes: 0x10_0000,
        keys,
        disc: None,
    })
}

fn vdu(session: &mut Session, bytes: &[u8]) -> Result<(), Fail> {
    for &byte in bytes {
        let mut regs = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        session.kernel.os().perform(X | WRITE_C, &mut regs)?;
    }
    Ok(())
}

/// The drivers still read a mode variable.
fn answers(session: &mut Session) -> bool {
    let mut regs = Regs::new([u32::MAX, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | READ_MODE_VARIABLE, &mut regs)
        .is_ok()
}

#[test]
fn a_selector_naming_no_screen_is_refused() {
    let mut session = boot(b"");
    for (index, value) in [
        (12, u32::MAX),
        (11, u32::MAX),
        (1, u32::MAX),
        (10, 32),
        (10, 7),
        (4, 32),
        (3, 256),
        (6, 0x100_0000),
    ] {
        let os = &mut session.kernel.os();
        for (i, word) in (0u32..).zip([1, 640, 480, 3, u32::MAX, index, value, u32::MAX]) {
            os.write_word(SELECTOR_AT + 4 * i, word).expect("written");
        }
        let mut regs = Regs::new([0, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let refused = os.perform(X | SCREEN_MODE, &mut regs);
        assert!(
            matches!(refused, Err(Fail::Error(_))),
            "variable {index} as {value:#x}: {refused:?}"
        );
        assert!(answers(&mut session));
    }
}

#[test]
fn a_teletext_page_past_what_one_holds_is_refused() {
    let mut session = boot(b"");
    let os = &mut session.kernel.os();
    os.write_bytes(SELECTOR_AT, b"X640 Y500 T16 TX999999999\0")
        .expect("written");
    let mut regs = Regs::new([15, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(matches!(
        os.perform(X | SCREEN_MODE, &mut regs),
        Err(Fail::Error(_))
    ));
    os.write_bytes(SELECTOR_AT, b"X640 Y500 T16\0")
        .expect("written");
    let mut regs = Regs::new([15, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(
        os.perform(X | SCREEN_MODE, &mut regs).is_ok(),
        "a page of the cells that fit"
    );
}

#[test]
fn a_mode_extension_answered_with_a_wild_list_is_a_value() {
    let mut session = boot(b"");
    let image = wild_module();
    let os = &mut session.kernel.os();
    let mut claim = Regs::new([
        CLAIM,
        0,
        0,
        u32::try_from(image.len()).unwrap(),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.perform(X | MODULE, &mut claim).expect("claimed");
    os.write_bytes(claim.r[2], &image).expect("written");
    let mut insert = Regs::new([INSERT, claim.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | MODULE, &mut insert).expect("inserted");
    let _ = vdu(&mut session, &[22, 60]);
    assert!(answers(&mut session));
}

#[test]
fn a_copy_cursor_outside_a_smaller_window_is_let_go() {
    let mut session = boot(b"");
    vdu(&mut session, &[22, 12, 31, 0, 31]).expect("the cursor on the last row");
    for key in [0x8F, b'A'] {
        let mut regs = Regs::new([138, 0, u32::from(key), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        session
            .kernel
            .os()
            .perform(X | BYTE, &mut regs)
            .expect("inserted");
    }
    let mut regs = Regs::new([0; 13]);
    session
        .kernel
        .os()
        .perform(X | READ_C, &mut regs)
        .expect("read");
    assert_eq!(
        regs.r[0],
        u32::from(b'A'),
        "the cursor key taken by copy editing"
    );
    vdu(
        &mut session,
        &[28, 0, 5, 10, 0, 23, 16, 4, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("written");
    let mut regs = Regs::new([135, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(session.kernel.os().perform(X | BYTE, &mut regs).is_ok());
    assert!(
        display::cursor(&mut session.kernel.os()).is_some(),
        "the text cursor drawn where it stands"
    );
}

#[test]
fn shapes_about_a_cursor_wrapped_round_the_coordinates_are_values() {
    let mut session = boot(b"");
    vdu(&mut session, &[22, 28]).expect("mode 28");
    let plot = |session: &mut Session, code: u32, x: i32, y: i32| {
        let mut regs = Regs::new([
            code,
            x.cast_unsigned(),
            y.cast_unsigned(),
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
        ]);
        session.kernel.os().perform(X | PLOT, &mut regs)
    };
    for _ in 0..70_000 {
        plot(&mut session, 0, 32_767, -32_767).expect("moved");
    }
    for code in [
        5, 85, 101, 117, 133, 149, 157, 165, 173, 181, 189, 197, 205, 189,
    ] {
        assert!(
            plot(&mut session, code, 30_000, -30_000).is_ok(),
            "PLOT {code}"
        );
        assert!(
            plot(&mut session, code, -32_768, 32_767).is_ok(),
            "PLOT {code} back"
        );
    }
    assert!(answers(&mut session));
}

#[test]
fn a_character_as_large_as_vdu_5_allows_is_drawn_on_the_screen_alone() {
    let mut session = boot(b"");
    vdu(
        &mut session,
        &[
            22, 28, 23, 255, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ],
    )
    .expect("a block");
    vdu(
        &mut session,
        &[
            23, 17, 7, 2, 0, 0x80, 0, 0x80, 0, 0, 5, 25, 4, 0, 0, 0xBF, 3,
        ],
    )
    .expect("set up");
    vdu(&mut session, b"\xFF").expect("printed");
    let mut line = Vec::new();
    display::line(&mut session.kernel.os(), 0, &mut line).expect("read");
    assert!(
        line.iter().all(|&colour| colour != 0),
        "the block glyph's top row fills the screen's top line"
    );
}

use crate::input::InputEvent;
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::msgtrans::tests::swi;
use crate::swi::os::{MOUSE, WORD};
use crate::testkit::{with_application, Session, DATA};

/// `OS_Word` 21's reason `reason` with `rest` after it in the block.
fn word_21(s: &mut Session, reason: u8, rest: &[u8]) {
    let mut block = alloc::vec![reason];
    block.extend_from_slice(rest);
    s.write(DATA, &block);
    swi(s, WORD, [21, DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("OS_Word 21");
}

fn halves(values: &[i16]) -> alloc::vec::Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// What `OS_Mouse` reads: the position and the buttons.
fn mouse(s: &mut Session) -> (i32, i32, u32) {
    let regs = swi(s, MOUSE, [0; 13]).expect("OS_Mouse");
    (regs.r[0].cast_signed(), regs.r[1].cast_signed(), regs.r[2])
}

fn deliver(s: &mut Session, event: InputEvent) {
    s.arrive(0, event);
    s.kernel.os().take_input().expect("taken");
}

#[test]
fn a_position_past_the_box_is_kept_within_it() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    word_21(&mut s, 1, &halves(&[0, 0, 1279, 1023]));
    deliver(
        &mut s,
        InputEvent::Pointer {
            x: 2000,
            y: -40,
            buttons: 0,
        },
    );
    assert_eq!(mouse(&mut s), (1279, 0, 0));
    word_21(&mut s, 4, &[0; 4]);
    assert_eq!(s.read(DATA + 1, 4), halves(&[1279, 0]));
}

#[test]
fn a_new_box_homes_the_mouse_and_moves_what_is_buffered_into_it() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    deliver(
        &mut s,
        InputEvent::Pointer {
            x: 900,
            y: 700,
            buttons: 4,
        },
    );
    word_21(&mut s, 1, &halves(&[100, 100, 500, 400]));
    assert_eq!(s.kernel.os().mouse_position(), (500, 400));
    assert_eq!(
        mouse(&mut s),
        (500, 400, 4),
        "the packet, read inside the box"
    );
}

#[test]
fn a_box_turned_inside_out_is_ignored() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    word_21(&mut s, 1, &halves(&[500, 0, 100, 400]));
    deliver(
        &mut s,
        InputEvent::Pointer {
            x: 900,
            y: 700,
            buttons: 0,
        },
    );
    assert_eq!(mouse(&mut s), (900, 700, 0));
}

#[test]
fn a_position_set_outside_the_box_is_ignored() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    word_21(&mut s, 1, &halves(&[0, 0, 639, 511]));
    word_21(&mut s, 3, &halves(&[320, 256]));
    assert_eq!(s.kernel.os().mouse_position(), (320, 256));
    word_21(&mut s, 3, &halves(&[700, 256]));
    assert_eq!(s.kernel.os().mouse_position(), (320, 256));
}

#[test]
fn movement_is_multiplied_by_the_configured_step_until_set() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    deliver(
        &mut s,
        InputEvent::Motion {
            dx: 10,
            dy: -5,
            buttons: 0,
        },
    );
    assert_eq!(s.kernel.os().mouse_position(), (20, -10), "MouseStep 2");
    word_21(&mut s, 2, &[3, 0xFF]);
    deliver(
        &mut s,
        InputEvent::Motion {
            dx: 10,
            dy: 5,
            buttons: 1,
        },
    );
    assert_eq!(mouse(&mut s), (50, -15, 1));
}

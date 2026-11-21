//! What a front end is shown of a session's screen: its geometry, each line
//! as colours through the palette, the lines stored into since it last
//! asked, and the text cursor drawn over it.

mod common;

use common::{drivers, vdu};
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::os::{BYTE, WORD};
use tairix_riscos_kernel::swi::X;
use tairix_riscos_vdu::display;

/// The runs of lines `damage` gives.
fn damaged(session: &mut Session) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    display::damage(&mut session.kernel.os(), |first, last| {
        runs.push((first, last));
    });
    runs
}

#[test]
fn a_screen_is_shown_through_its_palette_with_the_lines_drawn_since_last_asked() {
    let mut session = drivers(b"");
    vdu(&mut session, &[22, 12]).expect("written");
    let view = display::view(&mut session.kernel.os()).expect("a screen");
    assert_eq!(
        (view.width, view.height, view.x_eig, view.y_eig),
        (640, 256, 1, 2)
    );
    assert!(!view.blanked);
    damaged(&mut session);
    assert_eq!(damaged(&mut session), [], "taken once");

    // GCOL 0,1, then a red rectangle from the origin to (99, 99): pixels 0
    // to 49 across and the bottom 25 lines.
    vdu(
        &mut session,
        &[18, 0, 1, 25, 4, 0, 0, 0, 0, 25, 101, 99, 0, 99, 0],
    )
    .expect("written");
    let runs = damaged(&mut session);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let (first, last) = runs[0];
    assert!((229..=231).contains(&first) && last == 255, "{runs:?}");

    let mut line = Vec::new();
    display::line(&mut session.kernel.os(), 240, &mut line).expect("read");
    assert_eq!(line.len(), 640);
    assert!(
        line[..50].iter().all(|&c| c == 0xFF_0000),
        "red where drawn"
    );
    assert!(line[50..].iter().all(|&c| c == 0), "black beyond");
    display::line(&mut session.kernel.os(), 256, &mut line).expect("read");
    assert!(line.is_empty(), "no line past the last");
}

#[test]
fn a_double_pixel_mode_s_lines_are_shown_at_its_width() {
    let mut session = drivers(b"");
    vdu(&mut session, &[22, 2]).expect("written");
    let view = display::view(&mut session.kernel.os()).expect("a screen");
    assert_eq!((view.width, view.height), (320, 256));
    vdu(
        &mut session,
        &[18, 0, 2, 25, 4, 0, 0, 0, 0, 25, 101, 7, 0, 3, 0],
    )
    .expect("written");
    let mut line = Vec::new();
    display::line(&mut session.kernel.os(), 255, &mut line).expect("read");
    assert_eq!(line.len(), 320);
    assert_eq!(
        &line[..2],
        [0x00_FF00; 2],
        "the first stored pixel shown twice"
    );
    assert_eq!(line[2], 0);
}

#[test]
fn the_text_cursor_is_drawn_where_it_stands_and_not_in_vdu_5() {
    let mut session = drivers(b"");
    vdu(&mut session, &[22, 12, 31, 3, 2]).expect("written");
    let cursor = display::cursor(&mut session.kernel.os()).expect("a cursor");
    assert_eq!((cursor.x, cursor.y, cursor.width), (24, 16, 8));
    vdu(&mut session, &[5]).expect("written");
    assert_eq!(
        display::cursor(&mut session.kernel.os()),
        None,
        "none at the graphics cursor"
    );
}

#[test]
fn the_pointer_is_its_selected_shape_a_pixel_to_a_colour() {
    let mut session = drivers(b"");
    let mut pixels = Vec::new();
    let os = &mut session.kernel.os();
    assert_eq!(
        display::pointer(os, &mut pixels).expect("read"),
        None,
        "no shape selected"
    );
    let (block, data) = (0x9000, 0xA000u32);
    let mut head = vec![0, 1, 1, 2, 1, 0];
    head.extend(data.to_le_bytes());
    os.write_bytes(block, &head).expect("written");
    os.write_bytes(data, &[0b1110_0100, 0b0000_0001])
        .expect("written");
    os.perform(
        X | WORD,
        &mut Regs::new([21, block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    )
    .expect("defined");
    os.perform(
        X | BYTE,
        &mut Regs::new([106, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    )
    .expect("selected");
    let view = display::pointer(os, &mut pixels)
        .expect("read")
        .expect("a pointer");
    assert_eq!((view.width, view.height, view.active), (4, 2, (1, 0)));
    assert_eq!(
        pixels,
        [0, 1, 2, 3, 1, 0, 0, 0],
        "the leftmost pixel in each byte's low bits"
    );
}

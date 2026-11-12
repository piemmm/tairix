//! The hooks' code: assembled to fit where it is laid, its jumps resolved.

use super::*;

#[test]
fn the_filter_and_the_handler_fit_their_room() {
    let filter = filter(at::OLD_09).unwrap();
    assert!(
        filter.len() <= usize::from(at::FILTER_LEN),
        "{} bytes",
        filter.len()
    );
    assert_eq!(
        &filter[..5],
        &[0x50, 0x51, 0xE4, 0x60, 0x80],
        "AX and CX kept, the scan code read"
    );
    let chain = [0x2E, 0xFF, 0x2E];
    let at = filter
        .windows(3)
        .position(|window| window == chain)
        .unwrap();
    assert_eq!(
        u16::from_le_bytes([filter[at + 3], filter[at + 4]]),
        at::OLD_09,
        "chained through the kept vector"
    );
    let handler = mouse_handler().unwrap();
    assert!(
        handler.len() <= usize::from(at::MOUSE_LEN),
        "{} bytes",
        handler.len()
    );
    assert_eq!(handler.last(), Some(&0xCB), "a far return to the driver");
}

#[test]
fn a_point_falls_in_its_cell() {
    assert_eq!(cell(0, 0), (0, 0));
    assert_eq!(cell(639, 199), (24, 79));
    assert_eq!(cell(8, 16), (2, 1));
}

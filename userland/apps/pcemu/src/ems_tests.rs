use super::*;

#[test]
fn a_window_shows_its_page_once_enabled_and_only_pages_the_board_holds() {
    let mut board = Ems::new(301);
    assert_eq!(board.shown(0), None, "every window empty at power-up");
    assert_eq!(board.output(0, 0x2C), 0);
    assert_eq!(board.shown(0), None, "not yet enabled");
    assert_eq!(board.output(1, 0x81), 0);
    assert_eq!(
        board.shown(0),
        Some(0x12C),
        "the ninth bit from the high byte"
    );
    board.output(7, 0x81);
    board.output(6, 0xFF);
    assert_eq!(board.shown(3), None, "page 511 is past this board's 301");
    assert_eq!(board.shown(4), None, "no fifth window");
}

#[test]
fn the_registers_read_back_without_their_unbuilt_bits() {
    let mut board = Ems::new(MAX_PAGES);
    assert_eq!(board.output(3, 0xFF), 1);
    assert_eq!(board.input(3), 0x81);
    board.output(2, 0xA5);
    assert_eq!(board.input(2), 0xA5);
    assert_eq!(board.input(10), board.input(2), "the ports repeat");
}

#[test]
fn a_register_value_round_trips_and_one_no_register_holds_does_not_fit() {
    assert_eq!(page_of(register(Some(0x1FF)), MAX_PAGES), Some(0x1FF));
    assert_eq!(page_of(register(None), MAX_PAGES), None);
    assert_eq!(
        page_of(register(Some(9)), 9),
        None,
        "a page the board lacks shows nothing"
    );
    assert!(fits(register(Some(0x1FF))));
    assert!(!fits([0, 0x40]));
}

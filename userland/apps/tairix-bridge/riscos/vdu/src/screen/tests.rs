use super::*;

fn screen(mode: u32, size: u32) -> Screen {
    Screen {
        format: Format::numbered(tairix_vdu::mode::numbered(mode).expect("a mode")),
        named: Named::Number(mode),
        frame_rate: 60,
        base: 0x10_0000,
        size,
        vdu_start: 0,
        display_start: 0,
        own_bank: 1,
        tv: [0, 0],
        selector_copy: 0,
        check_copy: 0,
        lent: false,
    }
}

#[test]
fn banks_lie_one_after_another_and_the_second_is_known() {
    let mut s = screen(1, 0x14000);
    assert_eq!(s.banks(), 4);
    s.set_bank(true, 2);
    assert_eq!(s.canvas().start, 0x10_0000 + 20_480);
    assert!(s.writing_second());
    assert_eq!(s.needed(), 2 * 20_480);
    assert_eq!(s.display_start_address(), 0x10_0000, "the display unmoved");
}

#[test]
fn a_hardware_scroll_turns_both_starts_round_the_area() {
    let mut s = screen(1, 0x5000);
    s.hardware_scroll(true, 8);
    assert_eq!((s.vdu_start, s.display_start), (640, 640));
    s.hardware_scroll(false, 8);
    s.hardware_scroll(false, 8);
    assert_eq!(
        s.vdu_start,
        0x5000 - 640,
        "back past the start, round the end"
    );
}

#[test]
fn a_gap_mode_s_bank_is_the_lines_it_shows() {
    let s = screen(3, 0x14000);
    assert_eq!(s.bank_bytes(), 40_000);
    assert_eq!(s.needed(), 40_960, "whole pages");
}

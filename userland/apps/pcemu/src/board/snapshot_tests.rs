use alloc::vec::Vec;

use super::*;
use crate::board::BoardConfig;
use crate::cpu::Bus;
use crate::machine::Template;
use crate::snapshot::Torn;

fn saved(board: &Board) -> Vec<u8> {
    let mut out = Out::refusable();
    board.save(&mut out);
    out.finish().unwrap()
}

/// A board of `config` restored from what `board` keeps.
fn twin(board: &Board, config: &BoardConfig) -> Board {
    let mut twin = Board::new(config).unwrap();
    let bytes = saved(board);
    let mut input = In::new(&bytes);
    twin.restore(&mut input).unwrap();
    assert!(input.is_exhausted());
    twin
}

#[test]
fn an_expanded_memory_window_shows_again_the_page_it_showed() {
    let config = Template::TurboXt.config();
    let mut board = Board::new(&config).unwrap();
    // Page 5 in the first window, page 9 in the second.
    for (port, value) in [(0x258, 5), (0x259, 0x80), (0x25A, 9), (0x25B, 0x80)] {
        board.output(port, value, 0);
    }
    board.write(0xD_0000, 0x5A, 0);
    board.write(0xD_4000, 0xA5, 0);
    let mut twin = twin(&board, &config);
    assert_eq!(twin.read(0xD_0000, 0), 0x5A);
    assert_eq!(twin.read(0xD_4000, 0), 0xA5);
    // The pages behind the windows, shown elsewhere, are the same pages.
    for (port, value) in [(0x25C, 5), (0x25D, 0x80)] {
        twin.output(port, value, 0);
    }
    assert_eq!(twin.read(0xD_8000, 0), 0x5A);
}

#[test]
fn a_display_card_answers_again_in_the_window_it_was_moved_to() {
    let config = Template::TurboXt.config();
    let mut board = Board::new(&config).unwrap();
    // RAM on and every plane written, the planes sequential, then the
    // graphics controller's memory map: A0000h for 64 KiB.
    for (port, value) in [
        (0x3C2, 0x63),
        (0x3C4, 0x02),
        (0x3C5, 0x0F),
        (0x3C4, 0x04),
        (0x3C5, 0x06),
        (0x3CE, 0x05),
        (0x3CF, 0x00),
        (0x3CE, 0x08),
        (0x3CF, 0xFF),
        (0x3CE, 0x06),
        (0x3CF, 0x05),
    ] {
        board.output(port, value, 0);
    }
    board.write(0xA_0010, 0x3C, 0);
    assert_eq!(board.read(0xA_0010, 0), 0x3C);
    let mut twin = twin(&board, &config);
    assert_eq!(twin.read(0xA_0010, 0), 0x3C);
}

#[test]
fn the_80186s_control_block_is_found_again_where_it_was_moved() {
    let config = Template::Workstation186.config();
    let mut board = Board::new(&config).unwrap();
    // The relocation register: memory space, at 80000h.
    board.output_word(0xFFFE, 0x1800, 0);
    let relocation = board.read(0x8_00FF, 0);
    assert_ne!(relocation, 0, "the block, not the memory under it");
    let port = board.input(0xFFFE, 0);
    let mut twin = twin(&board, &config);
    assert_eq!(twin.read(0x8_00FF, 0), relocation, "read through the block");
    assert_eq!(twin.input(0xFFFE, 0), port, "and not through the ports");
}

/// Whether a board of `config` takes back what `board` keeps.
fn takes(board: &Board, config: &BoardConfig) -> Result<(), Torn> {
    let bytes = saved(board);
    Board::new(config).unwrap().restore(&mut In::new(&bytes))
}

#[test]
fn a_board_owed_bus_time_past_a_second_or_its_devices_far_behind_is_refused() {
    let config = Template::Workstation186.config();
    let mut board = Board::new(&config).unwrap();
    board.settle(2 * NANOS_PER_SECOND);
    assert_eq!(takes(&board, &config), Ok(()), "settled, it is taken back");
    let mut owed = Board::new(&config).unwrap();
    owed.settle(2 * NANOS_PER_SECOND);
    owed.stolen = owed.clock.rate().ticks_at(2 * NANOS_PER_SECOND);
    assert_eq!(
        takes(&owed, &config),
        Err(Torn::Malformed),
        "two seconds of bus"
    );
    let mut ahead = Board::new(&config).unwrap();
    ahead.settle(2 * NANOS_PER_SECOND);
    ahead.planar.now = 4 * NANOS_PER_SECOND;
    assert_eq!(
        takes(&ahead, &config),
        Err(Torn::Malformed),
        "every device two seconds behind"
    );
}

#[test]
fn a_board_whose_processor_runs_at_no_processor_s_clock_is_refused() {
    let config = Template::IbmPc.config();
    let mut board = Board::new(&config).unwrap();
    assert_eq!(takes(&board, &config), Ok(()));
    let hundred = Hertz::whole(100_000_000).unwrap();
    board.clock = crate::time::Clock::new(hundred);
    assert_eq!(
        takes(&board, &config),
        Err(Torn::Malformed),
        "a board clock, not a processor's"
    );
}

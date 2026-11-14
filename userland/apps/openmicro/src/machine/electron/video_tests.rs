use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::machine::canvas::CANVAS_WIDTH;
use crate::snapshot::{self, Reader, SnapshotError, Writer};

const WHITE: u8 = encode(7, 0, 3);
const RED: u8 = encode(1, 0, 3);

/// The slot the shown frame holds at display line `line`'s pixel slot
/// `slot`, 0–639.
fn shown_at(video: &Video, line: u64, slot: usize) -> u8 {
    let row = usize::try_from(2 * (line + TOP_EDGE)).unwrap();
    video.shown()[row * CANVAS_WIDTH + 128 + slot]
}

/// A two-colour palette: entry 0 black, entry 8 white.
fn black_and_white(video: &mut Video) {
    // Entry 0's guns off, entry 8's on: blue D4/D6, green D2 in &FE08 and
    // green D4, red D0/D2 in &FE09.
    video.write_palette(0, 0x10 | 0x20 | 0x80);
    video.write_palette(1, 0x01 | 0x02 | 0x08 | 0x10 | 0x20);
}

#[test]
fn each_register_pair_turns_the_guns_of_its_four_entries_off() {
    assert_eq!(colours_of(0, 0), [7; 4], "nothing off: white");
    assert_eq!(colours_of(0xFF, 0xFF), [0; 4], "everything off: black");
    // The first entry's red alone left on.
    let red = colours_of(0xFF, !0x01);
    assert_eq!(red, [1, 0, 0, 0]);
    // The last entry's green alone left on, from the even register's bit 3.
    assert_eq!(colours_of(!0x08, 0xFF), [0, 0, 0, 2]);
    let mut video = Video::new();
    black_and_white(&mut video);
    assert_eq!(video.slots[0], encode(0, 0, 3));
    assert_eq!(video.slots[8], WHITE);
}

#[test]
fn the_bits_that_set_an_entrys_colour_read_back_as_it() {
    for place in 0..4 {
        for colour in 0..8 {
            let (even, odd) = register_bits(place, colour);
            let others = (0..4).filter(|&p| p != place).fold((0, 0), |(e, o), p| {
                let (pe, po) = register_bits(p, 0);
                (e | pe, o | po)
            });
            let read = colours_of(even | others.0, odd | others.1);
            assert_eq!(read[place], colour, "place {place} colour {colour}");
        }
    }
}

#[test]
fn a_pixel_shows_the_entry_its_bits_name_in_every_depth() {
    assert_eq!(entry(0x80, 0, 1), 8);
    assert_eq!(entry(0x80, 1, 1), 0);
    // Two bits a pixel: the high bit from the top nibble, the low from the
    // bottom.
    assert_eq!(entry(0x88, 0, 2), 10);
    assert_eq!(entry(0x08, 0, 2), 2);
    assert_eq!(entry(0x11, 3, 2), 10);
    // Four bits a pixel: bits 7, 5, 3 and 1 for the first.
    assert_eq!(entry(0xAA, 0, 4), 15);
    assert_eq!(entry(0x55, 1, 4), 15);
    assert_eq!(entry(0x80, 0, 4), 8);
}

#[test]
fn ram_waits_out_the_window_in_modes_0_to_3_and_runs_at_1_mhz_in_4_to_6() {
    let mut video = Video::new();
    let line = |n: u64, cycle: u64| n * LINE_CYCLES + cycle;
    assert_eq!(video.ram_wait(line(10, 23)), 0, "before the window");
    assert_eq!(video.ram_wait(line(10, 24)), 80, "its first cycle");
    assert_eq!(video.ram_wait(line(10, 103)), 1);
    assert_eq!(video.ram_wait(line(10, 104)), 0, "after it");
    assert_eq!(video.ram_wait(line(256, 50)), 0, "below the display");
    video.write_mode(4);
    assert_eq!(video.ram_wait(line(10, 50)), 1, "an even cycle");
    assert_eq!(video.ram_wait(line(10, 51)), 2, "an odd one");
    assert_eq!(video.ram_wait(line(300, 51)), 2, "at 1 MHz everywhere");
}

#[test]
fn a_mode_written_shows_from_the_next_line() {
    let mut video = Video::new();
    let ram = vec![0u8; 0x8000];
    video.catch_up(5 * LINE_CYCLES + 40, &ram);
    video.write_mode(5);
    assert_eq!(
        video.mode_at(5 * LINE_CYCLES + 60),
        0,
        "the line in hand keeps its mode"
    );
    assert_eq!(video.mode_at(6 * LINE_CYCLES), 5, "the next takes it");
    assert_eq!(
        video.ram_wait(5 * LINE_CYCLES + 60),
        44,
        "so this line still waits"
    );
    video.catch_up(6 * LINE_CYCLES + 1, &ram);
    assert_eq!(video.mode, 5);
}

#[test]
fn mode_0_shows_each_byte_at_its_cell_through_the_palette() {
    let mut ram = vec![0u8; 0x8000];
    // The first cell's first line, and the second line of the cell beside it.
    ram[0x3000] = 0x81;
    ram[0x3009] = 0xFF;
    let mut video = Video::new();
    black_and_white(&mut video);
    video.catch_up(FRAME_CYCLES, &ram);
    assert_eq!(shown_at(&video, 0, 0), WHITE);
    assert_eq!(shown_at(&video, 0, 1), encode(0, 0, 3));
    assert_eq!(shown_at(&video, 0, 7), WHITE);
    assert!((8..16).all(|slot| shown_at(&video, 1, slot) == WHITE));
    assert_eq!(shown_at(&video, 1, 0), encode(0, 0, 3));
    assert_eq!(video.frames(), 1);
}

#[test]
fn a_slow_mode_shows_each_pixel_twice_as_wide_from_one_fetch() {
    let mut ram = vec![0u8; 0x8000];
    ram[0x5800] = 0xC0;
    let mut video = Video::new();
    black_and_white(&mut video);
    video.write_mode(4);
    video.catch_up(FRAME_CYCLES, &ram);
    assert!((0..4).all(|slot| shown_at(&video, 0, slot) == WHITE));
    assert_eq!(shown_at(&video, 0, 4), encode(0, 0, 3));
}

#[test]
fn a_text_mode_leaves_each_rows_last_two_lines_blank() {
    let mut ram = vec![0xFFu8; 0x8000];
    ram[0x4000..0x4008].fill(0xFF);
    let mut video = Video::new();
    black_and_white(&mut video);
    video.write_mode(3);
    video.catch_up(FRAME_CYCLES, &ram);
    assert_eq!(shown_at(&video, 7, 0), WHITE);
    assert_eq!(shown_at(&video, 8, 0), BLACK, "the gap");
    assert_eq!(shown_at(&video, 9, 0), BLACK);
    assert_eq!(shown_at(&video, 10, 0), WHITE, "the next row");
    assert_eq!(shown_at(&video, 252, 0), BLACK, "past the 25th row");
}

#[test]
fn the_screen_start_is_taken_at_the_frames_start_and_wraps_to_the_base() {
    let mut ram = vec![0u8; 0x8000];
    // With the screen started at &7FC0, its 64th byte is the base's first.
    ram[0x7FC0] = 0xF0;
    ram[0x3000] = 0x0F;
    let mut video = Video::new();
    black_and_white(&mut video);
    video.catch_up(10, &ram);
    // &7FC0: A6–A8 are 7, A9–A14 are &3F.
    video.write_start(false, 0xE0);
    video.write_start(true, 0x3F);
    assert_eq!(video.start_written, 0x7FC0);
    video.catch_up(FRAME_CYCLES, &ram);
    assert_eq!(shown_at(&video, 0, 4), WHITE, "this frame kept its start");
    video.catch_up(2 * FRAME_CYCLES, &ram);
    assert_eq!(shown_at(&video, 0, 0), WHITE, "the next took &7FC0");
    assert_eq!(shown_at(&video, 0, 4), BLACK);
    // Cell 8 of row 0 is &7FC0 + 64, past the top: the base's first byte.
    assert_eq!(shown_at(&video, 0, 64 + 4), WHITE);
}

#[test]
fn a_palette_written_mid_line_changes_the_rest_of_it() {
    let ram = vec![0xFFu8; 0x8000];
    let mut video = Video::new();
    black_and_white(&mut video);
    video.catch_up(LINE_CYCLES * 3 + 64, &ram);
    // Entry 8 to red: its green and blue off as well as entry 0's guns.
    video.write_palette(0, 0x10 | 0x20 | 0x80 | 0x40 | 0x04);
    video.catch_up(FRAME_CYCLES, &ram);
    let first = usize::try_from((64 - 24) * 8).unwrap();
    assert_eq!(shown_at(&video, 3, first - 1), WHITE);
    assert_eq!(shown_at(&video, 3, first), RED);
    assert_eq!(
        shown_at(&video, 2, first),
        WHITE,
        "the line above was drawn"
    );
}

fn saved(video: &Video) -> Vec<u8> {
    let mut w = Writer::new();
    w.put(video);
    w.finish().unwrap()
}

fn restored(saved: &[u8], clock: u64) -> Result<Video, SnapshotError> {
    let body = snapshot::open(saved)?;
    let mut r = Reader::new(&body);
    r.set_clock(clock)?;
    r.get()
}

#[test]
fn a_restored_display_draws_on_as_the_one_it_was_taken_from() {
    let mut ram = vec![0u8; 0x8000];
    ram[0x3000..0x3100].fill(0x55);
    let mut original = Video::new();
    black_and_white(&mut original);
    original.write_mode(1);
    let at = FRAME_CYCLES + 77 * LINE_CYCLES + 33;
    original.catch_up(at, &ram);
    let mut copy = restored(&saved(&original), at).unwrap();
    assert_eq!(
        copy.take_dirty().rows().count(),
        crate::machine::canvas::CANVAS_HEIGHT
    );
    original.take_dirty();
    for video in [&mut original, &mut copy] {
        video.write_palette(3, 0x12);
        video.catch_up(3 * FRAME_CYCLES, &ram);
    }
    assert_eq!(saved(&original), saved(&copy));
    assert_eq!(
        restored(&saved(&original), at).err(),
        Some(SnapshotError::Malformed),
        "not drawn to its time"
    );
    let mut odd = original.clone();
    odd.start = 0x0001;
    assert_eq!(
        restored(&saved(&odd), 3 * FRAME_CYCLES).err(),
        Some(SnapshotError::Malformed),
        "a start the registers cannot hold"
    );
}

use alloc::vec;
use alloc::vec::Vec;

use super::{reg, screen_address, Signal, Video, CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::canvas::shows_dot;
use crate::snapshot::{self, Reader, SnapshotError, Writer};

/// The 6845 registers and ULA control the MOS programs for mode 7.
const MODE_7_CRTC: [u8; 14] = [
    0x3F, 0x28, 0x33, 0x24, 0x1E, 0x02, 0x19, 0x1C, 0x93, 0x12, 0x72, 0x13, 0x28, 0x00,
];
const MODE_7_ULA: u8 = 0x4B;

/// One field at 1 MHz: 312.5 lines of 64 characters, two cycles each.
const FIELD_CYCLES: u64 = 40_000;

fn set_crtc(video: &mut Video, register: usize, value: u8) {
    video.write_crtc(false, u8::try_from(register).unwrap_or(0));
    video.write_crtc(true, value);
}

fn mode_7(video: &mut Video) {
    for (register, &value) in MODE_7_CRTC.iter().enumerate() {
        set_crtc(video, register, value);
    }
    video.write_ula(false, MODE_7_ULA);
}

fn lit_rows(video: &Video) -> Vec<usize> {
    video
        .shown()
        .as_chunks::<CANVAS_WIDTH>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, row)| row.iter().any(|&slot| shows_dot(slot)))
        .map(|(index, _)| index)
        .collect()
}

#[test]
fn teletext_addressing_reads_the_one_kilobyte_screen() {
    assert_eq!(screen_address(0x2800, 0, 0x4000), 0x7C00);
    assert_eq!(screen_address(0x2000, 0, 0x4000), 0x3C00);
    assert_eq!(screen_address(0x2BFF, 9, 0x4000), 0x7FFF);
}

#[test]
fn hardware_scrolled_addresses_wrap_back_to_the_screen() {
    // Mode 0's 20K screen: &8050 wraps to &3050.
    assert_eq!(screen_address(0x100A, 0, 0x3000), 0x3050);
    assert_eq!(screen_address(0x0600, 3, 0x3000), 0x3003);
}

#[test]
fn mode_7_text_is_drawn_on_the_rows_it_occupies() {
    let mut ram = vec![0x20u8; 0x8000];
    ram[0x7C00 + 40..0x7C00 + 44].copy_from_slice(b"ABCD");
    let mut video = Video::new();
    mode_7(&mut video);
    let mut edges = 0;
    video.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |_, signal| {
        if matches!(signal, Signal::Vsync(_)) {
            edges += 1;
        }
    });
    assert!(
        video.fields() >= 3 && video.fields() <= 5,
        "fields: {}",
        video.fields()
    );
    assert!(edges >= 6, "VSYNC rose and fell each field: {edges}");
    let rows = lit_rows(&video);
    assert!(!rows.is_empty(), "the text on row 1 lit nothing");
    // A character row is twenty canvas rows; the lit ones lie in one row's span.
    let span = rows.last().copied().unwrap_or(0) - rows.first().copied().unwrap_or(0);
    assert!(span < 20, "lit rows {rows:?}");
    assert!(rows.iter().all(|&row| row < CANVAS_HEIGHT));
}

/// The two fields interleave in the order of the SAA5050's half-lines: the
/// field of odd raster lines, which round toward the row below, lies below
/// the other, so an 'A' widens down from its apex as its half-lines do.
#[test]
fn mode_7_fields_interleave_in_the_order_of_the_characters_half_lines() {
    let mut ram = vec![0x20u8; 0x8000];
    ram[0x7C00 + 40] = b'A';
    let mut video = Video::new();
    mode_7(&mut video);
    video.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    let widths: Vec<usize> = video
        .shown()
        .as_chunks::<CANVAS_WIDTH>()
        .0
        .iter()
        .map(|row| row.iter().filter(|&&slot| shows_dot(slot)).count())
        .collect();
    let apex = widths
        .iter()
        .position(|&lit| lit > 0)
        .expect("the A is drawn");
    // Its apex dot, then rounded half a dot wider each side, then the two
    // strokes with the gap between them filled.
    assert_eq!(
        widths[apex..apex + 3],
        [3, 6, 9],
        "{:?}",
        &widths[apex..apex + 8]
    );
}

#[test]
fn mode_7_draws_the_first_column_under_its_own_cursor_position() {
    let mut ram = vec![0x20u8; 0x8000];
    ram[0x7C00 + 40] = b'H';
    let mut video = Video::new();
    mode_7(&mut video);
    video.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    let first_lit = video
        .shown()
        .as_chunks::<CANVAS_WIDTH>()
        .0
        .iter()
        .filter_map(|row| row.iter().position(|&slot| shows_dot(slot)))
        .min();
    // Character 0 of a line starts 13 µs after HSYNC, the canvas 8 µs after
    // it, and the SAA5050's dots trail the fetch by three characters: 128.
    assert_eq!(first_lit, Some(128));
}

#[test]
fn the_light_pen_strobes_over_lit_slots_and_latches_the_address_ahead_of_it() {
    let mut ram = vec![0x20u8; 0x8000];
    // A solid mosaic block in the middle of row 1.
    ram[0x7C00 + 40 + 9] = 0x97;
    ram[0x7C00 + 40 + 10] = 0xFF;
    let mut video = Video::new();
    mode_7(&mut video);
    video.catch_up(2 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    let (column, row) = lit_slot(&video).expect("the block is drawn");
    video.set_light_pen(Some((column, row)));
    let mut strobes = Vec::new();
    video.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |cycle, signal| {
        if let Signal::LightPen(level) = signal {
            strobes.push((cycle, level));
        }
    });
    assert!(strobes.len() >= 4, "{strobes:?}");
    assert!(strobes
        .chunks(2)
        .all(|pair| pair[0].1 && !pair[1].1 && pair[1].0 - pair[0].0 == 4));
    let latched = u16::from_be_bytes([video.crtc().register(16), video.crtc().register(17)]);
    // The dots trail the 6845 by three characters and the pen answers 3 µs
    // after them: six characters past the block, row 1 column 10.
    assert_eq!(latched, 0x2800 + 40 + 10 + 6);

    let mut dark = Video::new();
    mode_7(&mut dark);
    dark.set_light_pen(Some((0, 0)));
    let mut any = false;
    dark.catch_up(2 * FIELD_CYCLES, &ram, 0x4000, |_, signal| {
        any |= matches!(signal, Signal::LightPen(_));
    });
    assert!(!any, "a pen over black never strobes");
}

/// The first lit slot of the shown picture, as a column and row.
fn lit_slot(video: &Video) -> Option<(usize, usize)> {
    video
        .shown()
        .iter()
        .position(|&slot| slot >> 6 != 0 && slot & 7 != 0)
        .map(|at| (at % CANVAS_WIDTH, at / CANVAS_WIDTH))
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
fn a_restored_picture_draws_on_as_the_one_it_was_taken_from_through_a_pen_strobe() {
    let mut ram = vec![0x20u8; 0x8000];
    ram[0x7C00 + 40 + 9] = 0x97;
    ram[0x7C00 + 40 + 10] = 0xFF;
    let started = || {
        let mut video = Video::new();
        mode_7(&mut video);
        video.catch_up(2 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
        let pen = lit_slot(&video);
        video.set_light_pen(pen);
        video
    };
    let mut probe = started();
    let mut rise = None;
    probe.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |cycle, signal| {
        if signal == Signal::LightPen(true) {
            rise.get_or_insert(cycle);
        }
    });
    let rise = rise.expect("the pen strobes");
    // From before the beam reaches the pen to after the strobe falls.
    for at in rise - 8..rise + 8 {
        let mut original = started();
        original.catch_up(at, &ram, 0x4000, |_, _| {});
        let mut copy = restored(&saved(&original), at).unwrap();
        assert_eq!(copy.shown(), original.shown());
        assert!(
            copy.take_dirty().rows().count() == CANVAS_HEIGHT,
            "a restored picture is all new"
        );
        let mut signals = [Vec::new(), Vec::new()];
        for (video, signals) in [&mut original, &mut copy].into_iter().zip(&mut signals) {
            video.catch_up(at + FIELD_CYCLES, &ram, 0x4000, |cycle, signal| {
                signals.push((cycle, signal));
            });
        }
        assert!(!signals[0].is_empty());
        assert_eq!(signals[0], signals[1], "from {at}");
        assert!(saved(&original) == saved(&copy), "from {at}");
    }
    assert_eq!(
        restored(&saved(&probe), 0).err(),
        Some(SnapshotError::Malformed),
        "drawn past the clock"
    );
    let drawn = 4 * FIELD_CYCLES;
    assert!(restored(&saved(&probe), drawn).is_ok());
    assert_eq!(
        restored(&saved(&probe), drawn + 2).err(),
        Some(SnapshotError::Malformed),
        "drawn short of it"
    );
}

/// Catch the picture up one event at a time to `until`, recording each
/// signal with the event it fell in.
fn by_event(video: &mut Video, ram: &[u8], until: u64) -> (usize, Vec<(u64, u64, Signal)>) {
    let (mut wakes, mut seen) = (0, Vec::new());
    while video.cycle < until {
        let event = video.next_event();
        video.catch_up(event + 1, ram, 0x4000, |cycle, signal| {
            seen.push((event, cycle, signal));
        });
        wakes += 1;
    }
    (wakes, seen)
}

/// The regression: an odd field's VSYNC, half way along a line, was named a
/// character early, and the pen's strobe not until it was drawn, so their
/// interrupts waited for the next line to start.
#[test]
fn every_sync_and_pen_change_lands_on_a_cycle_the_video_named() {
    let mut ram = vec![0x20u8; 0x8000];
    ram[0x7C00 + 40 + 9] = 0x97;
    ram[0x7C00 + 40 + 10] = 0xFF;
    let mut video = Video::new();
    mode_7(&mut video);
    video.catch_up(2 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    video.set_light_pen(lit_slot(&video));
    let (_, seen) = by_event(&mut video, &ram, 6 * FIELD_CYCLES);
    let rises = |wanted: Signal| {
        seen.iter()
            .filter(|&&(_, _, signal)| signal == wanted)
            .count()
    };
    assert!(
        rises(Signal::Vsync(true)) >= 4,
        "both fields' syncs: {seen:?}"
    );
    assert!(
        rises(Signal::LightPen(true)) >= 2,
        "the pen strobes: {seen:?}"
    );
    for (event, cycle, signal) in seen {
        assert_eq!(cycle, event, "{signal:?}");
    }
}

/// The regression: a line's middle was an event on every line, so the
/// machine woke twice a line where an odd field's sync can start once a
/// field.
#[test]
fn a_field_wakes_the_machine_once_a_line() {
    let ram = vec![0x20u8; 0x8000];
    let mut video = Video::new();
    mode_7(&mut video);
    video.catch_up(2 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    let (wakes, _) = by_event(&mut video, &ram, 4 * FIELD_CYCLES);
    // Two fields of 312½ lines, and the odd field's sync half way along one.
    assert!((625..=630).contains(&wakes), "{wakes}");
}

/// The regression: every field compared and copied the whole canvas,
/// however little was drawn into it.
#[test]
fn a_field_touches_only_the_rows_drawn_differently() {
    let mut ram = vec![0x20u8; 0x8000];
    let mut video = Video::new();
    mode_7(&mut video);
    video.catch_up(3 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    video.take_dirty();
    video.catch_up(5 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    assert_eq!(
        video.take_dirty().rows().count(),
        0,
        "a still picture changes nothing"
    );
    ram[0x7C00 + 40 + 5] = b'X';
    video.catch_up(6 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    let dirty: Vec<usize> = video.take_dirty().rows().collect();
    assert!(
        !dirty.is_empty() && dirty.len() <= 20,
        "one character row: {dirty:?}"
    );
}

/// The regression: rows a field's beam never reached kept an earlier
/// field's picture, so a frame ending above the canvas's foot left a strip
/// of an old screen below it.
#[test]
fn a_field_ending_early_leaves_nothing_below_it() {
    let ram = vec![b'H'; 0x8000];
    let mut video = Video::new();
    mode_7(&mut video);
    // Every row displayed, in fields of 320 lines, which reach the foot.
    set_crtc(&mut video, reg::V_DISPLAYED, 31);
    set_crtc(&mut video, reg::V_ADJUST, 10);
    video.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    // Mode 7's own fields of 312 lines end four lines short of it.
    let unreached = CANVAS_HEIGHT - 2 * 4;
    assert!(
        lit_rows(&video).iter().any(|&row| row >= unreached),
        "the long fields reach the foot"
    );
    set_crtc(&mut video, reg::V_ADJUST, MODE_7_CRTC[reg::V_ADJUST]);
    video.catch_up(8 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    let rows = lit_rows(&video);
    assert!(rows.iter().all(|&row| row < unreached), "{rows:?}");
    assert!(rows.iter().any(|&row| row >= unreached - 10), "{rows:?}");
}

/// The regression: the part of a line its sync cut short kept what an
/// earlier, longer line drew there.
#[test]
fn a_line_ending_early_leaves_nothing_beside_it() {
    let ram = vec![b'H'; 0x8000];
    let lit_beyond = |video: &Video, column: usize| {
        video
            .shown()
            .as_chunks::<CANVAS_WIDTH>()
            .0
            .iter()
            .any(|row| row[column..].iter().any(|&slot| shows_dot(slot)))
    };
    let mut video = Video::new();
    mode_7(&mut video);
    // Characters all the way along lines of 64 µs, which reach the right
    // edge.
    set_crtc(&mut video, reg::H_DISPLAYED, 64);
    video.catch_up(4 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    // Lines of 56 µs end the canvas's last 8 µs, 128 slots, short.
    let unreached = CANVAS_WIDTH - 128;
    assert!(
        lit_beyond(&video, unreached),
        "the long lines reach the edge"
    );
    set_crtc(&mut video, reg::H_TOTAL, 55);
    video.catch_up(8 * FIELD_CYCLES, &ram, 0x4000, |_, _| {});
    assert!(
        !lit_beyond(&video, unreached),
        "nothing past the short lines"
    );
    assert!(
        lit_beyond(&video, unreached - 16),
        "the short lines are drawn"
    );
}

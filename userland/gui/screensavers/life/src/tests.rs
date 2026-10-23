//! Host tests of the Game of Life: the bit-sliced generation against a
//! plain one, the torus, colour inheritance, reseeding, and the repaint.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::saver_ipc::{Start, SAVER_DAMAGE_RECTS};
use tairix_abi::Errno;
use tairix_geometry::Point;
use tairix_raster::Color;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_saver::testing::{self, Screen};
use tairix_saver::NextFrame;

use super::{
    queued_in, Life, FAMILIES, GENERATIONS_PER_SECOND, MAX_CELLS, QUIET_LIMIT, SETTLED_GRACE,
};

/// The start the host would send for a `size` screen with these options.
fn start(size: (u32, u32), cells: &str, speed: &str) -> Start {
    let mut given = testing::start(size.0, size.1);
    testing::give(&mut given, "cells", testing::choice(cells));
    testing::give(&mut given, "speed", testing::choice(speed));
    given
}

fn made(given: &Start) -> Result<Life, Errno> {
    Life::new(&testing::setup(given, 0))
}

/// The start for a world `cols` by `rows` medium cells.
fn world_start(cols: u32, rows: u32) -> Start {
    start((cols * 8, rows * 8), "medium", "normal")
}

/// A world `cols` by `rows` cells, emptied: no cell alive, lit, or waiting
/// to be drawn.
fn world(cols: u32, rows: u32) -> Life {
    let mut life = made(&world_start(cols, rows)).expect("a world");
    let size = |n: u32| usize::try_from(n).expect("small");
    assert_eq!((life.cols, life.rows), (size(cols), size(rows)));
    life.board.fill(0);
    life.level.fill(0);
    life.queued.fill(0);
    life.active.clear();
    life
}

fn set(life: &mut Life, cells: &[(usize, usize)]) {
    life.board.fill(0);
    for &(row, col) in cells {
        life.board[row * life.words + col / 64] |= 1 << (col % 64);
    }
}

fn living(life: &Life) -> Vec<(usize, usize)> {
    let mut cells = Vec::new();
    for row in 0..life.rows {
        for col in 0..life.cols {
            if life.alive(row, col) {
                cells.push((row, col));
            }
        }
    }
    cells
}

/// Conway's rule applied cell by cell, the torus wrapping round: the
/// reference the bit-sliced generation is held to.
fn reference(life: &Life) -> Vec<(usize, usize)> {
    let mut next = Vec::new();
    for row in 0..life.rows {
        for col in 0..life.cols {
            let mut count = 0;
            for dr in [life.rows - 1, 0, 1] {
                for dc in [life.cols - 1, 0, 1] {
                    if (dr, dc) != (0, 0)
                        && life.alive((row + dr) % life.rows, (col + dc) % life.cols)
                    {
                        count += 1;
                    }
                }
            }
            if count == 3 || (count == 2 && life.alive(row, col)) {
                next.push((row, col));
            }
        }
    }
    next
}

/// Draw `life`'s frame at `now_ns`, answering the instant it wants the next.
fn draw(screen: &mut Screen, life: &mut Life, now_ns: u64) -> u64 {
    let (next, asked) = screen.draw(life, 0, now_ns);
    assert!(asked.is_empty(), "the world asks nothing of its host");
    let NextFrame::At(due) = next else {
        panic!("a world always wants another frame: {next:?}");
    };
    due
}

/// Random boards whose widths fall inside a word, on one, and across two:
/// every generation equals the cell-by-cell rule, torus and word seams
/// included.
#[test]
fn the_bit_sliced_generation_is_conways_rule() {
    let mut rng = NonCryptoRng::seed_from_u64(7);
    for (cols, rows) in [(5, 4), (63, 9), (64, 7), (65, 6), (70, 12), (129, 5)] {
        let mut life = world(cols, rows);
        for _ in 0..8 {
            let cells: Vec<(usize, usize)> = (0..life.rows)
                .flat_map(|row| (0..life.cols).map(move |col| (row, col)))
                .filter(|_| rng.next_below(100) < 35)
                .collect();
            set(&mut life, &cells);
            for _ in 0..6 {
                let expected = reference(&life);
                life.generation();
                assert_eq!(living(&life), expected, "{cols}x{rows}");
            }
        }
    }
}

#[test]
fn a_blinker_turns_and_turns_back() {
    let mut life = world(16, 16);
    set(&mut life, &[(5, 4), (5, 5), (5, 6)]);
    life.generation();
    assert_eq!(living(&life), vec![(4, 5), (5, 5), (6, 5)]);
    life.generation();
    assert_eq!(living(&life), vec![(5, 4), (5, 5), (5, 6)]);
}

/// A glider leaving the right edge arrives on the left, and one crossing a
/// word boundary is not torn by it.
#[test]
fn a_glider_crosses_the_word_seam_and_the_torus_edge() {
    let mut life = world(70, 10);
    let glider = |row: usize, col: usize| {
        vec![
            (row, col + 1),
            (row + 1, col + 2),
            (row + 2, col),
            (row + 2, col + 1),
            (row + 2, col + 2),
        ]
    };
    set(&mut life, &glider(2, 61));
    for _ in 0..4 {
        life.generation();
    }
    assert_eq!(living(&life), {
        let mut moved = glider(3, 62);
        moved.sort_unstable();
        moved
    });
    // Twelve more periods carry it past column 69 and round to the left.
    for _ in 0..48 {
        life.generation();
    }
    assert_eq!(living(&life).len(), 5, "a glider, whole");
    assert!(living(&life).iter().any(|&(_, col)| col < 5));
}

/// A newborn takes the colour at least two of its parents share, or the one
/// none of them has when all three differ.
#[test]
fn a_newborn_takes_its_parents_colour() {
    let mut life = world(16, 16);
    let parents = [(4, 4), (4, 5), (4, 6)];
    set(&mut life, &parents);
    let family = |life: &mut Life, cell: (usize, usize), family: u8| {
        let at = cell.0 * life.cols + cell.1;
        life.family[at] = family;
    };
    family(&mut life, parents[0], 2);
    family(&mut life, parents[1], 2);
    family(&mut life, parents[2], 1);
    assert_eq!(life.inherited_family(3, 5), 2, "the shared colour");
    family(&mut life, parents[0], 0);
    family(&mut life, parents[1], 1);
    family(&mut life, parents[2], 3);
    assert_eq!(life.inherited_family(3, 5), 2, "the one none has");
    assert!(usize::from(life.inherited_family(3, 5)) < FAMILIES);
}

/// A world that has settled into stillness is reseeded once its grace runs
/// out, and not before.
#[test]
fn a_still_world_is_reseeded_after_its_grace() {
    let mut life = world(40, 30);
    set(&mut life, &[(5, 5), (5, 6), (6, 5), (6, 6)]);
    life.recorded = 0;
    life.quiet = 0;
    life.settled = 0;
    // A block never changes: its digest repeats from the second generation,
    // and the grace counts from there.
    for _ in 0..SETTLED_GRACE {
        life.generation();
    }
    assert_eq!(living(&life).len(), 4, "not before its grace is out");
    life.generation();
    assert!(living(&life).len() > 4, "reseeded");
    const {
        assert!(
            SETTLED_GRACE < QUIET_LIMIT,
            "a cycle is caught before quiet is"
        );
    }
}

#[test]
fn a_vast_screen_grows_its_cells_rather_than_its_board() {
    let life = made(&start((15_360, 8_640), "medium", "normal")).expect("a world");
    let cells = u64::try_from(life.cols * life.rows).expect("small");
    assert!(cells <= MAX_CELLS, "{cells}");
    assert!(life.cell > 8);
    assert_eq!(
        made(&start((16, 16), "medium", "normal")).map(|_| ()),
        Err(Errno::OutOfRange),
        "no room for a world"
    );
}

/// The world is built only from what the host sends: an option it did not
/// send, or a value the manifest never declared, starts nothing.
#[test]
fn a_world_needs_both_of_its_options_as_declared() {
    let mut no_speed = testing::start(640, 480);
    testing::give(&mut no_speed, "cells", testing::choice("small"));
    assert_eq!(made(&no_speed).map(|_| ()), Err(Errno::NotFound));
    let odd = start((640, 480), "medium", "ludicrous");
    assert_eq!(made(&odd).map(|_| ()), Err(Errno::OutOfRange));
}

/// Under reduced motion a cell reaches its look in the frame it changes in:
/// a calm world has nothing left fading after one frame.
#[test]
fn a_calm_world_is_born_and_dies_at_once() {
    let mut calm = world_start(24, 16);
    calm.reduced_motion = true;
    let mut life = made(&calm).expect("a world");
    let mut screen = Screen::new(calm);
    draw(&mut screen, &mut life, 0);
    assert!(life.active.is_empty(), "the soup is drawn whole at once");
    set(&mut life, &[(5, 4), (5, 5), (5, 6)]);
    for cell in [(5usize, 4usize), (5, 5), (5, 6)] {
        life.level[cell.0 * life.cols + cell.1] = u8::MAX;
    }
    life.generation();
    life.fade();
    life.retire_settled();
    assert!(life.active.is_empty(), "no cell is left mid-fade");
}

/// A frame repaints its changing cells alone and names the tiles they lie
/// in: a mark in a still cell far from them survives the frame, outside the
/// damage.
#[test]
fn a_frame_repaints_only_its_changing_cells() {
    let mut life = world(40, 30);
    let mut screen = Screen::new(world_start(40, 30));
    // Rows of cells at the top and the bottom, a still cell midway between.
    let rows = [0, 2, life.rows - 3, life.rows - 1];
    let changing: Vec<(usize, usize)> = rows
        .iter()
        .flat_map(|&row| (0..life.cols).step_by(2).map(move |col| (row, col)))
        .collect();
    set(&mut life, &changing);
    for &(row, col) in &changing {
        life.queue(row * life.cols + col);
    }
    let still = life.cell_rect(life.rows / 2, life.cols / 2);
    let (x, y) = (
        u32::try_from(still.left()).expect("on screen") + life.cell / 2,
        u32::try_from(still.top()).expect("on screen") + life.cell / 2,
    );
    screen.surface.fill_rect(x, y, 1, 1, Color::rgb(1, 2, 3));
    draw(&mut screen, &mut life, 0);
    assert!(screen.damage.rects().len() <= SAVER_DAMAGE_RECTS);
    for &(row, col) in &changing {
        let cell = life.cell_rect(row, col);
        assert!(screen.damage.contains(cell.origin), "{row}, {col} named");
    }
    let at = Point::new(
        i32::try_from(x).expect("small"),
        i32::try_from(y).expect("small"),
    );
    assert!(!screen.damage.contains(at), "the still cell is not named");
    let pixel = screen.surface.get(x, y).expect("in bounds");
    assert_eq!(
        (pixel.r, pixel.g, pixel.b),
        (1, 2, 3),
        "the still cell was repainted"
    );
}

/// Once every cell has reached its look, a frame repaints nothing and wants
/// no frame before the next generation.
#[test]
fn a_settled_board_repaints_nothing_and_waits_for_its_next_generation() {
    let mut life = world(24, 16);
    let mut screen = Screen::new(world_start(24, 16));
    set(&mut life, &[(5, 5), (5, 6), (6, 5), (6, 6)]);
    for cell in [(5usize, 5usize), (5, 6), (6, 5), (6, 6)] {
        let at = cell.0 * life.cols + cell.1;
        life.queue(at);
    }
    let mut now = 0;
    while !life.active.is_empty() {
        now = draw(&mut screen, &mut life, now);
    }
    // A block never changes, so its next generation changes nothing either.
    let generation = life.next_generation_ns;
    let due = draw(&mut screen, &mut life, generation);
    assert!(
        screen.damage.is_empty(),
        "nothing changed, nothing repainted"
    );
    assert_eq!(due, life.next_generation_ns, "nothing until the next one");
    assert!(due > generation);
}

/// Smaller cells make a finer board, larger a coarser one.
#[test]
fn the_cell_size_decides_how_fine_the_board_is() {
    let columns = |cells: &str| {
        made(&start((1_280, 720), cells, "normal"))
            .expect("a world")
            .cols
    };
    let (small, medium, large) = (columns("small"), columns("medium"), columns("large"));
    assert!(small > medium && medium > large, "{small} {medium} {large}");
}

/// A world runs as many generations a second as its speed asks on every
/// display, however many of the display's frames a generation spans, and a
/// birth still completes before the next generation comes.
#[test]
fn the_speed_paces_the_generations_on_every_display() {
    const SECONDS: u64 = 10;
    for mhz in [
        24_000, 50_000, 59_940, 60_000, 75_000, 120_000, 144_000, 165_000, 240_000,
    ] {
        let refresh = RefreshRate::from_mhz(mhz).expect("in bounds");
        for (speed, percent) in [("slow", 50), ("normal", 100), ("fast", 150)] {
            let mut given = start((24 * 8, 16 * 8), "medium", speed);
            given.refresh = refresh;
            let mut life = made(&given).expect("a world");
            let mut screen = Screen::new(given);
            let fewest_frames = (life.cadence.generation_ns / life.cadence.frame_ns).max(1);
            assert!(
                u64::from(life.cadence.steps.0) * fewest_frames >= 255,
                "{mhz} {speed}: a newborn is still arriving when the next generation comes"
            );
            let (mut now, mut generations) = (0, 0u64);
            while now < SECONDS * 1_000_000_000 {
                let due = life.next_generation_ns;
                now = draw(&mut screen, &mut life, now);
                generations += u64::from(life.next_generation_ns != due);
            }
            let owed = GENERATIONS_PER_SECOND * percent / 100 * SECONDS;
            assert!(
                generations.abs_diff(owed) <= 1,
                "{mhz} {speed}: {generations} generations of {owed}"
            );
        }
    }
}

/// A generation missed outright, the saver having been held off the CPU, is
/// dropped rather than made up in a burst.
#[test]
fn a_stalled_world_resumes_its_pace_without_a_burst() {
    let mut life = world(24, 16);
    let mut screen = Screen::new(world_start(24, 16));
    let interval = life.cadence.generation_ns;
    let stalled = 50 * interval + interval / 2;
    let due = life.next_generation_ns;
    let next = draw(&mut screen, &mut life, stalled);
    assert_ne!(life.next_generation_ns, due, "the late generation runs");
    assert_eq!(life.next_generation_ns, stalled + interval);
    assert_eq!(next, stalled + interval, "an empty world waits for it");
    let (due, frame) = (life.next_generation_ns, life.cadence.frame_ns);
    draw(&mut screen, &mut life, stalled + frame);
    assert_eq!(life.next_generation_ns, due, "the next waits its interval");
}

/// Changing cells painted in bands of whole cell rows across a runner paint
/// exactly what one pass paints.
#[test]
fn changing_cells_painted_in_bands_match_one_pass() {
    let mut life = world(64, 48);
    let mut rng = NonCryptoRng::seed_from_u64(5);
    let mut changing = Vec::new();
    for _ in 0..1_500 {
        let (row, col) = (
            usize::try_from(rng.next_u64() % 48).expect("small"),
            usize::try_from(rng.next_u64() % 64).expect("small"),
        );
        changing.push((row, col));
        life.queue(row * life.cols + col);
    }
    set(&mut life, &changing);
    life.fade();
    let given = world_start(64, 48);
    let (mut serial, mut banded) = (Screen::new(given.clone()), Screen::new(given));
    life.repaint_active(&mut serial.surface, &tairix_parallel::SERIAL);
    let runner = tairix_parallel::Reversed::new(7);
    life.repaint_active(&mut banded.surface, &runner);
    assert!(runner.widest() > 1, "the cells were split");
    assert_eq!(serial.surface, banded.surface);
}

/// The queued cells of a range are exactly the marked ones a scan of every
/// cell finds, whichever words the range starts and ends in.
#[test]
fn a_ranges_queued_cells_are_its_marked_ones() {
    let mut rng = NonCryptoRng::seed_from_u64(11);
    let queued: Vec<u64> = (0..5).map(|_| rng.next_u64()).collect();
    let marked = |cell: usize| (queued[cell / 64] >> (cell % 64)) & 1 == 1;
    let edges = [0, 1, 63, 64, 65, 127, 128, 200, 319, 320];
    for start in edges {
        for end in edges.into_iter().filter(|end| *end >= start) {
            let found: Vec<usize> = queued_in(&queued, start..end).collect();
            let expected: Vec<usize> = (start..end).filter(|cell| marked(*cell)).collect();
            assert_eq!(found, expected, "{start}..{end}");
        }
    }
}

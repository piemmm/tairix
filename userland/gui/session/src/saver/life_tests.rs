//! Host tests of the Game of Life: the bit-sliced generation against a
//! plain one, the torus, colour inheritance, reseeding, and the repaint.

use alloc::vec;
use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_wallpaper::{CellSize, LifeOptions, Pace};
use tairix_wm::{Compositor, Point, Scale, Surface, WindowId};

use super::{Life, FAMILIES, GENERATIONS_PER_SECOND, MAX_CELLS, QUIET_LIMIT, SETTLED_GRACE};
use crate::tests::compositor;
use tairix_theme::motion::SceneClock;

/// A world `cols` by `rows` cells, emptied: no cell alive, lit, or waiting
/// to be drawn.
fn world(cols: u32, rows: u32) -> Life {
    let mut life = Life::new(
        (cols * 8, rows * 8),
        Scale::ONE,
        (false, LifeOptions::default()),
        0,
    )
    .expect("a world");
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
    let life = Life::new(
        (15_360, 8_640),
        Scale::ONE,
        (false, LifeOptions::default()),
        0,
    )
    .expect("a world");
    let cells = u64::try_from(life.cols * life.rows).expect("small");
    assert!(cells <= MAX_CELLS, "{cells}");
    assert!(life.cell > 8);
    assert!(
        Life::new((16, 16), Scale::ONE, (false, LifeOptions::default()), 0).is_none(),
        "no room for a world"
    );
}

fn canvas(comp: &mut Compositor, life: &Life) -> WindowId {
    let mut black = Surface::new(life.size.0, life.size.1).expect("a surface");
    black.fill(tairix_wm::Color::rgb(0, 0, 0));
    comp.add_window(Point::new(0, 0), black)
}

/// Under reduced motion a cell reaches its look in the frame it changes in:
/// a calm world has nothing left fading after one frame.
#[test]
fn a_calm_world_is_born_and_dies_at_once() {
    let mut comp = compositor();
    let mut life = Life::new(
        (24 * 8, 16 * 8),
        Scale::ONE,
        (true, LifeOptions::default()),
        0,
    )
    .expect("a world");
    let wm = canvas(&mut comp, &life);
    life.advance(0, wm, &mut comp);
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

/// However coarsely a frame's damage is kept, it repaints the changing cells
/// alone: with more changing than the rectangle budget holds, a mark in a
/// still cell between them survives the frame.
#[test]
fn a_frame_repaints_only_its_changing_cells() {
    let mut comp = compositor();
    let mut life = world(40, 30);
    // Apart from each other, so each is a rectangle of its own and together
    // they pass the budget.
    let rows = [0, 2, life.rows - 3, life.rows - 1];
    let changing: Vec<(usize, usize)> = rows
        .iter()
        .flat_map(|&row| (0..life.cols).step_by(2).map(move |col| (row, col)))
        .collect();
    assert!(changing.len() > super::DAMAGE_BUDGET);
    set(&mut life, &changing);
    for &(row, col) in &changing {
        life.queue(row * life.cols + col);
    }
    let wm = canvas(&mut comp, &life);
    let still = life.cell_rect(life.rows / 2, life.cols / 2);
    let (x, y) = (
        u32::try_from(still.left()).expect("on screen") + life.cell / 2,
        u32::try_from(still.top()).expect("on screen") + life.cell / 2,
    );
    let mut spot = tairix_wm::Region::new();
    spot.add(tairix_wm::Rect::new(
        i32::try_from(x).expect("small"),
        i32::try_from(y).expect("small"),
        1,
        1,
    ));
    assert!(comp.repaint_window(wm, life.size, &spot, |surface, _| {
        surface.fill_rect(x, y, 1, 1, tairix_wm::Color::rgb(1, 2, 3));
    }));
    life.frames = 0;
    life.advance(0, wm, &mut comp);
    let at = Point::new(
        i32::try_from(x).expect("small"),
        i32::try_from(y).expect("small"),
    );
    assert!(
        life.damage.bounds().contains(at),
        "inside the coarse damage"
    );
    let content = comp
        .window(wm)
        .and_then(tairix_wm::Window::content)
        .expect("the window keeps its pixels");
    let pixel = content.get(x, y).expect("in bounds");
    assert_eq!(
        (pixel.r, pixel.g, pixel.b),
        (1, 2, 3),
        "the still cell was repainted"
    );
}

/// A window whose pixels were lost is handed back a fresh buffer, which the
/// frame repaints whole: every live cell is drawn, whether or not it changed.
#[test]
fn a_frame_into_a_fresh_buffer_repaints_the_whole_board() {
    let mut comp = compositor();
    let mut life = world(24, 16);
    set(&mut life, &[(5, 5)]);
    life.level[5 * life.cols + 5] = u8::MAX;
    let wm = canvas(&mut comp, &life);
    assert!(comp.set_surface(wm, Surface::new(4, 4).expect("a surface")));
    life.frames = 0;
    life.advance(0, wm, &mut comp);
    let content = comp
        .window(wm)
        .and_then(tairix_wm::Window::content)
        .expect("the window keeps its pixels");
    assert_eq!((content.width(), content.height()), life.size);
    let cell = life.cell_rect(5, 5);
    let centre = |start: i32| u32::try_from(start).expect("on screen") + life.cell / 2;
    let pixel = content
        .get(centre(cell.left()), centre(cell.top()))
        .expect("in bounds");
    assert_ne!(
        (pixel.r, pixel.g, pixel.b),
        (0, 0, 0),
        "the live cell is drawn"
    );
    let corner = content.get(0, 0).expect("in bounds");
    assert_eq!(
        (corner.r, corner.g, corner.b, corner.a),
        (0, 0, 0, 255),
        "the ground is black"
    );
}

/// Once every cell has reached its look, a frame with no generation in it
/// repaints nothing at all.
#[test]
fn a_settled_board_repaints_nothing_between_generations() {
    let mut comp = compositor();
    let mut life = world(24, 16);
    set(&mut life, &[(5, 5), (5, 6), (6, 5), (6, 6)]);
    for cell in [(5usize, 5usize), (5, 6), (6, 5), (6, 6)] {
        let at = cell.0 * life.cols + cell.1;
        life.queue(at);
    }
    let wm = canvas(&mut comp, &life);
    let mut now = 0;
    for _ in 0..12 {
        life.advance(now, wm, &mut comp);
        now += SceneClock::FRAME_NS;
    }
    assert!(life.active.is_empty(), "every cell is where it is going");
    comp.composite();
    life.frames = 0;
    life.advance(now, wm, &mut comp);
    assert!(!comp.has_damage(), "nothing changed, nothing repainted");
}

/// Smaller cells make a finer board, larger a coarser one.
#[test]
fn the_cell_size_decides_how_fine_the_board_is() {
    let size = (1_280, 720);
    let columns = |cells: CellSize| {
        Life::new(
            size,
            Scale::ONE,
            (
                false,
                LifeOptions {
                    cells,
                    ..LifeOptions::default()
                },
            ),
            0,
        )
        .expect("a world")
        .cols
    };
    let (small, medium, large) = (
        columns(CellSize::Small),
        columns(CellSize::Medium),
        columns(CellSize::Large),
    );
    assert!(small > medium && medium > large, "{small} {medium} {large}");
}

/// The speed decides how many frames pass between generations, and a birth
/// still completes within one generation at every speed.
#[test]
fn the_speed_paces_the_generations_and_keeps_a_birth_within_one() {
    let pace = |speed: Pace| {
        let life = Life::new(
            (1_280, 720),
            Scale::ONE,
            (
                false,
                LifeOptions {
                    speed,
                    ..LifeOptions::default()
                },
            ),
            0,
        )
        .expect("a world");
        assert!(
            u32::from(life.steps.0) * life.pace >= 255,
            "{speed:?}: a newborn cell is still arriving when the next generation comes"
        );
        life.pace
    };
    let (slow, normal, fast) = (pace(Pace::Slow), pace(Pace::Normal), pace(Pace::Fast));
    assert!(slow > normal && normal > fast, "{slow} {normal} {fast}");
    let per_second = |pace: u32| 1_000_000_000 / (u64::from(pace) * SceneClock::FRAME_NS);
    let owed = |pace: Pace| GENERATIONS_PER_SECOND * u64::from(pace.percent()) / 100;
    assert_eq!(per_second(normal), 10);
    assert_eq!(per_second(slow), owed(Pace::Slow));
    assert_eq!(per_second(fast), owed(Pace::Fast));
}

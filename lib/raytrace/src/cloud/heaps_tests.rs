//! Host tests of a deck's heaps: they cover its share of the sky, are as broad
//! as cumulus fields' sizes fall and as deep as their breadth, stand at the
//! same places whatever square they are drawn for, and every read and band
//! meets each heap whose cloud can reach it.

use tairix_mix::{mix32, unit};

use super::*;
use crate::cloud::Matter;

const CUMULUS: Deck = Deck {
    base: 1200.0,
    base_spread: 200.0,
    depth: (600.0, 2200.0),
    cover: 0.4,
    heap: 1.0,
    scale: 2600.0,
    stretch: 1.1,
    heading: 0.7,
    thickness: 0.14,
    billow: 420.0,
    fibre: 1.0,
    matter: Matter::Water,
    seed: 29,
};

/// How far past its envelope the tests take a heap's cloud to stand.
const MARGIN: f64 = 900.0;

/// The tests' heaps over a square `half` either way of `centre`, drawn and
/// listed across `runner` so many bands and rows a unit as `per` has it.
fn laid_by(
    deck: &Deck,
    (centre, half): ((f64, f64), f64),
    runner: &dyn JobRunner,
    per: (usize, usize),
) -> Heaps {
    let mut heaps = Heaps::begin(deck, (centre, half), MARGIN).expect("room");
    for first in (0..WEATHER_CELLS).step_by(per.0) {
        let bands = first..(first + per.0).min(WEATHER_CELLS);
        heaps.draw(deck, bands, runner).expect("drawn");
    }
    for first in (0..WEATHER_CELLS).step_by(per.1) {
        heaps
            .list(first..(first + per.1).min(WEATHER_CELLS), runner)
            .expect("listed");
    }
    heaps
}

/// The tests' heaps over a square `half` either way of `centre`.
fn laid(deck: &Deck, centre: (f64, f64), half: f64) -> Heaps {
    laid_by(
        deck,
        (centre, half),
        &tairix_parallel::Threaded::new(4),
        (37, 41),
    )
}

/// A place `draw` of the way across a square `half` either way of `centre`,
/// salted by `salt`.
fn place(draw: u32, salt: u32, (centre, half): ((f64, f64), f64)) -> (f64, f64) {
    (
        centre.0 + (2.0 * unit(mix32(draw ^ salt)) - 1.0) * half,
        centre.1 + (2.0 * unit(mix32(draw ^ salt ^ 0x5bd1)) - 1.0) * half,
    )
}

/// How far `(x, z)` lies within `heap`'s footprint, through its frame.
fn within(heaps: &Heaps, heap: &Heap, (x, z): (f64, f64)) -> f64 {
    let (cos, sin, stretch) = heaps.frame;
    let (east, north) = (x - f64::from(heap.east), z - f64::from(heap.north));
    let (along, across) = (east * cos + north * sin, north * cos - east * sin);
    f64::from(heap.radius) - mathf::hypot(along / stretch, across)
}

/// The cell of `heaps`' level `(x, z)` lies in.
fn cell(heaps: &Heaps, (x, z): (f64, f64)) -> usize {
    let at = |value: f64, from: f64| {
        cell_of((value - from) / heaps.size)
            .0
            .min(WEATHER_CELLS - 1)
    };
    at(z, heaps.corner.1) * WEATHER_CELLS + at(x, heaps.corner.0)
}

/// Over a broad square a deck's heaps cover its share of the sky, however
/// much of it that is, as the Poisson field they are drawn as has it.
#[test]
fn a_decks_heaps_cover_its_share_of_the_sky() {
    for cover in [0.15, 0.3, 0.42, 0.93] {
        let deck = Deck { cover, ..CUMULUS };
        let square = ((0.0, 0.0), 40_000.0);
        let heaps = laid(&deck, square.0, square.1);
        let samples = 20_000u32;
        let covered = (0..samples)
            .filter(|&draw| {
                let at = place(draw, 0x11, square);
                heaps.laid.iter().any(|heap| within(&heaps, heap, at) > 0.0)
            })
            .count();
        let share = real(covered) / f64::from(samples);
        assert!((share - cover).abs() < 0.04, "{cover}: {share} covered");
    }
}

/// A deck's heaps fall in breadth as cumulus fields' do: none narrower than
/// a quarter of its scale or broader than the whole of it, half of them
/// narrower than the distribution's median.
#[test]
fn heaps_breadths_fall_as_cumulus_fields_do() {
    let (least, most) = (NARROWEST * CUMULUS.scale, CUMULUS.scale);
    let order = 1.0 - SIZES;
    let median = power(
        f64::midpoint(power(least, order), power(most, order)),
        1.0 / order,
    );
    let heaps = laid(&CUMULUS, (0.0, 0.0), 40_000.0);
    assert!(heaps.laid.len() > 500, "{} heaps", heaps.laid.len());
    let breadths = || heaps.laid.iter().map(|heap| 2.0 * f64::from(heap.radius));
    assert!(breadths().all(|breadth| (least * 0.999..=most * 1.001).contains(&breadth)));
    let narrower = breadths().filter(|&breadth| breadth < median).count();
    let share = real(narrower) / real(heaps.laid.len());
    assert!((share - 0.5).abs() < 0.05, "{share} narrower than {median}");
    let mut previous = 0.0;
    for step in 0..=100u32 {
        let breadth = breadth_drawn(&CUMULUS, f64::from(step) / 100.0);
        assert!(breadth >= previous, "{step}: {breadth} after {previous}");
        previous = breadth;
    }
}

/// A heap stands as deep as its breadth gives it, from the deck's least depth
/// for its narrowest to its greatest for its broadest, and never outside
/// the deck's depths however its depth strays: nearly as tall as broad.
#[test]
fn a_heap_is_as_deep_as_its_breadth_gives_it() {
    let (least, most) = (NARROWEST * CUMULUS.scale, CUMULUS.scale);
    assert!((depth_of(&CUMULUS, least, 0.5) - CUMULUS.depth.0).abs() < 1e-9);
    assert!((depth_of(&CUMULUS, most, 0.5) - CUMULUS.depth.1).abs() < 1e-9);
    for step in 0..=200u32 {
        let breadth = least + (most - least) * f64::from(step) / 200.0;
        for draw in [0.0, 0.3, 0.999] {
            let depth = depth_of(&CUMULUS, breadth, draw);
            assert!(
                (CUMULUS.depth.0..=CUMULUS.depth.1).contains(&depth),
                "{depth}"
            );
        }
    }
    let heaps = laid(&CUMULUS, (0.0, 0.0), 20_000.0);
    let tall: f64 = heaps
        .laid
        .iter()
        .map(|heap| f64::from(heap.depth) / (2.0 * f64::from(heap.radius)))
        .sum::<f64>()
        / real(heaps.laid.len());
    assert!((0.7..1.1).contains(&tall), "{tall} as deep as broad");
}

/// The same heaps stand at a place whatever square they are drawn for, so a
/// heap is one heap from every eye and on every level.
#[test]
fn the_same_heaps_stand_wherever_their_square_is_drawn() {
    let (one, other) = (
        laid(&CUMULUS, (0.0, 0.0), 12_000.0),
        laid(&CUMULUS, (9_000.0, -5_000.0), 24_000.0),
    );
    let within_both = |heap: &Heap| {
        let (x, z) = (f64::from(heap.east), f64::from(heap.north));
        x.abs() < 10_000.0 && z.abs() < 10_000.0
    };
    let mut shared: Vec<Heap> = one.laid.iter().copied().filter(within_both).collect();
    let mut seen: Vec<Heap> = other.laid.iter().copied().filter(within_both).collect();
    assert!(shared.len() > 50, "{} heaps", shared.len());
    let order = |a: &Heap, b: &Heap| a.east.total_cmp(&b.east).then(a.north.total_cmp(&b.north));
    shared.sort_by(order);
    seen.sort_by(order);
    assert_eq!(shared, seen);
}

/// A read over a place meets the heap it lies furthest within, as every
/// heap would place it, and none where it lies too far outside every one;
/// and wherever any heap's cloud can stand, the band of the cell over it
/// holds the height.
#[test]
fn reads_and_bands_meet_every_heap_whose_cloud_can_reach_them() {
    let square = ((3_000.0, -2_000.0), 15_000.0);
    let heaps = laid(&CUMULUS, square.0, square.1);
    let (mut met, mut banded) = (0u32, 0u32);
    for draw in 0..20_000u32 {
        let at = place(draw, 0x23, (square.0, square.1 * 0.999));
        let altitude = 900.0 + 3_000.0 * unit(mix32(draw ^ 0x77));
        let cell = cell(&heaps, at);
        let best = heaps
            .laid
            .iter()
            .filter_map(|heap| {
                Placed::of(
                    within(&heaps, heap, at),
                    altitude - f64::from(heap.base),
                    heap.shape(),
                )
            })
            .filter(|placed| placed.inside + MARGIN > 0.0)
            .max_by(|a, b| a.inside.total_cmp(&b.inside));
        assert_eq!(
            heaps.placed(cell, at, altitude, MARGIN),
            best,
            "{at:?} {altitude}"
        );
        if best.is_none() {
            continue;
        }
        met += 1;
        let (row, column) = (cell / WEATHER_CELLS, cell % WEATHER_CELLS);
        let (low, high) = heaps.band((column, row), MARGIN);
        assert!(
            (f64::from(low)..=f64::from(high)).contains(&altitude),
            "{at:?} {altitude} beyond {low}..{high}"
        );
        banded += 1;
    }
    assert!(met > 1_000 && banded > 1_000, "{met} met, {banded} banded");
}

/// No cell's count reaches the most a cell holds, however crowded its deck:
/// its lattice is drawn narrow enough that a cell holds no more than
/// `CROWDED` heaps on the whole, short of the most by more than any draw
/// reaches.
#[test]
fn no_cell_draws_the_most_it_can_hold() {
    assert!(poisson(CROWDED, unit(u32::MAX), MOST_IN_A_CELL) < MOST_IN_A_CELL);
    for cover in [0.12, 0.44, 0.93] {
        for stretch in [1.0, 1.3, 3.0] {
            let deck = Deck {
                cover,
                stretch,
                ..CUMULUS
            };
            let held = (1.0 + GATHERING) * heaps_a_cell(&deck, lattice_of(&deck));
            assert!(held <= CROWDED * (1.0 + 1e-9), "{cover}, {stretch}: {held}");
        }
    }
}

/// A deck whose scale is too fine for a square to draw its lattice over is
/// refused rather than drawn without end.
#[test]
fn a_lattice_too_fine_to_draw_is_refused() {
    let fine = Deck {
        scale: 1.0,
        ..CUMULUS
    };
    assert!(Heaps::begin(&fine, ((0.0, 0.0), 40_000.0), MARGIN).is_none());
}

/// A level's heaps and its cells' lists come out the same however their
/// bands and rows are divided between units and cores, and each lists the
/// same heaps a single pass over every heap would.
#[test]
fn heaps_are_laid_alike_however_their_work_is_divided() {
    let square = ((-4_000.0, 7_000.0), 18_000.0);
    let one = laid_by(&CUMULUS, square, &tairix_parallel::SERIAL, (37, 41));
    let other = laid_by(
        &CUMULUS,
        square,
        &tairix_parallel::Threaded::new(7),
        (WEATHER_CELLS, 1),
    );
    assert_eq!(one.laid, other.laid);
    assert!(one.laid.len() > 200, "{} heaps", one.laid.len());
    let mut reached = alloc::vec![0usize; WEATHER_CELLS * WEATHER_CELLS];
    for heap in &one.laid {
        for row in 0..WEATHER_CELLS {
            one.reach_in(heap, row, |column, _| {
                reached[row * WEATHER_CELLS + column] += 1;
            });
        }
    }
    for (cell, &reached) in reached.iter().enumerate() {
        let listed: Vec<_> = one.listed(cell).map(|(heap, most)| (*heap, most)).collect();
        let again: Vec<_> = other
            .listed(cell)
            .map(|(heap, most)| (*heap, most))
            .collect();
        assert_eq!(listed, again, "cell {cell}");
        assert_eq!(listed.len(), reached, "cell {cell}");
        assert!(
            listed.windows(2).all(|pair| pair[0].1 >= pair[1].1),
            "cell {cell}"
        );
    }
}

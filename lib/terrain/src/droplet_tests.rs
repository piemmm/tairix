use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_util::mathf;

use super::*;

/// Short-lived droplets, so a test's lattice holds many tiles and its pieces
/// a halo a fraction of the land.
const LAW: Droplets = Droplets {
    lifetime: 12,
    inertia: 0.1,
    capacity: 4.0,
    min_slope: 0.01,
    erosion: 0.1,
    deposition: 0.1,
    evaporation: 0.02,
    gravity: 4.0,
    friction: 0.1,
    radius: 1,
};

/// A hillside falling toward the low-x edge of the square `domain` onto a
/// basin's level floor, the basin's far lip rising again at that edge, and the
/// whole rippled across so water gathers in its troughs: the height at lattice
/// place `(x, y)`.
fn hillside(domain: Placed, (x, y): (i64, i64)) -> f32 {
    let x = f64::from(i32::try_from(x - domain.origin.0).expect("a test place"));
    let y = f64::from(i32::try_from(y - domain.origin.1).expect("a test place"));
    let run = f64::from(domain.size.0 - 17);
    let slope = mathf::fmax((x - 16.0) / run, 0.0);
    let lip = mathf::fmax((6.0 - x) / 6.0, 0.0);
    let ripple = mathf::sin(y * 0.4);
    mathf::single(14.0 * slope * slope + 3.0 * lip * lip + ripple)
}

/// The ground before any droplet: the hillside over a land, or level ground.
struct Land {
    domain: Placed,
    level: Option<f32>,
}

impl Source for Land {
    fn fill(&self, region: Placed, into: &mut [f32]) -> Result<(), TerrainError> {
        let across = i64::from(region.size.0);
        for (index, slot) in into.iter_mut().enumerate() {
            let index = i64::try_from(index).map_err(|_| TerrainError::Shape)?;
            let at = (
                region.origin.0 + index % across,
                region.origin.1 + index / across,
            );
            *slot = self.level.unwrap_or_else(|| hillside(self.domain, at));
        }
        Ok(())
    }
}

/// A store that keeps what it is handed, but for every `drops`-th block, and
/// counts what it read.
#[derive(Default)]
struct Kept {
    blocks: BTreeMap<Block, super::Kept>,
    drops: Option<usize>,
    offered: usize,
    read: usize,
}

impl Store for Kept {
    fn read(&mut self, block: Block, read: &mut dyn FnMut(&super::Kept)) -> bool {
        match self.blocks.get(&block) {
            Some(kept) => {
                self.read += 1;
                read(kept);
                true
            }
            None => false,
        }
    }

    fn keep(&mut self, block: Block, kept: super::Kept) {
        self.offered += 1;
        if self
            .drops
            .is_none_or(|every| !self.offered.is_multiple_of(every))
        {
            self.blocks.insert(block, kept);
        }
    }
}

/// `piece` of `domain` worn by `rate` droplets under `key` across `runner`,
/// keeping the water's tracks, reading and keeping blocks in `store`: its
/// ground, its tracks, and the work it did.
fn worn(
    (domain, piece): (Placed, Placed),
    (rate, key): (f64, u64),
    store: &mut dyn Store,
    runner: &dyn JobRunner,
) -> (Vec<f32>, Vec<f32>, Work) {
    let tiles = Tiles::new(LAW, (rate, key), domain).expect("tiles");
    let land = Land {
        domain,
        level: None,
    };
    let mut wearing = Wearing::new(&tiles, (piece, true), store).expect("a wearing");
    while !wearing.step((&tiles, &land), store, runner).expect("wears") {}
    let (work, done) = wearing.work();
    assert_eq!(work, done, "a wearing done has done all it set out to");
    let (ground, track) = wearing.finish(store).expect("worn");
    (ground, track, work)
}

/// The ground before any droplet over `region`.
fn held(domain: Placed, region: Placed) -> Vec<f32> {
    let mut ground = alloc::vec![0.0; region.area().expect("an area")];
    Land {
        domain,
        level: None,
    }
    .fill(region, &mut ground)
    .expect("filled");
    ground
}

/// A square of `side` samples from the lattice's origin.
const fn square(side: u32) -> Placed {
    Placed {
        origin: (0, 0),
        size: (side, side),
    }
}

/// Ground worn from the steep upper slope is laid down on the gentle foot.
#[test]
fn droplets_carry_the_upper_slope_down_to_its_foot() {
    let domain = square(64);
    let before = held(domain, domain);
    let (height, _, work) = worn((domain, domain), (0.9, 7), &mut Unkept, &SERIAL);
    // Nine in ten of the land's 59² starting places, each tile's count
    // rounded on its own.
    let places = 59.0 * 59.0;
    let total = f64::from(u32::try_from(work.droplets).expect("a count"));
    assert!((total - 0.9 * places).abs() < 0.01 * places, "{total}");
    assert!(height.iter().all(|h| h.is_finite()));
    let change = |columns: core::ops::Range<usize>| {
        let mut total = 0.0;
        for y in 4..60 {
            for x in columns.clone() {
                let index = y * 64 + x;
                total += f64::from(height[index] - before[index]);
            }
        }
        total
    };
    let (foot, upper) = (change(6..16), change(24..60));
    assert!(upper < -1.0, "the upper slope lost {upper}");
    assert!(foot > 0.0, "the foot gained {foot}");
}

#[test]
fn level_ground_stays_level() {
    let domain = square(32);
    let tiles = Tiles::new(LAW, (0.8, 1), domain).expect("tiles");
    let land = Land {
        domain,
        level: Some(3.0),
    };
    let mut wearing = Wearing::new(&tiles, (domain, false), &mut Unkept).expect("a wearing");
    while !wearing
        .step((&tiles, &land), &mut Unkept, &SERIAL)
        .expect("wears")
    {}
    let (height, track) = wearing.finish(&mut Unkept).expect("worn");
    assert!(
        track.is_empty(),
        "a wearing that keeps no tracks hands back none"
    );
    assert!(height.iter().all(|h| (h - 3.0).abs() < 1e-6));
}

/// A land too narrow for a droplet's brush to stand on starts no droplet: its
/// wearing does no work and wears nothing; and a piece off its land is
/// refused.
#[test]
fn where_no_droplet_can_stand_nothing_is_worn() {
    let narrow = square(4);
    let tiles = Tiles::new(LAW, (0.9, 4), narrow).expect("tiles");
    let land = Land {
        domain: square(64),
        level: None,
    };
    let mut wearing = Wearing::new(&tiles, (narrow, false), &mut Unkept).expect("a wearing");
    while !wearing
        .step((&tiles, &land), &mut Unkept, &SERIAL)
        .expect("wears")
    {}
    assert_eq!(wearing.work().0.droplets, 0);
    let (height, _) = wearing.finish(&mut Unkept).expect("worn");
    assert_eq!(height, held(square(64), narrow));
    let tiles = Tiles::new(LAW, (0.9, 4), square(64)).expect("tiles");
    let apart = Placed {
        origin: (500, -500),
        size: (64, 64),
    };
    assert_eq!(
        Wearing::new(&tiles, (apart, false), &mut Unkept).err(),
        Some(TerrainError::Shape)
    );
}

/// A wearing's tiles shared among any number of cores, in whatever order they
/// take them, leave the ground and the water's tracks exactly as one core
/// running them in turn leaves them.
#[test]
fn a_run_shared_among_cores_leaves_the_ground_one_core_leaves() {
    let domain = square(400);
    let ground = |runner: &dyn JobRunner| {
        let (height, track, _) = worn((domain, domain), (0.15, 99), &mut Unkept, runner);
        (height, track)
    };
    let alone = ground(&SERIAL);
    assert_ne!(alone.0, held(domain, domain), "the droplets wore nothing");
    for runner in [&Reversed::new(3) as &dyn JobRunner, &Threaded::new(4)] {
        assert_eq!(ground(runner), alone);
    }
}

/// Whether `piece`'s ground and tracks are the whole land's over it, to the
/// bit, and where they first are not.
fn matches(
    domain: Placed,
    (piece, ground, track): (Placed, &[f32], &[f32]),
    (whole, whole_track): (&[f32], &[f32]),
) -> Result<(), (i64, i64)> {
    let (end_x, end_y) = piece.end().expect("an end");
    for y in piece.origin.1..end_y {
        for x in piece.origin.0..end_x {
            let (own, all) = (
                piece.index((x, y)).expect("an index"),
                domain.index((x, y)).expect("an index"),
            );
            if ground[own].to_bits() != whole[all].to_bits()
                || track[own].to_bits() != whole_track[all].to_bits()
            {
                return Err((x, y));
            }
        }
    }
    Ok(())
}

/// A piece of the land wears to the bit as the land worn whole does, its
/// water's tracks included, wherever on the lattice the land lies and however
/// its pieces fall across the tiles and the blocks: alone, and read from the
/// blocks a store kept of the pieces worn before it, in any order, whatever
/// the store dropped.
#[test]
fn a_piece_worn_from_its_blocks_matches_the_land_worn_whole() {
    let domain = Placed {
        origin: (-157, 2_031),
        size: (480, 480),
    };
    let law = (0.2, 0x5eed);
    let (whole, whole_track, _) = worn((domain, domain), law, &mut Unkept, &Threaded::new(4));
    assert_ne!(whole, held(domain, domain), "the droplets wore nothing");
    let piece = |(left, top, side): (i64, i64, u32)| {
        Placed::square((domain.origin.0 + left, domain.origin.1 + top), side)
    };
    // A corner, an edge, the middle, and one cut across the tilings' and the
    // blocks' seams.
    let pieces = [(0, 0, 120), (180, 0, 120), (180, 180, 120), (301, 233, 97)].map(piece);
    for &at in &pieces {
        let (ground, track, _) = worn((domain, at), law, &mut Unkept, &Reversed::new(3));
        assert_eq!(
            matches(domain, (at, &ground, &track), (&whole, &whole_track)),
            Ok(()),
            "{at:?} alone"
        );
    }
    for drops in [None, Some(3)] {
        for order in [[0, 1, 2, 3], [3, 1, 2, 0]] {
            let mut store = Kept {
                drops,
                ..Kept::default()
            };
            for at in order.map(|index| pieces[index]) {
                let (ground, track, _) = worn((domain, at), law, &mut store, &SERIAL);
                assert_eq!(
                    matches(domain, (at, &ground, &track), (&whole, &whole_track)),
                    Ok(()),
                    "{at:?} in order {order:?} dropping {drops:?}"
                );
            }
            assert!(store.read > 0, "later pieces read what earlier ones kept");
        }
    }
}

/// A piece beside one already worn fills only the ground no block kept and
/// runs fewer droplets than it would alone; worn again, it reads its own
/// blocks and does no work at all.
#[test]
fn a_piece_beside_one_worn_reruns_only_what_its_blocks_need() {
    let domain = square(480);
    let law = (0.2, 3);
    let (first, second) = (
        Placed::square((128, 128), 64),
        Placed::square((192, 128), 64),
    );
    let (_, _, alone) = worn((domain, second), law, &mut Unkept, &SERIAL);
    let mut store = Kept::default();
    worn((domain, first), law, &mut store, &SERIAL);
    let (_, _, beside) = worn((domain, second), law, &mut store, &SERIAL);
    assert!(
        beside.filled < alone.filled / 2,
        "{beside:?} against {alone:?}"
    );
    assert!(
        beside.droplets < alone.droplets,
        "{beside:?} against {alone:?}"
    );
    let (_, _, again) = worn((domain, second), law, &mut store, &SERIAL);
    assert_eq!(
        again,
        Work::default(),
        "a piece whose blocks are kept is read whole"
    );
}

/// A kept block of the wrong size is made again rather than read short.
#[test]
fn a_kept_block_that_does_not_fit_is_made_again() {
    let domain = square(200);
    let law = (0.3, 8);
    let piece = Placed::square((64, 64), 64);
    let (alone, _, _) = worn((domain, piece), law, &mut Unkept, &SERIAL);
    let mut store = Kept::default();
    store.blocks.insert(
        Block {
            worn: PASSES,
            row: 1,
            column: 1,
        },
        super::Kept {
            ground: alloc::vec![1.0; 10],
            track: Vec::new(),
        },
    );
    let (ground, _, work) = worn((domain, piece), law, &mut store, &SERIAL);
    assert_eq!(ground, alone);
    assert!(work.droplets > 0);
}

/// A stage answers the block at a place only where it holds one: never a
/// block of the next row for a place past the end of a row.
#[test]
fn a_stage_answers_a_block_only_where_it_holds_one() {
    let domain = square(300);
    let block = i64::from(BLOCK);
    let region = Placed {
        origin: (70, 10),
        size: (100, 150),
    };
    let stage = Stage::over(region, domain).expect("a stage");
    let (first, across) = (stage.first, stage.across);
    assert_eq!((first, across), ((1, 0), 2));
    for (index, held) in stage.blocks.iter().enumerate() {
        let place = block_place(first, across, index);
        let found = stage.at(place).expect("held");
        assert!(core::ptr::eq(found, held), "{place:?}");
        assert_eq!(held.region.origin, (place.0 * block, place.1 * block));
    }
    for outside in [
        (0, 0),
        (3, 0),
        (3, 1),
        (1, 3),
        (1, -1),
        (i64::MIN, 0),
        (i64::MAX, 0),
    ] {
        assert!(stage.at(outside).is_none(), "{outside:?}");
    }
}

#[test]
fn a_kept_block_counts_every_sample_it_holds() {
    let kept = super::Kept {
        ground: alloc::vec![0.0; 64],
        track: alloc::vec![0.0; 64],
    };
    assert_eq!(kept.bytes(), 128 * core::mem::size_of::<f32>());
    assert_eq!(super::Kept::default().bytes(), 0);
}

/// Whatever a tile's droplets do, they read and change nothing farther from
/// its own samples than their reach: run over its window, they wear it as run
/// over the whole land, and leave the rest as it was.
#[test]
fn a_tiles_droplets_change_nothing_past_their_window() {
    let domain = square(300);
    let tiles = Tiles::new(LAW, (0.6, 3), domain).expect("tiles");
    let before = held(domain, domain);
    let mut reaching = Vec::new();
    for pass in 0..PASSES {
        tiles.reaching(pass, domain, &mut reaching).expect("tiles");
        for &tile in [0, reaching.len() / 2 + 1, reaching.len() - 1]
            .iter()
            .map(|&index| &reaching[index])
        {
            let window = tiles.window(tile).expect("a window");
            let mut own = held(domain, window);
            let mut own_track = alloc::vec![0.0; own.len()];
            let mut running = tiles.begin(tile).expect("a run");
            let ran = tiles.run_on(&mut running, (&mut own, Some(&mut own_track)), u32::MAX);
            assert_eq!(ran, Ok(true), "one call runs them all");
            let touched = tiles
                .touched(&running)
                .expect("a box")
                .expect("its droplets touch something");
            assert!(window.holds(touched));
            // The same droplets over the whole land.
            let (mut all, mut all_track) = (before.clone(), alloc::vec![0.0; before.len()]);
            let mut reached = Reached {
                origin: domain.origin,
                columns: 300,
                rows: 300,
                height: &mut all,
                track: Some(&mut all_track),
                touched: Touched::NOTHING,
            };
            let (span, droplets) = tiles.span(tile).expect("a span");
            let mut dice = tiles.dice(tile);
            let brush = Brush {
                samples: &tiles.brush,
                spread: tiles.spread,
            };
            for _ in 0..droplets {
                let from = (
                    span.from.0 + span.breadth.0 * dice.next_f64(),
                    span.from.1 + span.breadth.1 * dice.next_f64(),
                );
                droplet((tiles.law, brush), &mut reached, from, tiles.bounds);
            }
            for (index, ((now, was), water)) in all.iter().zip(&before).zip(&all_track).enumerate()
            {
                let at = (
                    i64::try_from(index % 300).expect("a column"),
                    i64::try_from(index / 300).expect("a row"),
                );
                match window.index(at) {
                    Some(own_at) => {
                        assert_eq!(now.to_bits(), own[own_at].to_bits(), "{tile:?} at {at:?}");
                        assert_eq!(water.to_bits(), own_track[own_at].to_bits());
                    }
                    None => assert!(
                        now.to_bits() == was.to_bits() && *water == 0.0,
                        "{tile:?} changed {at:?}"
                    ),
                }
            }
        }
    }
}

/// A tile's droplets run in any number of parts wear its window to the bit as
/// they do run in one, touching the same box, each call running at most the
/// droplets it is allowed and a run ended running none.
#[test]
fn a_tiles_run_in_parts_is_its_run_whole() {
    let domain = square(120);
    let tiles = Tiles::new(LAW, (12.0, 7), domain).expect("tiles");
    let mut reaching = Vec::new();
    tiles.reaching(0, domain, &mut reaching).expect("tiles");
    let tile = reaching[reaching.len() / 2];
    let droplets = tiles.droplets(tile).expect("a count");
    assert!(droplets > DROPLET_UNIT, "{droplets} droplets fit one unit");
    let window = tiles.window(tile).expect("a window");
    let run = |most: u32| {
        let mut ground = held(domain, window);
        let mut track = alloc::vec![0.0; ground.len()];
        let mut running = tiles.begin(tile).expect("a run");
        let mut calls = 1;
        while !tiles
            .run_on(&mut running, (&mut ground, Some(&mut track)), most)
            .expect("runs")
        {
            calls += 1;
        }
        let ended = (ground.clone(), track.clone());
        let again = tiles.run_on(&mut running, (&mut ground, Some(&mut track)), most);
        assert_eq!(again, Ok(true));
        assert_eq!(
            (ground.clone(), track.clone()),
            ended,
            "an ended run runs none"
        );
        let bits = |samples: &[f32]| {
            samples
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>()
        };
        let touched = tiles.touched(&running).expect("a box");
        ((bits(&ground), bits(&track), touched), calls)
    };
    let (whole, calls) = run(u32::MAX);
    assert_eq!(calls, 1);
    assert!(whole.2.is_some(), "its droplets touch something");
    for most in [1, 7, DROPLET_UNIT] {
        let (parts, calls) = run(most);
        assert_eq!(calls, droplets.div_ceil(most), "{most} a call");
        assert!(parts == whole, "run {most} a call");
    }
}

/// Tiles starting more droplets than a unit runs go on over as many units as
/// the most of a batch's tiles needs, after a unit gathering their windows, a
/// core running at most a unit's droplets a step, and leave the ground one
/// run of each tile leaves, however many cores share them.
#[test]
fn a_tile_with_more_droplets_than_a_unit_runs_over_several_units() {
    let domain = square(120);
    let law = (12.0, 7);
    let tiles = Tiles::new(LAW, law, domain).expect("tiles");
    let land = Land {
        domain,
        level: None,
    };
    let mut wearing = Wearing::new(&tiles, (domain, true), &mut Unkept).expect("a wearing");
    let units: usize = wearing
        .tiles
        .iter()
        .flatten()
        .map(|&tile| {
            let droplets = tiles.droplets(tile).expect("a count");
            usize::try_from(droplets.div_ceil(DROPLET_UNIT)).expect("a count")
        })
        .sum();
    let count: usize = wearing.tiles.iter().map(Vec::len).sum();
    assert!(units > count, "every tile fits a unit");
    let mut stepped = 0;
    loop {
        let passing = matches!(wearing.phase, Phase::Pass { .. });
        let done = wearing
            .step((&tiles, &land), &mut Unkept, &SERIAL)
            .expect("wears");
        stepped += usize::from(passing);
        if done {
            break;
        }
    }
    assert_eq!(
        stepped,
        count + units,
        "a step gathering each tile, then a unit of it"
    );
    let alone = wearing.finish(&mut Unkept).expect("worn");
    assert_ne!(alone.0, held(domain, domain), "the droplets wore nothing");
    for runner in [&Reversed::new(3) as &dyn JobRunner, &Threaded::new(4)] {
        let (ground, track, _) = worn((domain, domain), law, &mut Unkept, runner);
        assert_eq!((ground, track), alone);
    }
}

/// A land that refuses its first fill.
struct Flaky(AtomicBool, Land);

impl Source for Flaky {
    fn fill(&self, region: Placed, into: &mut [f32]) -> Result<(), TerrainError> {
        if self.0.swap(false, Ordering::Relaxed) {
            return Err(TerrainError::OutOfMemory);
        }
        self.1.fill(region, into)
    }
}

/// A land that counts the samples it fills, and refuses the fill it is asked
/// for `refusing`-th.
struct Counted {
    land: Land,
    calls: AtomicUsize,
    samples: AtomicUsize,
    refusing: usize,
}

impl Source for Counted {
    fn fill(&self, region: Placed, into: &mut [f32]) -> Result<(), TerrainError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) + 1 == self.refusing {
            return Err(TerrainError::OutOfMemory);
        }
        self.samples.fetch_add(into.len(), Ordering::Relaxed);
        self.land.fill(region, into)
    }
}

/// A unit fills at most a band of a block's rows a core however wide the
/// runner, a block part filled resumed in the unit after; refused part way
/// through a block, the wearing carries on to the ground one never refused
/// leaves.
#[test]
fn a_fill_reads_a_band_of_rows_a_core_and_resumes_a_block_part_filled() {
    let domain = square(240);
    let tiles = Tiles::new(LAW, (0.3, 21), domain).expect("tiles");
    let (unbroken, _, _) = worn((domain, domain), (0.3, 21), &mut Unkept, &SERIAL);
    let band = usize::try_from(FILL_ROWS * BLOCK).expect("a band");
    let wide = Reversed::new(3);
    for (runner, refusing) in [(&SERIAL as &dyn JobRunner, 2), (&wide, 5)] {
        let counted = Counted {
            land: Land {
                domain,
                level: None,
            },
            calls: AtomicUsize::new(0),
            samples: AtomicUsize::new(0),
            refusing,
        };
        let mut wearing = Wearing::new(&tiles, (domain, true), &mut Unkept).expect("a wearing");
        let mut refused = 0;
        loop {
            let (filling, before) = (
                matches!(wearing.phase, Phase::Fill { .. }),
                counted.samples.load(Ordering::Relaxed),
            );
            match wearing.step((&tiles, &counted), &mut Unkept, runner) {
                Ok(done) => {
                    let filled = counted.samples.load(Ordering::Relaxed) - before;
                    assert!(!filling || filled <= runner.width() * band, "{filled}");
                    if done {
                        break;
                    }
                }
                Err(_) => refused += 1,
            }
        }
        assert_eq!(refused, 1);
        assert_eq!(wearing.finish(&mut Unkept).expect("worn").0, unbroken);
    }
}

/// A place's index on a square as wide as a lattice runs is its row times
/// that width and more, past what a signed word holds, never an overflow;
/// and where no index holds it, none.
#[test]
fn an_index_on_the_widest_square_is_exact() {
    let widest = Placed {
        origin: (0, 0),
        size: (u32::MAX, u32::MAX),
    };
    let expected: u64 = 4_000_000_000 * u64::from(u32::MAX) + 4_000_000_000;
    assert_eq!(
        widest.index((4_000_000_000, 4_000_000_000)),
        usize::try_from(expected).ok()
    );
    assert_eq!(widest.index((-1, 0)), None);
    assert_eq!(widest.index((0, i64::from(u32::MAX))), None);
}

/// A tile's run is refused buffers that do not fit its window, and a unit a
/// source refuses leaves the wearing where it stood: it carries on to the
/// ground one never refused leaves; and its finish, asked too soon, is
/// refused and leaves it to finish once worn.
#[test]
fn a_refused_unit_changes_nothing() {
    let domain = square(240);
    let tiles = Tiles::new(LAW, (0.3, 21), domain).expect("tiles");
    let mut reaching = Vec::new();
    tiles.reaching(0, domain, &mut reaching).expect("tiles");
    let tile = reaching[3];
    let area = tiles
        .window(tile)
        .expect("a window")
        .area()
        .expect("an area");
    let mut running = tiles.begin(tile).expect("a run");
    let mut short = alloc::vec![0.0; area - 1];
    assert_eq!(
        tiles.run_on(&mut running, (&mut short, None), u32::MAX),
        Err(TerrainError::Shape)
    );
    let mut ground = alloc::vec![0.0; area];
    let mut track = alloc::vec![0.0; area + 1];
    assert_eq!(
        tiles.run_on(&mut running, (&mut ground, Some(&mut track)), u32::MAX),
        Err(TerrainError::Shape)
    );
    assert_eq!(
        running.left,
        tiles.droplets(tile).expect("a count"),
        "a refused run runs no droplet"
    );

    let (unbroken, _, _) = worn((domain, domain), (0.3, 21), &mut Unkept, &SERIAL);
    let mut wearing = Wearing::new(&tiles, (domain, true), &mut Unkept).expect("a wearing");
    let flaky = Flaky(
        AtomicBool::new(true),
        Land {
            domain,
            level: None,
        },
    );
    assert!(wearing
        .step((&tiles, &flaky), &mut Unkept, &SERIAL)
        .is_err());
    assert_eq!(wearing.work().1, Work::default(), "nothing done");
    assert_eq!(wearing.finish(&mut Unkept), Err(TerrainError::Shape));
    while !wearing
        .step((&tiles, &flaky), &mut Unkept, &SERIAL)
        .expect("wears")
    {}
    assert_eq!(wearing.finish(&mut Unkept).expect("worn").0, unbroken);
}

/// A law too long to count, a rate that is no rate, and a land past what a
/// double holds exactly are refused rather than overflowing; the halo is two
/// passes of a tile and its reach either side, and three blocks less one.
#[test]
fn a_run_is_refused_what_it_cannot_count() {
    let domain = square(64);
    // Reach 12 + 1 + 1 = 14; a tile 14 + 5 = 19 samples a side.
    let tiles = Tiles::new(LAW, (0.5, 1), domain).expect("tiles");
    assert_eq!(tiles.halo(), Ok(2 * (2 * 14 + 19 - 1) + 3 * (BLOCK - 1)));
    let endless = Droplets {
        lifetime: u32::MAX,
        ..LAW
    };
    assert_eq!(
        Tiles::new(endless, (0.5, 1), domain).err(),
        Some(TerrainError::Shape)
    );
    for rate in [-0.1, f64::NAN, f64::INFINITY, 1e12] {
        let refused = Tiles::new(LAW, (rate, 1), domain).and_then(|tiles| {
            let mut reaching = Vec::new();
            tiles.reaching(0, domain, &mut reaching)
        });
        assert_eq!(refused.err(), Some(TerrainError::Shape), "{rate}");
    }
    let far = Placed {
        origin: (i64::MAX - 10, 0),
        size: (64, 64),
    };
    assert_eq!(
        Tiles::new(LAW, (0.5, 1), far).err(),
        Some(TerrainError::Shape)
    );
    let past_exact = Placed {
        origin: (1 << 54, 0),
        size: (64, 64),
    };
    assert_eq!(
        Tiles::new(LAW, (0.5, 1), past_exact).err(),
        Some(TerrainError::Shape)
    );
}

#[test]
fn water_gathers_in_the_hollows_the_droplets_run_through() {
    let domain = square(64);
    let (_, flux, _) = worn((domain, domain), (0.45, 3), &mut Unkept, &SERIAL);
    // The ripple's troughs lie where `sin(0.4 y)` is least, its crests where
    // it is most.
    let row_flux = |y: usize| (8..56).map(|x| f64::from(flux[y * 64 + x])).sum::<f64>();
    let trough = row_flux(12) + row_flux(27);
    let crest = row_flux(4) + row_flux(20);
    assert!(trough > 2.0 * crest, "trough {trough} crest {crest}");
}

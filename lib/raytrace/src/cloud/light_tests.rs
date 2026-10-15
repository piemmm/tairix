//! Host tests of a bank's light: it comes out the same however many cores
//! solve it, photons meet only cloud that stands, their light is counted in
//! the units it falls in, and the sun's optical depth deepens through cloud.

use super::*;
use crate::atmosphere::GROUND;
use crate::cloud::{Deck, Lighting, Matter, SCREEN_PIXEL, SUNLIGHT_LEVELS, SUN_COSINES};
use crate::sample::{mix32, unit};

const HEAPS: Deck = Deck {
    base: 1200.0,
    base_spread: 150.0,
    depth: (600.0, 1800.0),
    cover: 0.4,
    heap: 1.0,
    scale: 2500.0,
    stretch: 1.2,
    heading: 0.4,
    thickness: 0.1,
    billow: 400.0,
    fibre: 1.0,
    matter: Matter::Water,
    seed: 17,
};

/// A ceiling of cloud from horizon to horizon.
const OVERCAST: Deck = Deck {
    cover: 1.0,
    heap: 0.3,
    ..HEAPS
};

/// Light solved coarsely enough for a test to solve it many times over,
/// every level holding its light finely where cloud stands.
const COARSE: Resolution = Resolution {
    cells: 16,
    finest: 4000.0,
    tracks: 24.0,
    fine: 2,
    reach: f64::INFINITY,
};

/// The sunlight, sky and ground a test bank is lit by.
const SUNLIGHT: f64 = 20.0;
const SKY: Vec3 = Vec3::new(0.6, 0.8, 1.2);
const GROUND_LIGHT: f64 = 5.0;
const ALBEDO: f64 = 0.3;

/// A bank of `deck` lit from `sun`, its finest level spanning no more than
/// `most`, its light solved as `resolution` has it across `runner`.
fn solved(
    deck: Deck,
    sun: Vec3,
    (most, resolution): (f64, Resolution),
    runner: &dyn JobRunner,
) -> Cloudbank {
    let mut bank =
        Cloudbank::new([Some(deck), None], (most, resolution), sun, GROUND * 1000.0).expect("fits");
    bank.stand(Vec3::ZERO, SCREEN_PIXEL).expect("levels fit");
    let cosines = bank.sun_cosines(sun);
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(SUNLIGHT); SUNLIGHT_LEVELS * SUN_COSINES],
        cosines,
        toward: sun,
        above: SKY,
        ground: (Vec3::splat(GROUND_LIGHT), ALBEDO),
    });
    while !bank.step(runner, None).expect("held") {}
    bank
}

fn high_sun() -> Vec3 {
    Vec3::new(0.3, 0.8, 0.2).normalized()
}

#[test]
fn a_banks_light_comes_out_the_same_however_many_cores_follow_it() {
    let solve = |runner: &dyn JobRunner| solved(HEAPS, high_sun(), (12_000.0, COARSE), runner);
    let (alone, spread) = (
        solve(&tairix_parallel::SERIAL),
        solve(&tairix_parallel::Threaded::new(3)),
    );
    let (alone, spread) = (&alone.lights.volumes, &spread.lights.volumes);
    assert_eq!(alone.len(), spread.len());
    assert!(alone.len() >= 2, "{} levels", alone.len());
    for (a, b) in alone.iter().zip(spread) {
        assert_eq!(a.depth.codes, b.depth.codes);
        assert_eq!(a.fine.runs.blocks, b.fine.runs.blocks);
        for (a, b) in a
            .fluence
            .iter()
            .zip(&b.fluence)
            .chain(a.flux.iter().zip(&b.flux))
            .chain(a.fine.fluence.iter().zip(&b.fine.fluence))
            .chain(a.fine.flux.iter().zip(&b.fine.flux))
        {
            assert_eq!(a.codes, b.codes);
            assert_eq!(a.scale.to_bits(), b.scale.to_bits());
        }
        assert_eq!(a.open, b.open);
        assert_eq!(a.shed, b.shed);
        assert!(
            a.fluence
                .iter()
                .any(|field| field.codes.iter().any(|&code| code > 0)),
            "lit"
        );
    }
}

#[test]
fn a_banks_light_is_finite_and_its_fluence_never_negative() {
    let bank = solved(
        HEAPS,
        high_sun(),
        (12_000.0, COARSE),
        &tairix_parallel::Threaded::new(4),
    );
    for volume in &bank.lights.volumes {
        for field in &volume.fluence {
            assert!(!field.signed);
            assert!(field.scale.is_finite() && field.scale > 0.0);
        }
        for field in &volume.flux {
            assert!(field.signed && field.scale.is_finite());
        }
        assert!(volume
            .open
            .iter()
            .all(|&open| (0.0..=1.0 + 1e-6).contains(&open)));
        assert!(volume
            .shed
            .iter()
            .flatten()
            .all(|&shed| shed.is_finite() && shed >= 0.0));
    }
}

/// Beneath a deck too thin to scatter, the sunlight the ground gives back
/// fills the air with twice its exitance, as a lambertian ground's light
/// does; and the sky's light, falling evenly from above and given back from
/// below, with `2πL` and twice its own exitance off the ground. So every
/// photon's power is counted in the units the light it carries falls in.
#[test]
fn the_light_a_thin_veil_holds_is_the_sky_and_the_lit_ground_s_own() {
    let veil = Deck {
        thickness: 1e-7,
        ..OVERCAST
    };
    let resolution = Resolution {
        tracks: 120.0,
        ..COARSE
    };
    let bank = solved(
        veil,
        high_sun(),
        (4000.0, resolution),
        &tairix_parallel::Threaded::new(4),
    );
    let sky = SKY.luminance();
    let expected = [2.0 * ALBEDO * GROUND_LIGHT, 2.0 * PI * sky * (1.0 + ALBEDO)];
    let (mut sums, mut count) = ([0.0; 2], 0u32);
    for index in 0..400u32 {
        let draw = |salt: u32| unit(mix32(index.wrapping_mul(0x9e37_79b9) ^ salt));
        let at = (
            3000.0 * (2.0 * draw(1) - 1.0),
            bank.floor + (bank.ceiling - bank.floor) * (0.1 + 0.8 * draw(2)),
            3000.0 * (2.0 * draw(3) - 1.0),
        );
        let reading = bank.lights.reading(&bank, at).expect("over the level");
        for (sum, fluence) in sums.iter_mut().zip(reading.fluence) {
            *sum += fluence;
        }
        count += 1;
    }
    for (colour, (sum, expected)) in sums.iter().zip(expected).enumerate() {
        let mean = sum / f64::from(count);
        assert!(
            (mean / expected - 1.0).abs() < 0.08,
            "colour {colour}: {mean} where {expected} is held"
        );
    }
}

/// The sun's optical depth deepens down through an overcast and is all but
/// nought above it.
#[test]
fn the_suns_depth_deepens_down_through_an_overcast() {
    let bank = solved(
        OVERCAST,
        Vec3::UP,
        (12_000.0, COARSE),
        &tairix_parallel::SERIAL,
    );
    for index in 0..40u32 {
        let (x, z) = (
            f64::from(index) * 173.0 - 3000.0,
            f64::from(index % 7) * 410.0 - 1500.0,
        );
        let at = |height: f64| {
            bank.lights
                .depth(&bank, (x, height, z))
                .expect("over the level")
        };
        let (top, bottom) = (at(bank.ceiling - 1.0), at(bank.floor + 1.0));
        assert!(top < 0.5, "{top} above the overcast");
        assert!(bottom > 10.0, "{bottom} beneath it");
        let mut last = top;
        for step in 1..20u32 {
            let height = bank.ceiling - (bank.ceiling - bank.floor) * f64::from(step) / 20.0;
            let here = at(height);
            assert!(here + 1e-6 >= last, "{height}: {here} after {last}");
            last = here;
        }
    }
}

/// No cloud a photon meets holds more extinction than its voxel's bound, so
/// every meeting is kept as often as the cloud there truly has it; and none
/// stands where its weather says none can.
#[test]
fn no_cloud_holds_more_than_its_voxels_bound() {
    let bank = solved(
        HEAPS,
        high_sun(),
        (12_000.0, COARSE),
        &tairix_parallel::SERIAL,
    );
    let reader = Reader { bank: &bank };
    let (mut cloudy, mut checked) = (0u32, 0u32);
    for volume in &bank.lights.volumes {
        let grid = volume.grid;
        let mut solving = begin(grid).expect("fits");
        stand_rows(&mut solving, &bank, 0..grid.cells, &tairix_parallel::SERIAL).expect("fits");
        voxel_rows(
            &mut solving,
            &bank,
            0..grid.rows(),
            &tairix_parallel::SERIAL,
        )
        .expect("fits");
        for index in 0..4000u32 {
            let draw = |salt: u32| unit(mix32(index.wrapping_mul(0x85eb_ca6b) ^ salt));
            let voxel = (
                cell_of(draw(1) * real(grid.cells)).0.min(grid.cells - 1),
                cell_of(draw(2) * real(grid.layers)).0.min(grid.layers - 1),
                cell_of(draw(3) * real(grid.cells)).0.min(grid.cells - 1),
            );
            let (x, altitude, z) = grid.middle(&bank, voxel);
            let offset = |salt: u32, size: f64| (draw(salt) - 0.5) * size;
            let at = (
                x + offset(4, grid.breadth),
                altitude + offset(5, grid.height),
                z + offset(6, grid.breadth),
            );
            let extinction = reader
                .density_at(point(&bank, at), at.1, Edges::Resolved(FOOTPRINT))
                .map_or(0.0, |sample| sample.extinction);
            let bound = f64::from(solving.bounds[grid.index(voxel)]);
            assert!(
                extinction <= bound * (1.0 + 1e-6),
                "{extinction} past {bound} at {at:?}"
            );
            if extinction > 0.0 {
                assert!(
                    bank.may_stand((at.0, at.2), at.1),
                    "cloud where none can stand at {at:?}"
                );
                cloudy += 1;
            }
            checked += 1;
        }
    }
    assert!(cloudy > 200, "{cloudy} of {checked} places held cloud");
}

/// A thick overcast lit from overhead sends down, from its floor, the
/// sunlight that diffuses through it: about as much as the diffusion limit
/// of a lossless slab its scaled depth deep lets through with the sun
/// overhead, `(2/3 + 1) / (4/3 + τ')`.
#[test]
fn a_thick_overcast_lets_down_what_diffuses_through_it() {
    let deck = Deck {
        thickness: 0.06,
        ..OVERCAST
    };
    let mut bank = Cloudbank::new(
        [Some(deck), None],
        (4000.0, COARSE),
        Vec3::UP,
        GROUND * 1000.0,
    )
    .expect("fits");
    bank.stand(Vec3::ZERO, SCREEN_PIXEL).expect("levels fit");
    let cosines = bank.sun_cosines(Vec3::UP);
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(SUNLIGHT); SUNLIGHT_LEVELS * SUN_COSINES],
        cosines,
        toward: Vec3::UP,
        above: Vec3::ZERO,
        ground: (Vec3::ZERO, 0.0),
    });
    while !bank
        .step(&tairix_parallel::Threaded::new(4), None)
        .expect("held")
    {}
    let reader = Reader { bank: &bank };
    let scale = 1.0 - bank.scattering().asymmetry;
    let (mut shed, mut through, mut count) = (0.0, 0.0, 0u32);
    for index in 0..60u32 {
        let (x, z) = (
            f64::from(index % 10) * 500.0 - 2500.0,
            f64::from(index / 10) * 600.0 - 1500.0,
        );
        let depth: f64 = (0..4000u32)
            .filter_map(|metre| {
                let height = bank.floor + f64::from(metre);
                reader.density_at(point(&bank, (x, height, z)), height, Edges::Resolved(1.0))
            })
            .map(|sample| sample.extinction)
            .sum();
        let scaled = scale * depth;
        through += (2.0 / 3.0 + 1.0) / (4.0 / 3.0 + scaled);
        shed += bank.lights.beneath(&bank, (x, z)).1[SUNLIT];
        count += 1;
    }
    let (shed, expected) = (
        shed / (f64::from(count) * SUNLIGHT),
        through / f64::from(count),
    );
    assert!(
        expected < 0.6,
        "a deck thick enough to diffuse through: {expected}"
    );
    assert!(
        (shed / expected - 1.0).abs() < 0.3,
        "{shed} let down where diffusion lets {expected}"
    );
}

#[test]
fn a_packed_value_comes_back_within_its_step() {
    for signed in [false, true] {
        let most = 37.5;
        let mut packed =
            Packed::reserved(64, Packed::scale_for(most, signed), signed).expect("fits");
        for index in 0..64 {
            let value = if signed {
                most * (real(index) / 32.0 - 1.0)
            } else {
                most * real(index) / 63.0
            };
            packed.codes[index] = packed.code(value);
            assert!(
                (packed.at(index) - value).abs() <= 0.5 * packed.scale + 1e-12,
                "{value}"
            );
        }
    }
    let unsigned = Packed::reserved(1, 0.1, false).expect("fits");
    assert_eq!(
        unsigned.code(-5.0),
        0,
        "an unsigned field clamps below nought"
    );
}

#[test]
fn a_photon_comes_round_the_levels_square() {
    for (value, side, expected) in [
        (3.5, 10.0, 3.5),
        (-0.5, 10.0, 9.5),
        (25.0, 10.0, 5.0),
        (-30.0, 10.0, 0.0),
    ] {
        let wrapped = around(value, side);
        assert!((wrapped - expected).abs() < 1e-12, "{value}: {wrapped}");
        assert!((0.0..side).contains(&wrapped));
    }
}

#[test]
fn the_track_laid_counts_past_any_thirty_two_bits() {
    let plan = Plan {
        laid: (1 << 40) + 7,
        ..Plan::default()
    };
    assert!((track_laid(&plan) - 1_099_511_627_783.0).abs() < 1e-3);
}

/// A bank of `deck` stood and lit as [`solved`] has it, its light not yet
/// built.
fn lit(deck: Deck) -> Cloudbank {
    let sun = high_sun();
    let mut bank =
        Cloudbank::new([Some(deck), None], (12_000.0, COARSE), sun, GROUND * 1000.0).expect("fits");
    bank.stand(Vec3::ZERO, SCREEN_PIXEL).expect("levels fit");
    let cosines = bank.sun_cosines(sun);
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(SUNLIGHT); SUNLIGHT_LEVELS * SUN_COSINES],
        cosines,
        toward: sun,
        above: SKY,
        ground: (Vec3::splat(GROUND_LIGHT), ALBEDO),
    });
    bank
}

/// A bank of `deck` [`lit`], its light followed until its coarsest level's
/// photons are all followed, their tallies not yet made fields.
fn tallied(deck: Deck) -> Cloudbank {
    let mut bank = lit(deck);
    let runner = tairix_parallel::Threaded::new(3);
    while !matches!(bank.lights.stage, Stage::Fields(..)) {
        assert!(
            !bank.step(&runner, None).expect("held"),
            "solved before its fields"
        );
    }
    bank
}

/// A bank's light counts the units of its solving done as it goes, its fine
/// levels' among them: never falling back, never past its whole, and whole
/// only once every level is solved.
#[test]
fn a_banks_light_counts_its_units_to_its_whole_once_solved() {
    let mut bank = lit(HEAPS);
    assert!(bank
        .lights
        .volumes
        .iter()
        .any(|volume| volume.grid.fine > 1));
    let runner = tairix_parallel::Threaded::new(3);
    let mut before = 0.0;
    loop {
        let solved = bank.step(&runner, None).expect("held");
        let (done, total) = bank.lights.units();
        assert!(done >= before, "fell back from {before} to {done}");
        assert!(done <= total, "{done} past its whole {total}");
        assert_eq!(solved, done >= total, "{done} of {total}, solved {solved}");
        before = done;
        if solved {
            break;
        }
    }
}

/// A level holds its fine light in exactly the voxels holding cloud, each
/// as its own block, and finds each fine cell of them where it laid it.
#[test]
fn a_levels_runs_hold_each_voxel_holding_cloud_once() {
    let bank = tallied(HEAPS);
    let solving = bank.lights.solving.as_ref().expect("solving");
    let (grid, runs) = (solving.volume.grid, &solving.volume.fine.runs);
    assert!(runs.blocks > 100, "{} blocks", runs.blocks);
    assert_eq!(solving.voxels.len(), runs.blocks);
    let mut seen = alloc::vec![false; runs.blocks];
    for k in 0..grid.layers {
        for j in 0..grid.cells {
            for i in 0..grid.cells {
                let voxel = (i, k, j);
                let cloudy = solving.sigma[grid.index(voxel)] > 0.0;
                let block = runs.block(grid, voxel);
                assert_eq!(block.is_some(), cloudy, "{voxel:?}");
                let Some(block) = block else {
                    continue;
                };
                assert!(!seen[block], "{voxel:?} shares block {block}");
                seen[block] = true;
                assert_eq!(solving.voxels[block] as usize, grid.index(voxel));
                for within in 0..grid.fine_cells() {
                    let cell = fine_cell(grid, solving.voxels[block], within);
                    assert_eq!(
                        runs.fine(grid, cell),
                        Some(block * grid.fine_cells() + within)
                    );
                }
            }
        }
    }
    assert!(seen.iter().all(|&held| held));
}

/// A track through a voxel held finely is laid in its fine cells alone, the
/// whole of it shared out among those it crosses whatever its way; through
/// one that is not, in the voxel alone. And in a level's photons' tallies
/// every voxel held finely is lit through its fine cells and none through
/// its own.
#[test]
fn a_held_voxels_track_is_laid_whole_in_its_fine_cells_alone() {
    let bank = tallied(HEAPS);
    let solved = bank.lights.solving.as_ref().expect("solving");
    let grid = solved.volume.grid;
    let per = grid.fine_cells();
    assert!(per > 1, "{per} fine cells a voxel");
    let (runs, tally) = (&solved.volume.fine.runs, &solved.tally);
    let mut lit = 0;
    for block in 0..runs.blocks {
        let voxel = solved.voxels[block] as usize;
        for field in 0..FLUENCES + FLUXES {
            let own = tally.voxels.field(field).expect("held")[voxel].load(Ordering::Relaxed);
            assert_eq!(own, 0, "block {block} field {field} lit through its voxel");
        }
        let fine = tally.fine.field(SUNLIT).expect("held");
        lit += usize::from((0..per).any(|within| summed(fine, block * per + within) > 0.0));
    }
    assert!(lit > runs.blocks / 2, "{lit} of {} lit", runs.blocks);

    let mut solving = solved.clone();
    solving.tally = Tally::reserved(&solving.volume).expect("fits");
    while !solving.tally.lay(&tairix_parallel::SERIAL) {}
    let walker = Walker::new(&solving, &bank);
    let (cells, side) = (grid.cells, grid.cells * grid.cells);
    let at = |voxel: usize| [voxel % cells, voxel / side, voxel / cells % cells];
    for (draw, block) in (0..runs.blocks).step_by(runs.blocks / 40 + 1).enumerate() {
        let voxel = solving.voxels[block] as usize;
        let cell = at(voxel);
        let unit = |salt: u32| unit(mix32(u32::try_from(draw).expect("small") ^ salt));
        let way = Vec3::new(unit(1) - 0.5, unit(2) - 0.5, unit(3) - 0.5).normalized();
        let ways = [way.x, way.y, way.z];
        let from = [
            real(cell[0]) + unit(4),
            real(cell[1]) + unit(5),
            real(cell[2]) + unit(6),
        ];
        let (length, weight) = (unit(7) * grid.breadth, 1.0e3);
        let mut pending = [Pending::new(SUNLIT), Pending::new(SUNLIT)];
        walker.lay(
            &mut pending,
            (voxel, Some((block, from))),
            (cell, ways),
            (length, weight),
        );
        let [coarse, fine] = &mut pending;
        coarse.flush(&solving.tally.voxels);
        fine.flush(&solving.tally.fine);
        let fluence = solving.tally.fine.field(SUNLIT).expect("held");
        let parts: f64 = (0..per)
            .map(|within| summed(fluence, block * per + within))
            .sum();
        let laid = weight * length;
        // Each fine cell's add rounds to a whole on its own.
        assert!(
            (parts - laid).abs() <= real(per),
            "block {block}: {parts} of {laid}"
        );
        let own = solving.tally.voxels.field(SUNLIT).expect("held")[voxel].load(Ordering::Relaxed);
        assert_eq!(own, 0, "block {block} lit through its voxel");
        for within in 0..per {
            let index = block * per + within;
            fluence[index].store(0, Ordering::Relaxed);
        }
    }
    let open = (0..grid.voxels())
        .find(|&voxel| {
            runs.block(grid, (voxel % cells, voxel / side, voxel / cells % cells))
                .is_none()
        })
        .expect("a voxel holds no cloud");
    let mut pending = [Pending::new(SUNLIT), Pending::new(SUNLIT)];
    walker.lay(
        &mut pending,
        (open, None),
        (at(open), [0.0, 1.0, 0.0]),
        (10.0, 3.0),
    );
    let [coarse, fine] = &mut pending;
    coarse.flush(&solving.tally.voxels);
    fine.flush(&solving.tally.fine);
    let own = solving.tally.voxels.field(SUNLIT).expect("held")[open].load(Ordering::Relaxed);
    assert_eq!(own, 30, "laid whole in the voxel");
    let fluence = solving.tally.fine.field(SUNLIT).expect("held");
    assert!(fluence
        .iter()
        .all(|value| value.load(Ordering::Relaxed) == 0));
}

/// A place reads its light from the fine cells about it where all are held,
/// from its voxels where none is, and between the two where some are.
#[test]
fn a_place_reads_the_fine_light_where_it_is_held() {
    let bank = solved(
        HEAPS,
        high_sun(),
        (12_000.0, COARSE),
        &tairix_parallel::Threaded::new(4),
    );
    let (mut fine, mut coarse, mut between) = (0, 0, 0);
    for volume in &bank.lights.volumes {
        let grid = volume.grid;
        for index in 0..3000u32 {
            let draw = |salt: u32| unit(mix32(index.wrapping_mul(0x9e37_79b9) ^ salt));
            let at = (
                bank.centre.0 + (2.0 * draw(1) - 1.0) * 0.98 * grid.half,
                bank.floor + draw(2) * (bank.ceiling - bank.floor),
                bank.centre.1 + (2.0 * draw(3) - 1.0) * 0.98 * grid.half,
            );
            let reading = volume.reading(&bank, at).expect("on the level");
            let held = grid
                .cube(
                    grid.place_in(&bank, at, grid.fine).expect("on the level"),
                    grid.fine,
                )
                .iter()
                .filter(|&&(cell, weight)| {
                    weight > 0.0 && volume.fine.runs.fine(grid, cell).is_some()
                })
                .count();
            let corners = grid.corners(grid.place(&bank, at).expect("on the level"));
            let voxels = volume.fluence[SUNLIT].read(&corners);
            if held == 0 {
                assert!((reading.fluence[SUNLIT] - voxels).abs() <= 1e-9 * voxels.abs().max(1.0));
                coarse += 1;
            } else if held == 8 {
                fine += 1;
            } else {
                between += 1;
            }
            assert!(reading
                .fluence
                .iter()
                .all(|&value| value.is_finite() && value >= 0.0));
        }
    }
    assert!(
        fine > 50 && coarse > 50 && between > 20,
        "{fine} fine, {coarse} coarse, {between} between"
    );
}

/// A bank of heaps holds its light finely on the levels within its reach
/// and in its voxels past it; a bank of sheets holds it in its voxels
/// throughout.
#[test]
fn heaps_are_lit_finely_within_their_reach_and_sheets_not_at_all() {
    let resolution = Resolution {
        reach: 8000.0,
        ..COARSE
    };
    for (deck, heaped) in [(HEAPS, true), (OVERCAST, false)] {
        let mut bank = Cloudbank::new(
            [Some(deck), None],
            (12_000.0, resolution),
            high_sun(),
            GROUND * 1000.0,
        )
        .expect("fits");
        bank.stand(Vec3::ZERO, SCREEN_PIXEL).expect("levels fit");
        assert!(bank.lights.volumes.len() >= 3);
        for volume in &bank.lights.volumes {
            let fine = heaped && volume.grid.half <= resolution.reach;
            assert_eq!(
                volume.grid.fine,
                if fine { COARSE.fine } else { 1 },
                "{}",
                volume.grid.half
            );
        }
    }
}

/// A level's fields move on pass by pass to the stage after them, and a
/// field over no cells at all, as a level holding no fine light has, moves
/// straight there.
#[test]
fn a_field_over_no_cells_moves_straight_on() {
    let at = |pass, block| Stage::Fine(0, pass, block);
    let then = Stage::Maps(0, 0);
    assert_eq!(passed((0, 0, 0), at, then), then);
    assert_eq!(passed((0, 5, 10), at, then), Stage::Fine(0, 0, 5));
    assert_eq!(passed((0, 10, 10), at, then), Stage::Fine(0, 1, 0));
    assert_eq!(passed((FIELD_PASSES - 1, 10, 10), at, then), then);
}

/// A level's tallies are laid at nought a share at a time, no more than one
/// for each core and each in a tally of its own, within the room reserved
/// for them, until every one holds exactly its cells.
#[test]
fn a_levels_tallies_are_laid_at_nought_a_share_at_a_time() {
    let grid = Grid {
        cells: 96,
        layers: 32,
        fine: 1,
        ..Grid::default()
    };
    let mut tally = Tally::reserved(&Volume {
        grid,
        ..Volume::default()
    })
    .expect("fits");
    let rooms: Vec<usize> = tally
        .each_mut()
        .map(|(values, _)| values.capacity())
        .collect();
    let laid =
        |tally: &mut Tally| -> usize { tally.each_mut().map(|(values, _)| values.len()).sum() };
    let runner = tairix_parallel::Threaded::new(3);
    let mut before = laid(&mut tally);
    let mut units = 0;
    let done = loop {
        units += 1;
        let whole = tally.lay(&runner);
        let after = laid(&mut tally);
        assert!(after > before, "unit {units} laid nothing");
        assert!(
            after - before <= 3 * TALLY_CELLS,
            "unit {units} laid {}",
            after - before
        );
        before = after;
        if whole || units > 64 {
            break whole;
        }
    };
    assert!(done, "laid in {units} units");
    let floor = grid.floor_cells() * grid.floor_cells();
    for ((values, cells), room) in tally.each_mut().zip(rooms) {
        assert!(cells == grid.voxels() || cells == floor || cells == 0);
        assert_eq!(values.len(), cells);
        assert_eq!(values.capacity(), room, "laid within its room");
        assert!(values
            .iter()
            .all(|value| value.load(Ordering::Relaxed) == 0));
    }
}

/// A column's runs of layers holding cloud are found bit by bit across its
/// words: from mid-word, over a word's edge, and up to a top that is or is
/// not a word's edge.
#[test]
fn a_columns_layer_bits_are_searched_across_its_words() {
    let bits = [1u64 << 63 | 1 << 2, u64::MAX >> 60, 0];
    assert_eq!(next_bit(&bits, 0, true), Some(2));
    assert_eq!(next_bit(&bits, 2, false), Some(3));
    assert_eq!(next_bit(&bits, 3, true), Some(63));
    assert_eq!(next_bit(&bits, 63, false), Some(68), "over the word's edge");
    assert_eq!(next_bit(&bits, 68, true), None);
    assert_eq!(
        next_bit(&[u64::MAX], 5, false),
        None,
        "up to the last word's top"
    );
    assert_eq!(next_bit(&[], 0, true), None);
    let grid = Grid {
        cells: 2,
        layers: 70,
        fine: 2,
        ..Grid::default()
    };
    // Column (1, 0) holds layers 2, 63 to 67 and 69; the rest hold none.
    let mut layered = alloc::vec![0u64; 4 * grid.layer_words()];
    layered[grid.layer_words()] = bits[0];
    layered[grid.layer_words() + 1] = bits[1] | 1 << 5;
    let (runs, voxels) = Runs::holding(grid, &layered).expect("fits");
    assert_eq!(runs.blocks, 7);
    let held: Vec<usize> = (0..grid.layers)
        .filter(|&k| runs.block(grid, (1, k, 0)).is_some())
        .collect();
    assert_eq!(held, [2, 63, 64, 65, 66, 67, 69]);
    for (block, &k) in held.iter().enumerate() {
        assert_eq!(runs.block(grid, (1, k, 0)), Some(block));
        assert_eq!(voxels[block] as usize, grid.index((1, k, 0)));
    }
    assert!((0..grid.layers).all(|k| runs.block(grid, (0, k, 1)).is_none()));
}

/// A picture whose pixels a fine cell would not span where its level begins
/// holds a bank's light in its voxels however heaped the bank, and one whose
/// pixels it would, finely.
#[test]
fn a_picture_too_coarse_for_its_fine_cells_holds_the_light_in_voxels() {
    let resolution = Resolution {
        reach: f64::INFINITY,
        ..COARSE
    };
    let narrowest = 4.0 / real(COARSE.cells * COARSE.fine);
    for (pixel, fine) in [(0.9 * narrowest, COARSE.fine), (1.1 * narrowest, 1)] {
        let mut bank = Cloudbank::new(
            [Some(HEAPS), None],
            (12_000.0, resolution),
            high_sun(),
            GROUND * 1000.0,
        )
        .expect("fits");
        bank.stand(Vec3::ZERO, pixel).expect("levels fit");
        assert!(
            bank.lights
                .volumes
                .iter()
                .all(|volume| volume.grid.fine == fine),
            "pixel {pixel}"
        );
    }
}

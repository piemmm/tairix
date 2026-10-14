//! Host tests of a bank's light: it comes out the same however many cores
//! solve it, photons meet only cloud that stands, their light is counted in
//! the units it falls in, and the sun's optical depth deepens through cloud.

use super::*;
use crate::atmosphere::GROUND;
use crate::cloud::{Deck, Lighting, Matter, SUNLIGHT_LEVELS, SUN_COSINES};
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

/// Light solved coarsely enough for a test to solve it many times over.
const COARSE: Resolution = Resolution {
    cells: 16,
    finest: 4000.0,
    tracks: 24.0,
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
    bank.stand(Vec3::ZERO).expect("levels fit");
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
        for (a, b) in a
            .fluence
            .iter()
            .zip(&b.fluence)
            .chain(a.flux.iter().zip(&b.flux))
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
        let mut solving = begin(grid, &bank).expect("fits");
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
    bank.stand(Vec3::ZERO).expect("levels fit");
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

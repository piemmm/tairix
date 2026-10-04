//! Host tests of volumetric cloud: its noises tile, a ray meets the bank
//! where it lies — on over the Earth's curve to the horizon — and not past
//! it, its levels meet without a seam, and its shadow is a transmittance.

use super::*;

const CUMULUS: Deck = Deck {
    base: 1200.0,
    base_spread: 150.0,
    depth: (600.0, 1800.0),
    cover: 0.45,
    heap: 1.0,
    scale: 2500.0,
    stretch: 1.2,
    heading: 0.4,
    thickness: 0.05,
    billow: 400.0,
    fibre: 1.0,
    matter: Matter::Water,
    seed: 17,
};

/// A ceiling of cloud from horizon to horizon.
const OVERCAST: Deck = Deck {
    cover: 1.0,
    heap: 0.3,
    ..CUMULUS
};

/// A middle deck of small billows above the heaps.
const ALTOCUMULUS: Deck = Deck {
    base: 4200.0,
    base_spread: 120.0,
    depth: (150.0, 400.0),
    cover: 0.4,
    heap: 0.2,
    scale: 3000.0,
    stretch: 2.5,
    heading: 0.5,
    thickness: 0.02,
    billow: 260.0,
    fibre: 1.0,
    matter: Matter::Water,
    seed: 41,
};

/// Cirrus: thin streaks of ice.
const CIRRUS: Deck = Deck {
    base: 8000.0,
    base_spread: 200.0,
    depth: (600.0, 1200.0),
    cover: 0.45,
    heap: 0.1,
    scale: 2000.0,
    stretch: 6.0,
    heading: 0.5,
    thickness: 1e-3,
    billow: 250.0,
    fibre: 8.0,
    matter: Matter::Ice,
    seed: 23,
};

/// The scene's level's distance from the Earth's centre: at the sea.
const LEVEL: f64 = GROUND * 1000.0;

/// The most the finest level spans for a test that looks at the near bank
/// alone, and for one that looks out to the horizon through every level.
const BROAD: f64 = 200_000.0;
const FINE: f64 = 12_000.0;

/// `bank` lit by an even sun from `toward` and an even sky.
fn lit(mut bank: Cloudbank) -> Cloudbank {
    let toward = bank.sun;
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(20.0); SUNLIGHT_LEVELS * SUN_COSINES],
        cosines: (-1.0, 1.0),
        toward,
        above: Vec3::new(0.6, 0.8, 1.2),
        below: Vec3::splat(0.3),
    });
    bank
}

/// A bank of `decks` lit from `sun`, its finest level spanning no more
/// than `most`, stood about an eye at the scene's middle and built across
/// `runner`, beneath `above` if it lies beneath one.
fn stood(
    decks: [Option<Deck>; 2],
    (sun, most): (Vec3, f64),
    runner: &dyn JobRunner,
    above: Option<&Cloudbank>,
) -> Cloudbank {
    let mut bank = Cloudbank::new(decks, most, sun, LEVEL).expect("fits");
    bank.stand(Vec3::ZERO).expect("levels fit");
    while !bank.step(runner, above).expect("held") {}
    lit(bank)
}

/// A bank of `deck` alone, as [`stood`] lays one.
fn bank_of(deck: Deck, sun: Vec3, above: Option<&Cloudbank>) -> Cloudbank {
    stood(
        [Some(deck), None],
        (sun, BROAD),
        &tairix_parallel::SERIAL,
        above,
    )
}

/// The sun the cumulus tests are lit from.
fn high_sun() -> Vec3 {
    Vec3::new(0.3, 0.8, 0.2).normalized()
}

fn built(runner: &dyn JobRunner) -> Cloudbank {
    stood([Some(CUMULUS), None], (high_sun(), BROAD), runner, None)
}

/// A bank of heaps and a middle deck above them out to the horizon, laid in
/// every level a fine finest one asks for.
fn far(runner: &dyn JobRunner) -> Cloudbank {
    stood(
        [Some(CUMULUS), Some(ALTOCUMULUS)],
        (high_sun(), FINE),
        runner,
        None,
    )
}

/// The cloud a ray meets, if any: its light, what shows through, and its
/// distance.
fn cloud(bank: &Cloudbank, origin: Vec3, dir: Vec3, fine: bool) -> Option<(Vec3, f64, f64)> {
    bank.seen(origin, dir, fine, 0.5)
        .and_then(|seen| seen.cloud)
}

/// The unit way `elevation` radians up toward `around`.
fn heading(around: f64, elevation: f64) -> Vec3 {
    Vec3::new(
        mathf::cos(elevation) * mathf::cos(around),
        mathf::sin(elevation),
        mathf::cos(elevation) * mathf::sin(around),
    )
}

#[test]
fn the_noises_repeat_across_their_period() {
    for step in 0..24u32 {
        let p = Vec3::new(
            f64::from(step) * 0.37,
            f64::from(step) * 0.91,
            f64::from(step) * 1.53,
        );
        let shifted = p + Vec3::new(4.0, 8.0, 12.0);
        assert!((worley_tiled(p, 4.0, 3) - worley_tiled(shifted, 4.0, 3)).abs() < 1e-9);
        assert!((perlin_tiled(p, 4.0, 3) - perlin_tiled(shifted, 4.0, 3)).abs() < 1e-9);
    }
}

#[test]
fn rays_up_through_the_bank_meet_cloud_and_rays_past_its_reach_do_not() {
    let bank = built(&tairix_parallel::SERIAL);
    let mut met = 0;
    let mut hidden = 0;
    for step in 0..400u32 {
        let x = f64::from(step % 20) * 400.0 - 4000.0;
        let z = f64::from(step / 20) * 400.0 - 4000.0;
        if let Some((light, kept, depth)) = cloud(&bank, Vec3::new(x, 0.0, z), Vec3::UP, true) {
            met += 1;
            assert!(light.is_finite() && light.x.min(light.y).min(light.z) >= 0.0);
            assert!((0.0..=1.0).contains(&kept));
            assert!(depth > 900.0 && depth < 4000.0, "{depth}");
            if kept < 0.1 {
                hidden += 1;
            }
        }
    }
    assert!(met > 40, "{met} of 400 columns hold cloud");
    assert!(hidden > 10, "{hidden} are thick enough to hide the sky");
    assert!(met < 400, "and some are clear");
    let overhead = bank
        .seen(Vec3::ZERO, Vec3::UP, true, 0.5)
        .expect("runs through the bank");
    assert!(
        (overhead.leave - bank.ceiling).abs() < 1e-6,
        "{}",
        overhead.leave
    );
    let beyond = Vec3::new(
        2.0 * bank.levels.last().map_or(0.0, |outer| outer.half),
        0.0,
        0.0,
    );
    assert!(
        bank.seen(beyond, Vec3::UP, true, 0.5).is_none(),
        "past its reach"
    );
}

#[test]
fn the_bank_shades_some_of_the_ground_and_never_brightens_it() {
    let bank = built(&tairix_parallel::SERIAL);
    let sun = high_sun();
    let mut shaded = 0;
    for step in 0..900u32 {
        let x = f64::from(step % 30) * 300.0 - 4500.0;
        let z = f64::from(step / 30) * 300.0 - 4500.0;
        let kept = bank.shadow(Vec3::new(x, 0.0, z), sun);
        assert!((0.0..=1.0 + 1e-6).contains(&kept), "{kept}");
        if kept < 0.5 {
            shaded += 1;
        }
    }
    assert!(shaded > 50, "{shaded} of 900 points lie in cloud shadow");
    assert!(
        (bank.shadow(Vec3::new(0.0, 9_000.0, 0.0), sun) - 1.0).abs() < 1e-12,
        "nothing above the bank is shaded"
    );
}

#[test]
fn a_bank_built_across_workers_matches_one_built_alone() {
    // Two decks over two levels, so a level's rim is read from the next.
    let laid = |runner: &dyn JobRunner| {
        stood(
            [Some(CUMULUS), Some(ALTOCUMULUS)],
            (high_sun(), BROAD),
            runner,
            None,
        )
    };
    let (alone, spread) = (
        laid(&tairix_parallel::SERIAL),
        laid(&tairix_parallel::Threaded::new(3)),
    );
    assert!(alone.levels.len() >= 2, "{} levels", alone.levels.len());
    for step in 0..60u32 {
        let origin = Vec3::new(
            f64::from(step) * 97.0 - 2400.0,
            5.0,
            f64::from(step) * -61.0,
        );
        // Low rays among them, which run on through the coarser levels.
        let dir = heading(f64::from(step) * 0.7, 0.002 + 0.01 * f64::from(step % 7));
        assert_eq!(
            alone.seen(origin, dir, true, 0.25),
            spread.seen(origin, dir, true, 0.25)
        );
        assert_eq!(
            alone.shadow(origin, dir).to_bits(),
            spread.shadow(origin, dir).to_bits()
        );
    }
}

/// What fraction of the light from beyond a ray's stretch through the bank
/// comes through it, integrated in `step`-metre steps: the reference a
/// march must agree with.
fn integrated(bank: &Cloudbank, origin: Vec3, dir: Vec3, step: f64) -> f64 {
    let Some((enter, leave)) = bank.crossing(origin, dir) else {
        return 1.0;
    };
    let reader = Reader { bank };
    let mut optical = 0.0;
    let mut t = enter + 0.5 * step;
    while t < leave && optical < 10.0 {
        if let Some(sample) = reader.density(origin + dir * t, true) {
            optical += sample.density * sample.thickness * step;
        }
        t += step;
    }
    mathf::exp(-optical)
}

/// Seen from the ground across the bank, low and high, a march lets through
/// what the cloud it crosses lets through: were its steps long enough to fall
/// wherever they happened to at a cloud's edge, neighbouring rays would show
/// that edge in slices, and some would miss clouds altogether.
#[test]
fn a_march_through_the_bank_agrees_with_one_taken_in_fine_steps() {
    let bank = built(&tairix_parallel::SERIAL);
    let (mut crossed, mut worst) = (0u32, 0.0f64);
    for step in 0..240u32 {
        let rise = 0.03 + 0.9 * f64::from(step % 40) / 40.0;
        let dir = heading(f64::from(step) * 2.399_963, mathf::atan(rise));
        let origin = Vec3::new(
            f64::from(step % 13) * 300.0 - 1800.0,
            2.0,
            f64::from(step % 11) * -250.0,
        );
        let reference = integrated(&bank, origin, dir, 2.0);
        let kept = cloud(&bank, origin, dir, true).map_or(1.0, |(_, kept, _)| kept);
        // Past the transmittance a march takes as opaque, both hide the sky.
        let expected = if reference < OPAQUE { 0.0 } else { reference };
        worst = worst.max((kept - expected).abs());
        crossed += u32::from(reference < 0.9);
    }
    assert!(crossed > 40, "{crossed} of 240 rays cross cloud");
    assert!(
        worst < 0.06,
        "a march strays {worst} from the cloud it crosses"
    );
}

/// From the ground, a deck runs on toward the horizon as the Earth's curve
/// carries it: a ray however low meets it, and the lower the further off,
/// out past where a flat bank of the old breadth stopped; a ray below the
/// horizon meets the Earth instead.
#[test]
fn the_deck_runs_on_to_the_horizon() {
    let bank = stood(
        [Some(OVERCAST), None],
        (high_sun(), FINE),
        &tairix_parallel::Threaded::new(4),
        None,
    );
    let eye = Vec3::new(0.0, 2.0, 0.0);
    for step in 0..64u32 {
        let around = f64::from(step) * 0.41;
        let mut last = 0.0;
        for elevation in [3.0, 1.5, 0.8, 0.4, 0.15, 0.05] {
            let dir = heading(around, f64::to_radians(elevation));
            let (_, kept, depth) = cloud(&bank, eye, dir, false)
                .unwrap_or_else(|| panic!("{around} at {elevation}° meets the deck"));
            assert!(
                kept < 0.05,
                "{around} at {elevation}°: {kept} shows through"
            );
            assert!(
                depth > last,
                "{around} at {elevation}°: {depth} after {last}"
            );
            last = depth;
        }
        assert!(
            last > 60_000.0,
            "{around}: the deck at the horizon lies {last} off"
        );
        let below = heading(around, f64::to_radians(-0.5));
        assert!(
            cloud(&bank, eye, below, false).is_none(),
            "{around}: beneath the horizon"
        );
    }
}

/// A height over the Earth's curve and the point it lies at agree, and the
/// deck's floor falls away below the eye's level as the curve has it.
#[test]
fn a_deck_lies_along_the_earths_curve() {
    let bank = built(&tairix_parallel::SERIAL);
    for step in 0..200u32 {
        let (x, z) = (
            (f64::from(step) - 100.0) * 1_700.0,
            (f64::from(step % 17) - 8.0) * 9_000.0,
        );
        let height = f64::from(step % 9) * 1_500.0 - 200.0;
        let y = bank.height_at(height, (x, z));
        let measured = bank.altitude(Vec3::new(x, y, z));
        assert!(
            (measured - height).abs() < 1e-6,
            "{x} {z}: {measured} for {height}"
        );
    }
    let off = 100_000.0;
    let fallen = bank.floor - bank.height_at(bank.floor, (off, 0.0));
    let curve = off * off / (2.0 * (LEVEL + bank.floor));
    assert!((fallen - curve).abs() < 1.0, "{fallen} against {curve}");
}

/// Deck `slot`'s column at `(x, z)`, read from the finest level over it.
fn column_at(bank: &Cloudbank, slot: usize, (x, z): (f64, f64)) -> Option<Column> {
    let level = bank.level_over(Vec3::new(x, 0.0, z))?;
    let place = place_on(bank.centre, level.half, (x, z), WEATHER_CELLS);
    let (corners, across) = about(level.weather.get(slot)?, WEATHER_CELLS + 1, place)?;
    Some(Blend { corners, across }.column())
}

/// Either side of where one level gives way to the next, the weather, the
/// sun's optical depth and the shadow read alike: a level's rim is the next
/// level's own reading, so no seam shows where they meet.
#[test]
fn the_levels_meet_without_a_seam() {
    let bank = far(&tairix_parallel::Threaded::new(4));
    let reader = Reader { bank: &bank };
    assert!(bank.levels.len() >= 4, "{} levels", bank.levels.len());
    let mut compared = 0u32;
    for pair in bank.levels.windows(2) {
        let edge = pair[0].half;
        for step in 0..400u32 {
            // Along each of the edge's four sides, a hair inside and outside.
            let along = (f64::from(step / 4) / 100.0 * 2.0 - 1.0) * edge * 0.999;
            let side = |offset: f64| match step % 4 {
                0 => (offset, along),
                1 => (-offset, along),
                2 => (along, offset),
                _ => (along, -offset),
            };
            let (inside, outside) = (side(edge * (1.0 - 1e-9)), side(edge * (1.0 + 1e-9)));
            for slot in 0..2 {
                let (Some(a), Some(b)) = (
                    column_at(&bank, slot, inside),
                    column_at(&bank, slot, outside),
                ) else {
                    panic!("{edge}: both sides lie on the bank");
                };
                assert!(
                    (a.cover - b.cover).abs() < 1e-4
                        && (a.base - b.base).abs() < 0.05
                        && (a.top - b.top).abs() < 0.05,
                    "{edge} at {inside:?}: {a:?} against {b:?}"
                );
            }
            let height = f64::midpoint(bank.floor, bank.ceiling);
            let point = |(x, z): (f64, f64)| Vec3::new(x, bank.height_at(height, (x, z)), z);
            let (a, b) = (
                reader.light_at(point(inside)),
                reader.light_at(point(outside)),
            );
            assert!(
                (a - b).abs() < 1e-3 * (1.0 + a.abs()),
                "{edge}: light {a} against {b}"
            );
            let floor =
                |(x, z): (f64, f64)| Vec3::new(x, bank.height_at(bank.floor - 10.0, (x, z)), z);
            let up = Vec3::UP;
            let (a, b) = (
                bank.shadow(floor(inside), up),
                bank.shadow(floor(outside), up),
            );
            assert!((a - b).abs() < 1e-3, "{edge}: shadow {a} against {b}");
            compared += 1;
        }
    }
    assert!(compared > 1000, "{compared} places compared");
}

/// However coarse a level, no cloud stands outside the band its weather
/// cell holds: a march jumping a cell's clear air jumps none.
#[test]
fn no_cloud_stands_outside_its_weather_cells_band() {
    for bank in [
        far(&tairix_parallel::Threaded::new(4)),
        bank_of(CIRRUS, Vec3::UP, None),
    ] {
        let reader = Reader { bank: &bank };
        let (mut cloudy, mut clear) = (0u32, 0u32);
        for (index, level) in bank.levels.iter().enumerate() {
            for sample in 0..20_000u32 {
                let key = mix32(sample ^ (u32::try_from(index).unwrap_or(0) << 24));
                let across = |salt: u32| level.half * (2.0 * unit(mix32(key ^ salt)) - 1.0);
                let (x, z) = (
                    bank.centre.0 + across(0x51ed_270b),
                    bank.centre.1 + across(0x2545_f491),
                );
                let height =
                    bank.floor + (bank.ceiling - bank.floor) * unit(mix32(key ^ 0x9e37_79b9));
                let point = Vec3::new(x, bank.height_at(height, (x, z)), z);
                let cursor = bank.cursor(point, Vec3::UP, 0.0);
                let jumped = bank
                    .over(&cursor)
                    .clear(&bank.course(point, Vec3::UP), height, 0.0)
                    .is_some();
                clear += u32::from(jumped);
                if reader.density(point, false).is_some() {
                    cloudy += 1;
                    assert!(!jumped, "cloud at {point:?} lies outside its band");
                }
            }
        }
        assert!(cloudy > 1000, "{cloudy} points hold cloud");
        assert!(clear > 10_000, "and a march may jump {clear}");
    }
}

/// All along a ray, whichever way it heads, its march stands over the
/// finest level that holds each point of it and over the cell beneath the
/// point there, however far apart the points it is advanced to lie.
#[test]
fn a_march_stands_over_the_cell_beneath_each_point_of_it_on_every_level() {
    let bank = far(&tairix_parallel::Threaded::new(4));
    let mut checked = 0u32;
    for index in 0..300u32 {
        let around = f64::from(index) * 2.399_963;
        // Now and then straight along a line of cells, east, west, north or
        // south; and some inward from far off, crossing into finer levels.
        let toward = match index % 40 {
            0 => 0.0,
            1 => core::f64::consts::PI,
            2 => 0.5 * core::f64::consts::PI,
            3 => 1.5 * core::f64::consts::PI,
            _ => around,
        };
        let dir = heading(toward, 0.001 + 0.002 * f64::from(index % 5));
        let out = 1.0e3 * f64::from(index % 13) + if index % 3 == 0 { 90_000.0 } else { 0.0 };
        let origin = Vec3::new(-out * mathf::cos(toward), 1500.0, -out * mathf::sin(toward));
        let Some((enter, leave)) = bank.crossing(origin, dir) else {
            continue;
        };
        let mut cursor = bank.cursor(origin, dir, enter);
        let mut t = enter;
        while t < leave {
            bank.advance(&mut cursor, origin, dir, t);
            assert!(cursor.walk.until() > t, "ray {index} at {t}");
            let point = origin + dir * t;
            if let (Some(level), Some(at)) = (bank.levels.get(cursor.level), cursor.walk.cell()) {
                let (east, north) =
                    place_on(bank.centre, level.half, (point.x, point.z), WEATHER_CELLS);
                // On a line of cells or a level's edge, rounding may put a
                // point either side.
                let between =
                    |place: f64| (1e-6..1.0 - 1e-6).contains(&(place - mathf::floor(place)));
                if between(east) && between(north) {
                    assert_eq!(
                        Some(cursor.level),
                        bank.finest((point.x, point.z)),
                        "ray {index} at {t}"
                    );
                    assert_eq!(
                        at,
                        (cell_of(east).0, cell_of(north).0),
                        "ray {index} at {t}"
                    );
                    checked += 1;
                }
            }
            // Strides short and long, some across several cells at once.
            t += 23.0 + f64::from(index % 5) * 611.0;
        }
    }
    assert!(checked > 5000, "{checked} points checked");
}

/// However the samples of a pixel stagger a march, it finds the same cloud.
#[test]
fn a_march_sees_the_same_cloud_however_its_steps_are_staggered() {
    let bank = built(&tairix_parallel::SERIAL);
    for step in 0..120u32 {
        let dir = heading(
            f64::from(step) * 1.1,
            mathf::atan(0.05 + 0.01 * f64::from(step % 9)),
        );
        let origin = Vec3::new(f64::from(step % 7) * 400.0, 2.0, -1500.0);
        let kept = |jitter: f64| {
            bank.seen(origin, dir, true, jitter)
                .and_then(|seen| seen.cloud)
                .map_or(1.0, |(_, kept, _)| kept)
        };
        let (low, high) = (kept(0.05), kept(0.95));
        assert!(
            (low - high).abs() < 0.08,
            "ray {step}: {low} or {high} by its stagger"
        );
    }
}

#[test]
fn a_coarse_ray_reaches_cloud_beyond_however_many_clear_columns_it_crosses() {
    let mut bank = far(&tairix_parallel::Threaded::new(4));
    // Clear every level of the bank but where it lies past 8 km east, past
    // hundreds of clear columns a low ray from the west must cross.
    let centre = bank.centre;
    for index in 0..bank.levels.len() {
        let Some(level) = bank.levels.get_mut(index) else {
            continue;
        };
        let half = level.half;
        for map in &mut level.weather {
            for (place, column) in map.iter_mut().enumerate() {
                let i = place % (WEATHER_CELLS + 1);
                let (x, _) = vertex_of(centre, half, (i, 0), WEATHER_CELLS);
                if x < 8_000.0 {
                    column.cover = 0.0;
                }
            }
        }
        bank.fill_bands(index, 0..WEATHER_CELLS, &tairix_parallel::SERIAL);
    }
    let mut met = [0; 2];
    for step in 0..24u32 {
        // Rays level with the deck's middle, fanned across the stripe.
        let origin = Vec3::new(-11_500.0, 1_700.0, f64::from(step) * 300.0 - 3600.0);
        let dir = Vec3::new(1.0, 0.002, 0.0).normalized();
        for (fine, met) in [true, false].into_iter().zip(&mut met) {
            if let Some((_, _, depth)) = cloud(&bank, origin, dir, fine) {
                *met += 1;
                assert!(depth > 19_000.0, "{fine}: {depth}");
            }
        }
    }
    // A coarse ray once spent its steps on the clear columns and met nothing.
    assert!(
        met.iter().all(|&met| met > 6),
        "of 24 rays, fine and coarse meet {met:?}"
    );
}

#[test]
fn the_highest_a_cloud_stands_is_where_its_threshold_meets_its_billows_peak() {
    for (cover, heap, peak) in [
        (0.3, 0.1, 0.9),
        (0.6, 1.0, 0.95),
        (0.9, 0.5, 1.0),
        (0.05, 0.2, 0.97),
    ] {
        let within = highest(cover, heap, peak).expect("billows pass the threshold at the base");
        assert!(
            (threshold(cover, within, heap) - peak).abs() < 1e-12,
            "{cover} {heap} {peak}: {within}"
        );
    }
    assert!(
        highest(0.05, 0.5, 0.94).is_none(),
        "the cover leaves no billow standing"
    );
    assert!(highest(0.0, 0.5, 1.0).is_none());
}

/// A far cloud takes the sun as it stands over that cloud: higher toward
/// the sun than over the eye, lower away from it. Lit only where the sun
/// stands higher than over the eye, a thin overcast is lit far off toward
/// the sun as by a sun lit everywhere, and dark far off away from it.
#[test]
fn a_far_cloud_is_lit_by_the_sun_at_its_own_place() {
    let sun = heading(0.0, f64::to_radians(30.0));
    let thin = Deck {
        thickness: 0.002,
        ..OVERCAST
    };
    let mut bank = Cloudbank::new([Some(thin), None], FINE, sun, LEVEL).expect("fits");
    bank.stand(Vec3::ZERO).expect("levels fit");
    while !bank
        .step(&tairix_parallel::Threaded::new(4), None)
        .expect("held")
    {}
    let cosines = bank.sun_cosines(sun);
    let outer = bank.levels.last().map_or(0.0, |outer| outer.half);
    for step in 0..400u32 {
        let (u, v) = (unit(mix32(step)), unit(mix32(step ^ 0x5bd1)));
        let (x, z) = (outer * (2.0 * u - 1.0), outer * (2.0 * v - 1.0));
        let height = bank.floor + (bank.ceiling - bank.floor) * unit(mix32(step ^ 0x77));
        let point = Vec3::new(x, bank.height_at(height, (x, z)), z);
        let cosine = bank.cosine(point, height, sun);
        assert!(
            (cosines.0..=cosines.1).contains(&cosine),
            "{point:?}: {cosine} past {cosines:?}"
        );
    }
    let light_by = |bank: &mut Cloudbank, higher_only: bool| {
        let sunlight = sunlight_places(bank.span(), cosines).map(|(_, cosine)| {
            Vec3::splat(if !higher_only || cosine > sun.y {
                20.0
            } else {
                0.0
            })
        });
        bank.light_by(Lighting {
            sunlight: fallible::collected(SUNLIGHT_LEVELS * SUN_COSINES, sunlight).expect("fits"),
            cosines,
            toward: sun,
            above: Vec3::ZERO,
            below: Vec3::ZERO,
        });
    };
    let eye = Vec3::new(0.0, 2.0, 0.0);
    for (around, sunward) in [(0.0, true), (core::f64::consts::PI, false)] {
        let dir = heading(around, f64::to_radians(1.0));
        light_by(&mut bank, false);
        let (everywhere, _, depth) = cloud(&bank, eye, dir, false).expect("meets the deck");
        light_by(&mut bank, true);
        let (higher, _, _) = cloud(&bank, eye, dir, false).expect("meets the deck");
        assert!(everywhere.y > 1e-3, "{around}: {everywhere:?} at {depth}");
        if sunward {
            assert!(
                (higher - everywhere).length() < 1e-9 * everywhere.length(),
                "{higher:?}"
            );
        } else {
            assert!(
                higher.y < 1e-12,
                "{around}: {higher:?} where the sun stands lower"
            );
        }
    }
}

/// Under a sun low enough that its way rises through the deck far off, the
/// ground beneath an overcast lies in the overcast's shadow; under a sun set
/// beneath the Earth, the cloud shades nothing, the air saying it is dark.
#[test]
fn far_cloud_shades_the_ground_from_a_low_sun() {
    let low = heading(0.0, f64::to_radians(1.0));
    let bank = stood(
        [Some(OVERCAST), None],
        (low, FINE),
        &tairix_parallel::Threaded::new(4),
        None,
    );
    for step in 0..20u32 {
        let ground = Vec3::new(f64::from(step) * 500.0 - 5000.0, 0.0, 300.0);
        let kept = bank.shadow(ground, low);
        assert!(
            kept < 0.05,
            "{ground:?}: {kept} of the low sun gets through"
        );
        let set = heading(0.0, f64::to_radians(-5.0));
        assert!((bank.shadow(ground, set) - 1.0).abs() < 1e-12);
    }
}

#[test]
fn a_profile_holds_its_whole_depth_at_its_ceiling() {
    let levels = core::array::from_fn(|level| 1e-3 * (1.0 + real(level)));
    let profile = Profile::new(levels, (8000.0, 9500.0));
    // Extinction rising evenly up the slab, from its lowest level to its
    // highest.
    let whole = 1500.0 * f64::midpoint(levels[0], levels[SCATTER_LEVELS - 1]);
    assert!((profile.depth(9500.0) - whole).abs() < 1e-9 * whole);
    assert!((profile.at(9500.0) - levels[SCATTER_LEVELS - 1]).abs() < 1e-15);
    assert!(profile.depth(8000.0).abs() < 1e-15);
}

#[test]
fn ice_sends_the_skys_light_on_down_to_an_eye_below_and_little_back_up() {
    let bank = bank_of(CIRRUS, Vec3::UP, None);
    // The forward half of the phase holds `(1 − g²)/(2g) (1/(1 − g) −
    // 1/√(1 + g²))` of its light: fourteen fifteenths at `g = 0.75`.
    let g = ICE_ASYMMETRY;
    let forward = (1.0 - g * g) / (2.0 * g) * (1.0 / (1.0 - g) - 1.0 / mathf::sqrt(1.0 + g * g));
    let (down, level, up) = (
        bank.sent_down(-Vec3::UP),
        bank.sent_down(Vec3::new(1.0, 0.0, 0.0)),
        bank.sent_down(Vec3::UP),
    );
    assert!((down - forward).abs() < 2e-3, "{down} against {forward}");
    assert!((up - (1.0 - forward)).abs() < 2e-3, "{up}");
    assert!((level - 0.5).abs() < 2e-3, "{level}");
    let mut last = 1.0;
    for step in 0..=32u32 {
        let rise = f64::from(step) / 16.0 - 1.0;
        let out = Vec3::new(mathf::sqrt((1.0 - rise * rise).max(0.0)), rise, 0.0);
        let share = bank.sent_down(out);
        assert!(share <= last + 1e-6, "{rise}: {share} after {last}");
        last = share;
    }
}

#[test]
fn ice_scatters_its_sunlight_again_the_more_the_denser_it_lies() {
    let sun = Vec3::new(0.3, 0.9, 0.1).normalized();
    let thin = bank_of(CIRRUS, sun, None);
    let thick = bank_of(
        Deck {
            thickness: 4.0 * CIRRUS.thickness,
            ..CIRRUS
        },
        sun,
        None,
    );
    for level in 0..SCATTER_LEVELS {
        let (a, b) = (thin.scatter[level], thick.scatter[level]);
        assert!(a.is_finite() && a >= 0.0 && b >= a, "{level}: {a} {b}");
    }
    assert!(thick.scattered(0.5) > 2.0 * thin.scattered(0.5));
    assert!(
        built(&tairix_parallel::SERIAL)
            .scatter
            .iter()
            .all(|&value| value <= 0.0),
        "water has its own octaves"
    );
}

#[test]
fn ice_is_drawn_out_along_its_heading() {
    let bank = bank_of(CIRRUS, Vec3::UP, None);
    let reader = Reader { bank: &bank };
    let (cos, sin) = (mathf::cos(CIRRUS.heading), mathf::sin(CIRRUS.heading));
    let at = |along: f64, across: f64, height: f64| {
        let (x, z) = (along * cos - across * sin, along * sin + across * cos);
        Vec3::new(x, bank.height_at(height, (x, z)), z)
    };
    let density = |point: Vec3| {
        reader
            .density(point, true)
            .map_or(0.0, |sample| sample.density)
    };
    let (mut along, mut across) = (0.0, 0.0);
    // Low in the deck, where its streaks stand thickest.
    for height in [8_050.0, 8_150.0, 8_250.0] {
        for step in 0..900u32 {
            let (a, b) = (
                f64::from(step % 30) * 211.0 - 3000.0,
                f64::from(step / 30) * 173.0 - 2500.0,
            );
            let here = density(at(a, b, height));
            along += (density(at(a + 60.0, b, height)) - here).abs();
            across += (density(at(a, b + 60.0, height)) - here).abs();
        }
    }
    assert!(across > 0.0, "the streaks are met");
    assert!(along < 0.5 * across, "{along} along, {across} across");
}

#[test]
fn cirrus_above_dims_the_sunlight_lighting_the_bank_beneath() {
    let sun = Vec3::new(0.2, 0.9, 0.3).normalized();
    let cirrus = bank_of(
        Deck {
            cover: 1.0,
            ..CIRRUS
        },
        sun,
        None,
    );
    let (open, shaded) = (
        bank_of(CUMULUS, sun, None),
        bank_of(CUMULUS, sun, Some(&cirrus)),
    );
    for (bare, under) in open.levels.iter().zip(&shaded.levels) {
        assert_eq!(bare.light, under.light, "its own cloud is as deep");
        assert!(bare.overhead.is_empty());
        let dimmed = under
            .overhead
            .iter()
            .filter(|&&kept| {
                assert!((0.0..=1.0).contains(&kept), "{kept}");
                kept < 0.99
            })
            .count();
        assert!(dimmed > under.overhead.len() / 2, "{dimmed}");
    }
    // Cloud above dims the light a cloud's sunlit edge sends: it neither
    // powders that edge away nor spreads it into a brighter lobe.
    let (mut met, mut darker) = (0, 0);
    for step in 0..200u32 {
        let dir = heading(
            f64::from(step) * 2.399_963,
            mathf::atan(0.05 + 0.4 * f64::from(step % 10) / 10.0),
        );
        let origin = Vec3::new(f64::from(step % 13) * 300.0 - 1800.0, 2.0, -1000.0);
        let (Some(bare), Some(under)) = (
            cloud(&open, origin, dir, true),
            cloud(&shaded, origin, dir, true),
        ) else {
            continue;
        };
        met += 1;
        assert!(
            (under.0 - bare.0 * (1.0 + 1e-9)).max_element() <= 1e-12,
            "{:?} under cirrus against {:?}",
            under.0,
            bare.0
        );
        darker += u32::from(under.0.y < 0.99 * bare.0.y);
    }
    assert!(met > 40 && darker > met / 2, "{darker} of {met}");
}

#[test]
fn a_bank_lit_from_beneath_its_level_keeps_a_finite_light() {
    let below = Vec3::new(0.99, -0.03, 0.1).normalized();
    let bank = bank_of(CIRRUS, below, None);
    assert!(bank
        .levels
        .iter()
        .flat_map(|level| &level.light)
        .all(|depth| depth.is_finite() && *depth >= 0.0));
    assert!(bank
        .scatter
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0));
}

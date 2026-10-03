//! Host tests of volumetric cloud: its noises tile, a ray meets the bank
//! where it lies and not past it, and its shadow is a transmittance.

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

fn lit(mut bank: Cloudbank) -> Cloudbank {
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(20.0); SUNLIGHT_LEVELS],
        above: Vec3::new(0.6, 0.8, 1.2),
        below: Vec3::splat(0.3),
    });
    bank
}

/// A bank of `deck` lit from `sun`, beneath `above` if it lies beneath one.
fn bank_of(deck: Deck, sun: Vec3, above: Option<&Cloudbank>) -> Cloudbank {
    let mut bank = Cloudbank::new([Some(deck), None], (0.0, 0.0), 12_000.0, sun).expect("fits");
    while !bank.step(&tairix_parallel::SERIAL, above).expect("held") {}
    lit(bank)
}

fn built(runner: &dyn JobRunner) -> Cloudbank {
    let sun = Vec3::new(0.3, 0.8, 0.2).normalized();
    let mut bank = Cloudbank::new([Some(CUMULUS), None], (0.0, 0.0), 12_000.0, sun).expect("fits");
    while !bank.step(runner, None).expect("held") {}
    lit(bank)
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
fn rays_up_through_the_bank_meet_cloud_and_rays_beside_it_do_not() {
    let bank = built(&tairix_parallel::SERIAL);
    let mut met = 0;
    let mut hidden = 0;
    for step in 0..400u32 {
        let x = f64::from(step % 20) * 400.0 - 4000.0;
        let z = f64::from(step / 20) * 400.0 - 4000.0;
        if let Some((light, kept, depth)) = bank.seen(Vec3::new(x, 0.0, z), Vec3::UP, true, 0.5) {
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
    let far = Vec3::new(20_000.0, 0.0, 0.0);
    assert!(
        bank.seen(far, Vec3::UP, true, 0.5).is_none(),
        "past the bank's edge"
    );
}

#[test]
fn the_bank_shades_some_of_the_ground_and_never_brightens_it() {
    let bank = built(&tairix_parallel::SERIAL);
    let sun = Vec3::new(0.3, 0.8, 0.2).normalized();
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
    let alone = built(&tairix_parallel::SERIAL);
    let spread = built(&tairix_parallel::Threaded::new(3));
    for step in 0..50u32 {
        let origin = Vec3::new(
            f64::from(step) * 97.0 - 2400.0,
            5.0,
            f64::from(step) * -61.0,
        );
        let dir = Vec3::new(0.3, 0.7, f64::from(step % 7) * 0.1).normalized();
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
/// comes through it, integrated in two-metre steps: the reference a march
/// must agree with.
fn integrated(bank: &Cloudbank, origin: Vec3, dir: Vec3) -> f64 {
    let Some((enter, leave)) = bank.slab(origin, dir) else {
        return 1.0;
    };
    let reader = Reader { bank };
    let step = 2.0;
    let mut optical = 0.0;
    let mut t = enter + 0.5 * step;
    while t < leave {
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
        let heading = f64::from(step) * 2.399_963;
        let rise = 0.03 + 0.9 * f64::from(step % 40) / 40.0;
        let dir = Vec3::new(mathf::cos(heading), rise, mathf::sin(heading)).normalized();
        let origin = Vec3::new(
            f64::from(step % 13) * 300.0 - 1800.0,
            2.0,
            f64::from(step % 11) * -250.0,
        );
        let reference = integrated(&bank, origin, dir);
        let kept = bank
            .seen(origin, dir, true, 0.5)
            .map_or(1.0, |(_, kept, _)| kept);
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

/// A bank thins out toward its rim, so a ray leaving through its side leaves
/// no cloud behind a wall of it.
#[test]
fn a_bank_thins_out_to_nothing_at_its_rim() {
    let bank = built(&tairix_parallel::SERIAL);
    let reader = Reader { bank: &bank };
    let (floor, ceiling) = bank.span();
    let rim = bank.half * (1.0 - 1e-6);
    let mut inside = 0;
    for step in 0..2000u32 {
        let along = (f64::from(step) / 1000.0 - 1.0) * rim;
        let y = floor + (ceiling - floor) * f64::from(step % 40) / 40.0;
        for point in [
            Vec3::new(rim, y, along),
            Vec3::new(-rim, y, along),
            Vec3::new(along, y, rim),
            Vec3::new(along, y, -rim),
        ] {
            assert!(reader.density(point, true).is_none(), "{point:?}");
        }
        inside += u32::from(
            reader
                .density(Vec3::new(0.85 * rim, y, along), true)
                .is_some(),
        );
    }
    assert!(inside > 20, "{inside} points inside the rim hold cloud");
}

/// However the samples of a pixel stagger a march, it finds the same cloud.
#[test]
fn a_march_sees_the_same_cloud_however_its_steps_are_staggered() {
    let bank = built(&tairix_parallel::SERIAL);
    for step in 0..120u32 {
        let heading = f64::from(step) * 1.1;
        let dir = Vec3::new(
            mathf::cos(heading),
            0.05 + 0.01 * f64::from(step % 9),
            mathf::sin(heading),
        )
        .normalized();
        let origin = Vec3::new(f64::from(step % 7) * 400.0, 2.0, -1500.0);
        let kept = |jitter: f64| {
            bank.seen(origin, dir, true, jitter)
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
    let mut bank = built(&tairix_parallel::SERIAL);
    // Clear the whole map but a stripe of columns at its far side, past
    // hundreds of clear ones a low ray from the near side must cross.
    let size = bank.spacing.0;
    for (index, column) in bank.weather.iter_mut().enumerate() {
        let x = real(index % WEATHER_SIDE) * size - bank.half;
        if x < 8_000.0 {
            column.cover = [0.0; 2];
        }
    }
    bank.fill_bands(&tairix_parallel::SERIAL);
    let mut met = [0; 2];
    for step in 0..24u32 {
        // Rays level with the deck's middle, fanned across the stripe.
        let origin = Vec3::new(-11_500.0, 1_700.0, f64::from(step) * 300.0 - 3600.0);
        let dir = Vec3::new(1.0, 0.002, 0.0).normalized();
        for (fine, met) in [true, false].into_iter().zip(&mut met) {
            if let Some((_, _, depth)) = bank.seen(origin, dir, fine, 0.5) {
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

#[test]
fn no_cloud_stands_outside_its_weather_cells_band() {
    for bank in [
        built(&tairix_parallel::SERIAL),
        bank_of(CIRRUS, Vec3::UP, None),
    ] {
        let reader = Reader { bank: &bank };
        let (mut cloudy, mut clear) = (0u32, 0u32);
        for index in 0..200_000u32 {
            let (u, v, w) = (
                unit(mix32(index)),
                unit(mix32(index ^ 0x51ed_270b)),
                unit(mix32(index ^ 0x9e37_79b9)),
            );
            let point = Vec3::new(
                bank.centre.0 + bank.half * (2.0 * u - 1.0),
                bank.floor + (bank.ceiling - bank.floor) * w,
                bank.centre.1 + bank.half * (2.0 * v - 1.0),
            );
            let jumped = bank
                .over(&bank.walk(point, Vec3::UP, 0.0))
                .clear(point.y, 1.0, 0.0)
                .is_some();
            clear += u32::from(jumped);
            if reader.density(point, false).is_some() {
                cloudy += 1;
                assert!(!jumped, "cloud at {point:?} lies outside its band");
            }
        }
        assert!(cloudy > 1000, "{cloudy} points hold cloud");
        assert!(clear > 20_000, "and a march may jump {clear}");
    }
}

/// All along a ray, whichever way it heads, its walk across the weather map
/// stands over the cell each point of it lies over, however far apart the
/// points it is advanced to lie.
#[test]
fn a_rays_walk_stands_over_the_cell_beneath_each_point_of_it() {
    let bank = built(&tairix_parallel::SERIAL);
    let mut checked = 0u32;
    for index in 0..240u32 {
        let heading = f64::from(index) * 2.399_963;
        // Now and then straight along a line of cells, east, west, north or
        // south.
        let (x, z) = match index % 40 {
            0 => (1.0, 0.0),
            1 => (-1.0, 0.0),
            2 => (0.0, 1.0),
            3 => (0.0, -1.0),
            _ => (mathf::cos(heading), mathf::sin(heading)),
        };
        let dir = Vec3::new(x, 0.02 + 0.1 * f64::from(index % 7), z).normalized();
        let origin = Vec3::new(
            f64::from(index % 13) * 900.0 - 6000.0,
            1500.0,
            4000.0 - f64::from(index % 11) * 800.0,
        );
        let (enter, leave) = bank.slab(origin, dir).expect("starts within the bank");
        let mut walk = bank.walk(origin, dir, enter);
        let mut t = enter;
        while t < leave {
            walk.advance(t);
            assert!(walk.until() > t, "ray {index} at {t}");
            let point = origin + dir * t;
            if let Some(((column, east), (row, north))) = bank.weather_cell(point.x, point.z) {
                // On a line of cells, rounding may put a point either side.
                let between = |fraction: f64| (1e-9..1.0 - 1e-9).contains(&fraction);
                if between(east) && between(north) {
                    assert_eq!(walk.cell(), Some((column, row)), "ray {index} at {t}");
                    checked += 1;
                }
            }
            // Strides short and long, some across several cells at once.
            t += 23.0 + f64::from(index % 5) * 211.0;
        }
    }
    assert!(checked > 5000, "{checked} points checked");
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
        Vec3::new(
            along * cos - across * sin,
            height,
            along * sin + across * cos,
        )
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
    assert_eq!(open.light, shaded.light, "its own cloud is as deep");
    assert!(open.overhead.is_empty());
    let dimmed = shaded
        .overhead
        .iter()
        .filter(|&&kept| {
            assert!((0.0..=1.0).contains(&kept), "{kept}");
            kept < 0.99
        })
        .count();
    assert!(dimmed > shaded.overhead.len() / 2, "{dimmed}");
    // Cloud above dims the light a cloud's sunlit edge sends: it neither
    // powders that edge away nor spreads it into a brighter lobe.
    let (mut met, mut darker) = (0, 0);
    for step in 0..200u32 {
        let heading = f64::from(step) * 2.399_963;
        let dir = Vec3::new(
            mathf::cos(heading),
            0.05 + 0.4 * f64::from(step % 10) / 10.0,
            mathf::sin(heading),
        )
        .normalized();
        let origin = Vec3::new(f64::from(step % 13) * 300.0 - 1800.0, 2.0, -1000.0);
        let (Some(bare), Some(under)) = (
            open.seen(origin, dir, true, 0.5),
            shaded.seen(origin, dir, true, 0.5),
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
        .light
        .iter()
        .all(|depth| depth.is_finite() && *depth >= 0.0));
    assert!(bank
        .scatter
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0));
}

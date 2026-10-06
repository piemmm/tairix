use alloc::vec::Vec;

use super::*;

/// Places over a few kilometres about the origin.
fn places(count: u32, reach: f64) -> impl Iterator<Item = (f64, f64)> {
    (0..count).map(move |index| {
        let key = mix32(index ^ 0x5151);
        (
            reach * (2.0 * unit(mix32(key ^ 1)) - 1.0),
            reach * (2.0 * unit(mix32(key ^ 2)) - 1.0),
        )
    })
}

fn landforms() -> [Landform; 6] {
    [
        Landform::Hills {
            scale: 300.0,
            height: 60.0,
            seed: 1,
        },
        Landform::Mountains {
            scale: 1200.0,
            height: 1400.0,
            floor: 900.0,
            seed: 2,
        },
        Landform::Dunes {
            scale: 120.0,
            height: 25.0,
            heading: 0.7,
            seed: 3,
        },
        Landform::Island {
            centre: (200.0, -100.0),
            radius: 900.0,
            height: 120.0,
            seed: 4,
        },
        Landform::Mesas {
            scale: 400.0,
            height: 200.0,
            steps: 4.0,
            seed: 5,
        },
        Landform::Valley {
            heading: 1.2,
            floor: 60.0,
            height: 70.0,
            seed: 6,
        },
    ]
}

#[test]
fn no_landform_falls_below_its_lowest() {
    for form in landforms() {
        let lowest = form.lowest();
        let mut highest = f64::NEG_INFINITY;
        for (x, z) in places(5000, 3000.0) {
            let height = form.height(x, z);
            assert!(height.is_finite());
            assert!(
                height >= lowest - 1e-9,
                "{form:?} at ({x}, {z}): {height} below {lowest}"
            );
            highest = highest.max(height);
        }
        assert!(highest > lowest + 1.0, "{form:?} has some relief");
    }
}

#[test]
fn an_island_rises_from_the_sea_and_falls_away_beneath_it() {
    let island = Landform::Island {
        centre: (0.0, 0.0),
        radius: 800.0,
        height: 100.0,
        seed: 9,
    };
    assert!(island.height(0.0, 0.0) > 20.0, "land at its heart");
    for (x, z) in places(200, 400.0) {
        let far = (x + 3000.0, z - 3000.0);
        assert!(island.height(far.0, far.1) < 0.0, "under the sea far out");
    }
}

#[test]
fn terrain_is_level_in_its_clearing_and_settles_to_its_rim() {
    let form = Landform::Hills {
        scale: 300.0,
        height: 60.0,
        seed: 1,
    };
    let datum = form.height(0.0, 0.0);
    let terrain = Terrain {
        rim: Some(-5.0),
        form: form.clone(),
        datum,
        centre: (0.0, 0.0),
        radius: 2000.0,
        tilt: (0.0, 0.0),
        clearing: Some((-0.5, 40.0)),
    };
    // The clearing's edge wanders up to 0.22 of its radius in and out, so the
    // land lies level everywhere within three quarters of it, whatever the
    // seed.
    for (x, z) in places(300, 40.0) {
        if mathf::hypot(x, z) < 0.75 * 40.0 {
            assert!(
                (terrain.height(x, z) + 0.5).abs() < 1e-9,
                "level in the clearing at ({x}, {z})"
            );
        }
    }
    for angle in 0..16 {
        let (sin, cos) = (mathf::sin(f64::from(angle)), mathf::cos(f64::from(angle)));
        let (x, z) = (1990.0 * sin, 1990.0 * cos);
        assert!((terrain.height(x, z) + 5.0).abs() < 1e-9, "at the rim");
        let (x, z) = (400.0 * sin, 400.0 * cos);
        assert!(
            (terrain.height(x, z) - (form.height(x, z) - datum)).abs() < 1e-9,
            "its own shape between"
        );
    }
    let lowest = terrain.lowest();
    for (x, z) in places(3000, 2500.0) {
        assert!(terrain.height(x, z) >= lowest - 1e-9);
    }
}

/// A tilted land falls as its tilt has it, straight from its middle out to
/// where the fall begins to ease, then levelling off by its edge: never
/// climbing again however far out, and falling one way as it rises the
/// other.
#[test]
fn a_tilted_land_falls_evenly_then_levels_off() {
    let flat = Landform::Hills {
        scale: 300.0,
        height: 0.0,
        seed: 1,
    };
    let tilt = (-0.006, 0.008);
    let radius = 2000.0;
    let tilted = Terrain {
        form: flat,
        datum: 0.0,
        centre: (100.0, -50.0),
        radius,
        rim: None,
        tilt,
        clearing: None,
    };
    let slope = mathf::hypot(tilt.0, tilt.1);
    let (dx, dz) = (tilt.0 / slope, tilt.1 / slope);
    let at = |along: f64| tilted.height(100.0 + dx * along, -50.0 + dz * along);
    for step in 0..=20 {
        let along = EASE * radius * f64::from(step) / 20.0;
        assert!(
            (at(along) - slope * along).abs() < 1e-9,
            "{along}: {} not {}",
            at(along),
            slope * along
        );
        assert!(
            (at(-along) + at(along)).abs() < 1e-9,
            "{along}: falling as it rises"
        );
    }
    let mut was = at(EASE * radius);
    for step in 1..=60 {
        let along = EASE * radius + 100.0 * f64::from(step);
        let here = at(along);
        assert!(here >= was - 1e-9, "{along}: climbs back");
        assert!(here <= slope * radius + 1e-9, "{along}: past its level");
        was = here;
    }
    // Across the tilt, it does not fall at all.
    assert!(tilted.height(100.0 - dz * 900.0, -50.0 + dx * 900.0).abs() < 1e-9);
}

/// Without a rim, land past its disc runs on as its own shape, tilted.
#[test]
fn a_land_without_a_rim_runs_on_as_its_own_shape() {
    let form = Landform::Hills {
        scale: 300.0,
        height: 60.0,
        seed: 4,
    };
    let terrain = Terrain {
        form: form.clone(),
        datum: 7.0,
        centre: (0.0, 0.0),
        radius: 1000.0,
        rim: None,
        tilt: (0.0, 0.0),
        clearing: None,
    };
    for (x, z) in places(400, 3000.0) {
        assert!(
            (terrain.height(x, z) - (form.height(x, z) - 7.0)).abs() < 1e-9,
            "({x}, {z})"
        );
    }
}

/// A clearing keeps none of the land's relief well within it and all of it
/// well beyond; between, its edge wanders in and out round it, as ground
/// levelled by hand does, and the land pinned to it lies at the clearing's
/// level exactly where no relief is kept.
#[test]
fn a_clearings_edge_wanders_and_pins_its_level_within() {
    let form = Landform::Hills {
        scale: 300.0,
        height: 60.0,
        seed: 6,
    };
    let (level, radius) = (3.0, 100.0);
    let terrain = Terrain {
        form,
        datum: 0.0,
        centre: (20.0, 30.0),
        radius: 3000.0,
        rim: None,
        tilt: (0.0, 0.0),
        clearing: Some((level, radius)),
    };
    let round = |distance: f64, angle: f64| {
        (
            20.0 + distance * mathf::sin(angle),
            30.0 + distance * mathf::cos(angle),
        )
    };
    let (mut kept_at_radius, mut bare_at_radius) = (0, 0);
    for step in 0..96u32 {
        let angle = core::f64::consts::TAU * f64::from(step) / 96.0;
        let (x, z) = round(0.75 * radius, angle);
        assert!(terrain.keep(x, z) <= 0.0, "{angle}: relief kept within");
        assert!((terrain.pin(x, z, 99.0) - level).abs() < 1e-12);
        assert!((terrain.height(x, z) - level).abs() < 1e-9);
        let (x, z) = round(3.1 * radius, angle);
        assert!(
            (terrain.keep(x, z) - 1.0).abs() < 1e-12,
            "{angle}: all kept beyond"
        );
        assert!((terrain.pin(x, z, 99.0) - 99.0).abs() < 1e-12);
        let (x, z) = round(radius, angle);
        if terrain.keep(x, z) > 0.0 {
            kept_at_radius += 1;
        } else {
            bare_at_radius += 1;
        }
    }
    assert!(
        kept_at_radius > 0 && bare_at_radius > 0,
        "its edge wanders: {kept_at_radius} kept and {bare_at_radius} bare at its radius"
    );
}

#[test]
fn a_sea_repeats_every_period_and_keeps_to_its_height() {
    let period = 1024.0;
    let sea = Sea::new(period, (50.0, 4.0, 2.0), (0.4, 0.6), 17);
    let (mut highest, mut sum, mut squares): (f64, f64, f64) = (0.0, 0.0, 0.0);
    let samples = 20_000u32;
    for (x, z) in places(samples, 600.0) {
        let here = sea.height(x, z);
        assert!((sea.height(x + period, z) - here).abs() < 1e-9);
        assert!((sea.height(x, z - 2.0 * period) - here).abs() < 1e-9);
        highest = highest.max(here.abs());
        sum += here;
        squares += here * here;
    }
    let mean = sum / f64::from(samples);
    let deviation = mathf::sqrt(squares / f64::from(samples) - mean * mean);
    // Four standard deviations are the significant height the sea was
    // asked for, and its highest crest no more than about that.
    assert!(
        (4.0 * deviation - 2.0).abs() < 0.25,
        "significant height {}",
        4.0 * deviation
    );
    assert!(highest < 2.0, "the highest crest {highest}");
}

#[test]
fn a_sea_runs_before_its_wind() {
    // Along the wind the swell rises and falls; across it, much less.
    let sea = Sea::new(1024.0, (60.0, 4.0, 2.0), (0.0, 0.1), 3);
    let range = |step: (f64, f64)| {
        let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..400 {
            let at = f64::from(i) * 0.5;
            let height = sea.height(step.0 * at, step.1 * at);
            low = low.min(height);
            high = high.max(height);
        }
        high - low
    };
    assert!(range((1.0, 0.0)) > 2.0 * range((0.0, 1.0)));
}

/// No swell is shorter than the shortest the sea was given, so a grid of a
/// quarter of that holds every one without aliasing; and the longest is the
/// swell asked for.
#[test]
fn a_seas_swells_keep_within_their_lengths() {
    let sea = Sea::new(1024.0, (60.0, 4.0, 2.0), (0.7, 0.5), 9);
    let lengths: Vec<f64> = sea
        .waves
        .iter()
        .map(|wave| core::f64::consts::TAU / mathf::hypot(wave.kx, wave.kz))
        .collect();
    // Snapped to the period, a length moves by less than a part in ten.
    assert!(
        lengths.iter().all(|length| *length > 4.0 * 0.9),
        "{lengths:?}"
    );
    assert!(
        lengths.iter().any(|length| *length > 60.0 * 0.9),
        "{lengths:?}"
    );
}

/// A valley runs along its heading as a compass reads it, the way the scenes
/// that tilt it, route a road across it and stand an aqueduct over it read
/// that heading: its floor lies along the heading's line, and a line square
/// to it climbs out onto the hills.
#[test]
fn a_valley_runs_along_its_compass_heading() {
    for heading in [0.0, 0.5, 1.2, 2.9, 4.4] {
        let floor = 80.0;
        let form = Landform::Valley {
            heading,
            floor,
            height: 120.0,
            seed: 11,
        };
        let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
        let at = |along: f64, across: f64| {
            form.height(along * sin + across * cos, along * cos - across * sin)
        };
        for step in -10..=10 {
            let along = 150.0 * f64::from(step);
            let on_floor = at(along, 0.0);
            for side in [-1.0, 1.0] {
                let on_hills = at(along, side * 4.0 * floor);
                assert!(
                    on_floor < on_hills,
                    "heading {heading} along {along}: floor {on_floor} against hills {on_hills}"
                );
            }
        }
    }
}

/// A valley is a valley, not a plain: wherever its course wanders and its
/// spurs stand out, its sides climb more than a third of its height out of
/// its floor within the run soil keeps to, and never climb as a cliff does.
#[test]
fn a_valleys_sides_climb_out_of_its_floor() {
    let (heading, floor, height) = (0.7, 70.0, 260.0);
    let run = (height / SIDE_SLOPE).max(1.5 * floor);
    // Past the farthest the course wanders and a spur stands out.
    let beyond = 0.5 * floor + run + floor;
    for seed in [3, 11, 29] {
        let form = Landform::Valley {
            heading,
            floor,
            height,
            seed,
        };
        let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
        let at = |along: f64, across: f64| {
            form.height(along * sin + across * cos, along * cos - across * sin)
        };
        for step in -6..=6 {
            let along = 200.0 * f64::from(step);
            let lowest = (-40..=40)
                .map(|k| at(along, 0.03 * floor * f64::from(k)))
                .fold(f64::INFINITY, f64::min);
            for side in [-1.0, 1.0] {
                let upland = at(along, side * beyond);
                assert!(
                    upland - lowest > 0.35 * height,
                    "seed {seed} along {along}: a side climbs {} of {height}",
                    upland - lowest
                );
                let climbs = (0..400).map(|k| {
                    let across = side * beyond * f64::from(k) / 400.0;
                    (at(along, across + side * 2.0) - at(along, across)).abs() / 2.0
                });
                let steepest = climbs.fold(0.0, f64::max);
                assert!(
                    steepest < 1.5,
                    "seed {seed} along {along}: a side as steep as {steepest}"
                );
            }
        }
    }
}

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

#[test]
fn a_cloudscape_stays_about_its_range_and_stretches_along_its_heading() {
    let heaps = Cloudscape {
        scale: 900.0,
        stretch: 1.0,
        heading: 0.0,
        seed: 5,
    };
    let streaks = Cloudscape {
        scale: 900.0,
        stretch: 6.0,
        heading: 0.0,
        seed: 5,
    };
    for (x, z) in places(2000, 20_000.0) {
        assert!(heaps.density(x, z).abs() <= 1.2);
        assert!(streaks.density(x, z).abs() <= 1.2);
    }
    // Streaks change far more slowly along their heading than across it.
    let change = |form: &Cloudscape, (dx, dz): (f64, f64)| {
        places(400, 10_000.0)
            .map(|(x, z)| (form.density(x + dx, z + dz) - form.density(x, z)).abs())
            .sum::<f64>()
    };
    assert!(change(&streaks, (300.0, 0.0)) < 0.5 * change(&streaks, (0.0, 300.0)));
}

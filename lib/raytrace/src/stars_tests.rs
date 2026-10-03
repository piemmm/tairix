//! Host tests of the stars: as many of each brightness as the census counts,
//! each its own colour, each one's light held whole by the footprint that
//! spreads it, none looked for that could not show, and the whole sky's
//! starlight what is measured.

use alloc::vec::Vec;

use super::*;

/// Where `dir` meets the plane of `face`; `None` facing away from it.
fn on_face(face: u32, dir: Vec3) -> Option<(f64, f64)> {
    let (axis, sign, u, v) = axes(face);
    let major = sign * dir.along(axis);
    (major > 1e-12).then(|| (dir.along(u) / major, dir.along(v) / major))
}

/// What a ray seeing `spread` radians across sees of every tier, against
/// nothing.
fn sharp(field: &Starfield, spread: f64) -> Glimpse {
    field
        .glimpse(Some(spread), Vec3::ZERO)
        .expect("against nothing, every star shows")
}

#[test]
fn a_faces_cells_hold_as_many_stars_of_each_brightness_as_the_census_counts() {
    let field = Starfield::new();
    let (mut counted, mut seen, mut bright) = (0u32, 0u32, 0u32);
    for tier in &field.tiers {
        for column in 0..tier.cells {
            for row in 0..tier.cells {
                for star in stars(tier, 2, (column, row)) {
                    let (magnitude, _) = field.shine(tier, star.key);
                    counted += 1;
                    seen += u32::from(magnitude < 6.0);
                    bright += u32::from(magnitude < 3.0);
                }
            }
        }
    }
    // A face is a sixth of the sky; each count is within four of its
    // Poisson deviations of a sixth of the census's.
    let near = |count: u32, census: f64| {
        let expected = census / 6.0;
        (f64::from(count) - expected).abs() < 4.0 * mathf::sqrt(expected)
    };
    assert!(near(counted, 2.46e6), "{counted} to magnitude 12");
    assert!(near(seen, 4800.0), "{seen} to magnitude 6");
    assert!(near(bright, 171.0), "{bright} to magnitude 3");
}

#[test]
fn a_footprint_holds_all_the_light_of_its_star() {
    let field = Starfield::new();
    for deviation in [1e-4, 4e-4] {
        let (steps, reach) = (20_000u32, REACH * deviation);
        let ring = reach / f64::from(steps);
        let held: f64 = (0..steps)
            .map(|step| {
                let radius = ring * (f64::from(step) + 0.5);
                field.rise(radius * radius, deviation) * 2.0 * PI * radius * ring
            })
            .sum::<f64>()
            / (field.volume * deviation * deviation);
        assert!((held - 1.0).abs() < 1e-6, "{deviation}: {held}");
        assert!(field.rise(reach * reach * 1.0001, deviation) <= 0.0);
    }
}

/// The light of every star of every tier in the cells of `faces` within two
/// of where `dir` meets each, through a footprint of `deviation`: found by
/// brute force, the reference a ray's own search must match.
fn every_star_near(field: &Starfield, faces: &[u32], dir: Vec3, deviation: f64) -> Vec3 {
    let mut light = Vec3::ZERO;
    for tier in &field.tiers {
        for &face in faces {
            let Some((x, y)) = on_face(face, dir) else {
                continue;
            };
            let cell = |at: f64| span((at, at), tier.cells).0;
            let (column, row) = (cell(x.clamp(-1.0, 1.0)), cell(y.clamp(-1.0, 1.0)));
            for column in column.saturating_sub(2)..=(column + 2).min(tier.cells - 1) {
                for row in row.saturating_sub(2)..=(row + 2).min(tier.cells - 1) {
                    for star in stars(tier, face, (column, row)) {
                        let gap = dir - star.at;
                        light +=
                            field.shine(tier, star.key).1 * field.rise(gap.dot(gap), deviation);
                    }
                }
            }
        }
    }
    light * (1.0 / (field.volume * deviation * deviation))
}

#[test]
fn a_star_by_a_faces_edge_lights_rays_from_the_next_face_alike() {
    let field = Starfield::new();
    let deviation = 4e-4;
    // A faint star on the +x face within half a deviation of its edge toward
    // +y.
    let tier = &field.tiers[TIERS.len() - 1];
    let star = (0..tier.cells)
        .flat_map(|row| stars(tier, 0, (tier.cells - 1, row)))
        .find(|star| on_face(0, star.at).is_some_and(|(x, _)| x > 1.0 - 0.5 * deviation))
        .expect("the edge's cells hold such a star");
    let across = Vec3::UP.cross(star.at).cross(star.at).normalized() * -1.0;
    let (beyond, within) = (
        (star.at + across * deviation).normalized(),
        (star.at - across * deviation).normalized(),
    );
    assert!(on_face(2, beyond).is_some_and(|(x, z)| x.abs() <= 1.0 && z.abs() <= 1.0));
    for dir in [beyond, within] {
        let seen = field.radiance(dir, sharp(&field, 2.0 * deviation)) - field.glow;
        let expected = every_star_near(&field, &[0, 2], dir, deviation);
        assert!(expected.y > 0.0, "the star is within reach");
        assert!(
            (seen - expected).max_element().abs() < 1e-9 * expected.max_element(),
            "{seen:?} against {expected:?}"
        );
    }
}

#[test]
fn a_footprint_searches_every_cell_it_reaches_and_little_more() {
    let (reach, cells) = (6e-3, 512);
    let sine = mathf::sin(reach);
    let searched = |face: u32, dir: Vec3| {
        reached(face, dir, sine).map(|(across, up)| (span(across, cells), span(up, cells)))
    };
    let corner = Vec3::new(1.0, 1.0, 1.0).normalized();
    let edge = Vec3::new(1.0, 0.999, 0.3).normalized();
    // Barely above the +y face's plane, where that plane's own coordinates
    // for the ray run to hundreds.
    let level = Vec3::new(0.6, 1e-3, 0.8).normalized();
    for dir in [corner, edge, level] {
        let aside = dir.cross(Vec3::UP).normalized();
        let up = aside.cross(dir);
        for ring in 0..=4u32 {
            let radius = reach * f64::from(ring) / 4.0 * (1.0 - 1e-9);
            for step in 0..720u32 {
                let turn = f64::from(step) * PI / 360.0;
                let within = dir * mathf::cos(radius)
                    + (aside * mathf::cos(turn) + up * mathf::sin(turn)) * mathf::sin(radius);
                for face in 0..6 {
                    let Some((x, y)) =
                        on_face(face, within).filter(|&(x, y)| x.abs() <= 1.0 && y.abs() <= 1.0)
                    else {
                        continue;
                    };
                    let (columns, rows) = searched(face, dir).expect("the face is reached");
                    let (column, row) = (span((x, x), cells).0, span((y, y), cells).0);
                    assert!(
                        (columns.0..=columns.1).contains(&column)
                            && (rows.0..=rows.1).contains(&row),
                        "{dir:?} misses ({column}, {row}) of face {face}"
                    );
                }
            }
        }
    }
    let faces: Vec<_> = (0..6).filter_map(|face| searched(face, level)).collect();
    assert_eq!(faces.len(), 1, "{faces:?}");
    let ((c0, c1), (r0, r1)) = faces[0];
    assert!(c1 - c0 < 7 && r1 - r0 < 7, "{:?}", faces[0]);
    let lost = Vec3::new(f64::NAN, 0.8, 0.6);
    assert!(
        (0..6).all(|face| reached(face, lost, sine).is_none()),
        "nothing for a lost ray"
    );
}

/// A broad footprint, as a rough reflection's, blurs a bright star by the
/// cube's corner as it blurs one by a face's middle, though the corner's
/// cells are a third as broad.
#[test]
fn a_bright_star_blurs_alike_wherever_it_stands_on_the_cube() {
    let field = Starfield::new();
    let tier = &field.tiers[0];
    let star = (tier.cells - 8..tier.cells)
        .flat_map(|column| (tier.cells - 8..tier.cells).map(move |row| (column, row)))
        .flat_map(|cell| stars(tier, 0, cell))
        .max_by(|a, b| {
            let light = |star: &Star| field.shine(tier, star.key).1.y;
            light(a).total_cmp(&light(b))
        })
        .expect("the corner's cells hold a star");
    // Wider than the corner's own cells of the brightest tier, narrower
    // than its cells at a face's middle; past the finer tiers' cells.
    let spread = 0.05;
    let deviation = 0.5 * spread;
    let peak = (1.0 - field.floor) / (field.volume * deviation * deviation);
    let own = field.shine(tier, star.key).1 * peak;
    assert!(own.y > 2.0 * tier.mean.y, "a star that stands out: {own:?}");
    let finer = field.tiers[1].mean + field.tiers[2].mean;
    let bright = field.radiance(star.at, sharp(&field, spread)) - field.glow - finer;
    assert!(
        bright.y > own.y.max(1.5 * tier.mean.y),
        "{bright:?} against its own {own:?}"
    );
}

#[test]
fn a_star_is_coloured_by_its_index_and_one_like_the_sun_is_white() {
    let reference = temperature(SUN_INDEX);
    let sunlike = tint(SUN_INDEX, reference);
    assert!(
        (sunlike - Vec3::ONE).max_element().abs() < 1e-12,
        "{sunlike:?}"
    );
    let blue = tint(-0.3, reference);
    assert!(blue.z > 1.0 && blue.x < 1.0, "{blue:?}");
    let red = tint(2.0, reference);
    assert!(red.x > 1.0 && red.z < 1.0, "{red:?}");
    assert!((temperature(SUN_INDEX) - 5772.0).abs() < 20.0);
}

#[test]
fn a_scattered_ray_or_a_broad_footprint_sees_the_stars_mean() {
    let field = Starfield::new();
    let dir = Vec3::new(0.3, 0.8, -0.5).normalized();
    let mean = field.counted + field.glow;
    let scattered = field.glimpse(None, Vec3::ZERO).expect("shows");
    assert_eq!(field.radiance(dir, scattered), mean);
    // Far wider than even the broadest tier's cells.
    let broad = field.radiance(dir, sharp(&field, 0.5));
    assert!((broad - mean).max_element().abs() < 1e-12 * mean.max_element());
    let sharpest = field.radiance(dir, sharp(&field, 1e-3));
    assert_eq!(
        sharpest,
        field.radiance(dir, sharp(&field, 1e-3)),
        "the same every time"
    );
}

#[test]
fn a_tier_left_unsearched_holds_no_star_that_could_show() {
    let field = Starfield::new();
    let (day, night) = (Vec3::splat(0.5), Vec3::splat(1e-6));
    // A pixel's footprint on a picture a thousand rows tall.
    let spread = 7e-4;
    let glimpse = field
        .glimpse(Some(spread), day)
        .expect("the brightest show");
    assert_eq!(glimpse.shown, [true, false, false]);
    let deviation = 0.5 * spread;
    let peak = (1.0 - field.floor) / (field.volume * deviation * deviation);
    let least = day * VISIBLE;
    for (tier, _) in field
        .tiers
        .iter()
        .zip(glimpse.shown)
        .filter(|&(_, shown)| !shown)
    {
        assert!((tier.mean - least).max_element() < 0.0);
        for column in 0..tier.cells {
            for row in 0..tier.cells {
                for star in stars(tier, 4, (column, row)) {
                    let sharpest = field.shine(tier, star.key).1 * peak;
                    assert!((sharpest - least).max_element() < 0.0, "{sharpest:?}");
                }
            }
        }
    }
    assert!(
        field.glimpse(Some(0.01), day).is_none(),
        "too broad to show by day"
    );
    assert!(field.glimpse(None, day).is_none());
    let dark = field
        .glimpse(Some(spread), night)
        .expect("by night they show");
    assert_eq!(dark.shown, [true; TIERS.len()]);
    assert!(field.glimpse(None, night).is_some());
}

#[test]
fn the_whole_skys_starlight_is_its_measured_magnitude() {
    let field = Starfield::new();
    let share = 4.0 * PI * (field.counted + field.glow).y / sunlight().y;
    let magnitude = SUN_MAGNITUDE - mathf::ln(share) / FADING;
    // The integrated starlight of the whole sky, about −6.5.
    assert!((-7.3..-6.2).contains(&magnitude), "{magnitude}");
    assert!(field.glow.y < field.counted.y * 1.5, "{:?}", field.glow);
}

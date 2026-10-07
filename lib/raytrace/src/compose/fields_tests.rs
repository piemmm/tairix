use super::*;

#[test]
fn places_spaced_along_a_stretch_take_in_both_its_ends() {
    let places: Vec<f64> = spaced((2.0, 9.0), 2.5).collect();
    assert_eq!(places.first(), Some(&2.0));
    assert_eq!(places.last(), Some(&9.0));
    assert!(places
        .windows(2)
        .all(|pair| pair[1] - pair[0] <= 2.5 + 1e-9));
    assert_eq!(places.len(), 4);
}

/// A stretch with no length to measure is still taken in one step, never
/// none, so nothing divides by its count or counts back past it.
#[test]
fn a_stretch_of_no_measure_is_one_step() {
    assert_eq!(divided((0.0, f64::NAN), 2.0), 1);
    assert_eq!(divided((f64::NAN, 3.0), f64::NAN), 1);
    assert_eq!(divided((5.0, 5.0), 2.0), 1);
    assert_eq!(divided((0.0, 9.0), 2.0), 5);
}

#[test]
fn a_frame_laid_along_a_way_runs_its_x_along_it() {
    let way = Point::new(0.6, 0.8);
    let frame = along(way);
    assert!((frame.x.x - 0.6).abs() < 1e-12 && (frame.x.z - 0.8).abs() < 1e-12);
    assert!((frame.y.y - 1.0).abs() < 1e-12);
    let up = tipped(frame, 0.3);
    assert!(
        up.x.y > 0.29 && up.x.y < 0.3,
        "tipping raises x: {:?}",
        up.x
    );
}

/// A farmland's walls are built stone by stone of broken field stones where
/// they run near the eye, and none is built beyond where a wall is only
/// painted on the ground.
#[test]
fn a_farmlands_walls_are_built_of_field_stones_only_near_the_eye() {
    use crate::compose::{Composition, Setting};
    use crate::detail::Detail;
    use crate::prototype::Part;
    let mut walled = 0;
    for seed in 16..19 {
        let mut composition =
            Composition::new(Setting::Farmland, seed, (1280, 720), Detail::Simple)
                .expect("composes");
        let reach = drystone::TYPICAL
            / (Detail::Simple.densities().bounds.stones * composition.stage.pixel);
        let land = composition.run_until_seen().expect("a land");
        let layout = land.layout.as_ref().expect("farmed");
        let seen = composition.seen.as_ref().expect("seen").1.eye();
        let eye = Point::new(seen.x, seen.z);
        let near = layout.boundaries().iter().any(|boundary| {
            boundary.kind == Bound::Wall
                && plane::nearest(&boundary.line, eye)
                    .is_some_and(|near| near.distance < 0.5 * reach)
        });
        // A stile's slabs stand on end, far taller than any stone a wall is
        // laid of.
        let stones: Vec<Vec3> = composition
            .buildings()
            .flat_map(crate::prototype::Building::parts)
            .filter_map(|part| match part {
                Part::Solid(solid)
                    if matches!(solid.form(), Form::Rock { .. }) && solid.half().y < 0.35 =>
                {
                    Some(solid.centre())
                }
                _ => None,
            })
            .collect();
        if near {
            walled += 1;
            assert!(stones.len() > 500, "{seed}: {} field stones", stones.len());
        }
        for stone in &stones {
            let apart = (Point::new(stone.x, stone.z) - eye).length();
            assert!(apart < reach + 4.0, "{seed}: a stone built {apart} m off");
        }
    }
    assert!(walled > 0, "no farmland walled near the eye");
}

/// A rail let go that snapped hangs down from its post, its free end resting
/// on the ground at most and torn across its grain rather than sawn square:
/// the break's mesh gathers about that end, splinters standing out past it,
/// and the rail is never laid past it.
#[test]
fn a_snapped_rail_ends_torn_across_its_grain() {
    use crate::compose::courses::Stonework;
    use crate::prototype::Part;
    let work = Stonework {
        stone: 3,
        mortar: 3,
        cover: None,
        age: 0.6,
        softness: 0.9,
    };
    let start = Vec3::new(0.0, 0.8, 0.09);
    let mut torn = 0;
    for seed in 0..300 {
        let mut mason = Mason::new(work, 7).expect("room");
        let mut draws = Dice::keyed(seed, 0);
        let posts = ((Point::new(0.0, 0.0), 0.0), (Point::new(2.0, 0.0), 0.0));
        rail(&mut mason, posts, 0.8, (false, &mut draws), true).expect("room");
        let building = mason.finish().expect("a structure");
        let rails: Vec<_> = building
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Solid(solid) => Some(*solid),
                _ => None,
            })
            .collect();
        let corners: Vec<u32> = building
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Facet(facet) => Some(facet.corners),
                _ => None,
            })
            .flatten()
            .collect();
        if corners.is_empty() {
            continue;
        }
        torn += 1;
        let [rail] = rails[..] else {
            panic!("{seed}: {} rails", rails.len());
        };
        assert!(
            rail.half().x < 0.75,
            "{seed}: a snapped rail runs on {}",
            2.0 * rail.half().x
        );
        let end = rail.centre() * 2.0 - start;
        assert!(
            end.y < start.y - 0.1,
            "{seed}: a snapped rail stands up from its post to {}",
            end.y
        );
        assert!(
            end.y > 0.0,
            "{seed}: a snapped rail hangs through the ground to {}",
            end.y
        );
        let along = (end - start).normalized();
        let mut farthest = f64::NEG_INFINITY;
        for corner in corners {
            let at = building.vertex(corner).expect("a vertex");
            // No splinter reaches further than one and a third of the rail's
            // depth from the corner it was pulled out at.
            assert!(
                (at - end).length() < 0.11,
                "{seed}: its break lies {} off its end",
                (at - end).length()
            );
            assert!(
                (at - start).length() > 2.0 * rail.half().x - 0.01,
                "{seed}: its break lies back along it"
            );
            farthest = farthest.max((at - end).dot(along));
        }
        assert!(
            farthest > 0.008,
            "{seed}: its break stands only {farthest} past its end"
        );
    }
    assert!(torn > 10, "only {torn} of 300 rails let go snapped");
}

/// What a farmland lets go is laid in timber older than what it keeps:
/// silvered further, and damper, so mossier.
#[test]
fn a_farmlands_neglected_timber_is_older_than_its_kept() {
    use crate::compose::{Composition, Setting};
    use crate::detail::Detail;
    use crate::pigment::Pigment;
    for seed in 0..4 {
        let mut composition = Composition::new(Setting::Farmland, seed, (640, 360), Detail::Simple)
            .expect("composes");
        composition.run_until_seen().expect("a land");
        let timbers: Vec<_> = composition
            .stage
            .materials
            .iter()
            .filter_map(|made| match &made.pigment {
                Pigment::Timber(timber) => Some((timber.weathering, timber.damp)),
                _ => None,
            })
            .collect();
        let [(kept, kept_damp), (let_go, let_go_damp)] = timbers[..] else {
            panic!(
                "{seed}: {} timbers, not the kept and the let go",
                timbers.len()
            );
        };
        assert!(
            let_go > kept && let_go_damp > kept_damp,
            "{seed}: {timbers:?}"
        );
    }
}

/// A farmed land under snow banks it deep against one face of the walls and
/// hedges standing across the wind and scours it from the other, where the
/// open fields about them lie far shallower.
#[test]
fn snow_banks_against_a_farmlands_walls_and_hedges() {
    use crate::compose::{Composition, Setting};
    use crate::detail::Detail;
    let mut composition =
        Composition::new(Setting::Farmland, 5, (640, 360), Detail::Simple).expect("composes");
    let land = composition.run_until_seen().expect("a land");
    let seen = composition.seen.as_ref().expect("seen").1.eye();
    let eye = Point::new(seen.x, seen.z);
    let fields = &composition.stage.fields;
    let snow = |at: Point| land.grids.lie(fields, at.x, at.y).snow;
    let layout = land.layout.as_ref().expect("farmed");
    let mut banked = 0;
    let mut open = Vec::new();
    for boundary in layout
        .boundaries()
        .iter()
        .filter(|boundary| matches!(boundary.kind, Bound::Wall | Bound::Hedge))
    {
        let length = plane::length(&boundary.line);
        // Midway along a stretch standing between its gaps.
        let Some(&(from, to)) = standing(&boundary.gaps, (0.0, length), 6.0)
            .expect("room")
            .first()
        else {
            continue;
        };
        let Some((middle, way)) = plane::at(&boundary.line, f64::midpoint(from, to)) else {
            continue;
        };
        if (middle - eye).length() > 400.0 {
            continue;
        }
        let left = way.left();
        let (one, other) = (snow(middle + left * 1.0), snow(middle - left * 1.0));
        open.push(snow(middle + left * (20.0 * boundary.height)));
        let (deep, shallow) = (one.max(other), one.min(other));
        if deep > 0.6 && shallow < 0.5 * deep {
            banked += 1;
        }
    }
    open.sort_by(f64::total_cmp);
    let typical = open
        .get(open.len() / 2)
        .copied()
        .expect("barriers about the eye");
    assert!(typical < 0.45, "the open fields lie {typical} deep");
    assert!(
        banked > 3,
        "only {banked} barriers bank snow against a face"
    );
}

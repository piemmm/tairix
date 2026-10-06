use super::*;

fn gap(along: f64, width: f64) -> Gap {
    Gap {
        along,
        width,
        through: Through::Gateway,
        key: 0,
    }
}

#[test]
fn a_boundary_stands_but_across_its_gaps() {
    let gaps = [gap(10.0, 4.0), gap(30.0, 1.0), gap(31.2, 1.0)];
    let standing = stretches(&gaps, (0.0, 50.0), 0.5).expect("room");
    assert_eq!(standing, [(0.0, 8.0), (12.0, 29.5), (31.7, 50.0)]);
    let short = stretches(&gaps, (29.0, 31.0), 0.6).expect("room");
    assert!(short.is_empty(), "{short:?}");
}

#[test]
fn places_spaced_along_a_stretch_take_in_both_its_ends() {
    let places: Vec<f64> = spaced((2.0, 9.0), 2.5).collect();
    assert_eq!(places.first(), Some(&2.0));
    assert_eq!(places.last(), Some(&9.0));
    assert!(places.windows(2).all(|pair| pair[1] - pair[0] <= 2.5 + 1e-9));
    assert_eq!(places.len(), 4);
}

#[test]
fn a_frame_laid_along_a_way_runs_its_x_along_it() {
    let way = Point::new(0.6, 0.8);
    let frame = along(way);
    assert!((frame.x.x - 0.6).abs() < 1e-12 && (frame.x.z - 0.8).abs() < 1e-12);
    assert!((frame.y.y - 1.0).abs() < 1e-12);
    let up = tipped(frame, 0.3);
    assert!(up.x.y > 0.29 && up.x.y < 0.3, "tipping raises x: {:?}", up.x);
}

/// A farmland's walls are built stone by stone of broken field stones where
/// they run near the eye, and none is built beyond where a wall is only
/// painted on the ground.
#[test]
fn a_farmlands_walls_are_built_of_field_stones_only_near_the_eye() {
    use crate::compose::{Composition, Setting};
    use crate::detail::Detail;
    use crate::prototype::Part;
    let reach = Detail::Simple.densities().bounds.walls;
    let mut walled = 0;
    for seed in 16..19 {
        let mut composition = Composition::new(Setting::Farmland, seed, (320, 180), Detail::Simple).expect("composes");
        let land = composition.run_until_seen().expect("a land");
        let layout = land.layout.as_ref().expect("farmed");
        let seen = composition.seen.as_ref().expect("seen").1.eye();
        let eye = Point::new(seen.x, seen.z);
        let near = layout.boundaries().iter().any(|boundary| {
            boundary.kind == Bound::Wall && plane::nearest(&boundary.line, eye).is_some_and(|near| near.distance < 0.5 * reach)
        });
        // A stile's slabs stand on end, far taller than any stone a wall is
        // laid of.
        let stones: Vec<Vec3> = composition
            .buildings()
            .flat_map(|building| building.parts())
            .filter_map(|part| match part {
                Part::Solid(solid) if matches!(solid.form(), Form::Rock { .. }) && solid.half().y < 0.35 => Some(solid.centre()),
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

use alloc::vec;
use alloc::vec::Vec;

use tairix_countryside::usage::Crop;

use super::*;
use crate::farmed::{Grown, Stage as Growth};
use crate::heightfield::{GROWN, ROWS, SNOW};
use crate::land::Land;
use crate::prototype::{Assembly, Part, Prototype, Tube};
use crate::snow;
use crate::vector::real;

/// What the test land grows where its maize stands, and how its rows run.
fn grows() -> (u8, u8) {
    (Grown::Sown(Crop::Maize, Growth::Green).code(), 40)
}

/// A level land 60 m across about the origin growing maize short of `edge`
/// along x, and nothing farmed past it, snow lying `depth` deep over it.
fn land(edge: f64, depth: f64) -> (Grids, Vec<Heightfield>) {
    let mut field = Heightfield::new(120, (-30.0, -30.0), 0.5, false).expect("a grid");
    assert!(field.carry_attributes());
    let side = field.side();
    {
        let (heights, attributes) = field.rows_mut(0..side);
        heights.fill(0.0);
        for (index, slot) in attributes.iter_mut().enumerate() {
            let x = -30.0 + 0.5 * real(index % side);
            let (code, rows) = if x < edge { grows() } else { (0, 0) };
            slot[GROWN] = code;
            slot[ROWS] = rows;
            slot[SNOW] = snow::kept(depth);
        }
    }
    field.seal();
    (Land::plain(0, ((0.0, 0.0), 30.0)).grids, vec![field])
}

/// A plant of a stalk 2 m tall and a leaf held out level from half way up.
fn prototype() -> Prototype {
    let mut assembly = Assembly::with_room(4, 0).expect("room");
    let side = Vec3::new(1.0, 0.0, 0.0);
    for (from, to, radius) in [
        (Vec3::ZERO, Vec3::new(0.0, 2.0, 0.0), 0.02),
        (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.4, 1.1, 0.0), 0.012),
    ] {
        assembly
            .push(Part::Tube(Tube::new(
                (from, to),
                ((radius, radius), (0.0, 1.0)),
                (0, 1),
                side,
            )))
            .expect("room");
    }
    assembly.finish().expect("built").whole()
}

/// The field the stand stands in: a square 40 m across about the origin.
fn field() -> Convex {
    Convex {
        corners: vec![
            Point::new(-20.0, -20.0),
            Point::new(20.0, -20.0),
            Point::new(20.0, 20.0),
            Point::new(-20.0, 20.0),
        ],
    }
}

/// The test plant, as a stand stands it.
const PLANT: Plant = Plant {
    prototype: 0,
    material: 0,
    height: 2.0,
    reach: 0.45,
};

/// A drilled stand of the test plant on `land`, seen from `eye`, thinning
/// away over `thinning`.
fn sowing(eye: (f64, f64), thinning: (f64, f64)) -> Sowing {
    Sowing {
        grows: grows(),
        drill: Drill::of(0.75, grows()),
        apart: 0.15,
        planting: DRILLED,
        plants: [PLANT; PLANTS],
        kinds: 1,
        sizes: (0.9, 1.1),
        posts: None,
        eye,
        thinning,
        seed: 7,
    }
}

fn stand((grids, fields): (Grids, &[Heightfield]), eye: (f64, f64), thinning: (f64, f64)) -> Stand {
    Stand::new((grids, fields), &field(), &sowing(eye, thinning)).expect("a stand")
}

fn geometry<'a>(fields: &'a [Heightfield], prototypes: &'a [Prototype]) -> Geometry<'a> {
    Geometry {
        faces: &[],
        fields,
        prototypes,
        lawns: &[],
        far_woods: &[],
        stands: &[],
        materials: &[],
        view: None,
    }
}

/// Every place the stand holds that stands a plant, by its cell.
fn standing(stand: &Stand, fields: &[Heightfield]) -> Vec<((u32, u32), Place)> {
    let corner = |x: f64, z: f64| {
        stand
            .on_lattice(&Ray::new(Vec3::new(x, 0.0, z), Vec3::ZERO))
            .origin
    };
    let corners = [
        corner(-21.0, -21.0),
        corner(21.0, -21.0),
        corner(-21.0, 21.0),
        corner(21.0, 21.0),
    ];
    let most = |along: fn(&Vec3) -> f64| corners.iter().map(along).fold(0.0, f64::max);
    let (columns, rows) = (most(|at| at.x), most(|at| at.z));
    let mut standing = Vec::new();
    for row in 0..=mathf::round_i32(rows).cast_unsigned() {
        for column in 0..=mathf::round_i32(columns).cast_unsigned() {
            if let Some(place) = stand
                .place((column, row))
                .filter(|&place| stand.stands(place, fields))
            {
                standing.push(((column, row), place));
            }
        }
    }
    standing
}

/// The nearest of `places`' plants `ray` meets, met one by one.
fn nearest(
    stand: &Stand,
    places: &[((u32, u32), Place)],
    ray: &Ray,
    geometry: Geometry<'_>,
) -> Option<f64> {
    places
        .iter()
        .filter_map(|&(_, place)| {
            let stood = stand.plant(place, geometry.fields)?;
            meet_placed(
                (stood.prototype, &stood.pose, stood.scale, place.key),
                ray,
                (0.0, 1e9),
                geometry,
            )
        })
        .map(|hit| hit.t)
        .reduce(f64::min)
}

/// A stand's plants come up on its drill's rows, a plant's spacing apart
/// along each, never in a tramline, on ground not growing the crop, nor
/// about the eye.
#[test]
fn a_stands_plants_come_up_on_its_rows() {
    let (grids, fields) = land(5.0, 0.0);
    let stand = stand((grids, &fields), (0.0, -10.0), (100.0, 120.0));
    let places = standing(&stand, &fields);
    assert!(places.len() > 2000, "{} plants", places.len());
    let drill = Drill::of(0.75, grows());
    for &(_, place) in &places {
        let rows = drill.rows_from(place.at);
        assert!(
            (rows - mathf::round(rows)).abs() * 0.75 <= DRILLED.jitter.1 + 1e-9,
            "a plant {rows} rows across"
        );
        assert!(
            place.at.0 < 5.0 + 0.6,
            "a plant at {:?} past the crop",
            place.at
        );
        assert!(
            drill.tracked(place.at, 0.0) < 0.5,
            "a plant in a tramline at {:?}",
            place.at
        );
        assert!(
            mathf::hypot(place.at.0, place.at.1 + 10.0) > 1.5,
            "a plant by the eye"
        );
    }
    // As many to a row's length as are sown along it.
    let area = 25.0 * 40.0;
    let sown = area / (0.75 * 0.15) * DRILLED.come_up;
    let ratio = real(places.len()) / sown;
    assert!(
        ratio > 0.8 && ratio < 1.05,
        "{ratio} of the plants sown stand"
    );
}

/// Far enough off, a stand's plants thin away and are gone.
#[test]
fn a_stands_plants_thin_away_from_the_eye() {
    let (grids, fields) = land(30.0, 0.0);
    let stand = stand((grids, &fields), (0.0, -20.0), (15.0, 30.0));
    let places = standing(&stand, &fields);
    let off = |place: &Place| mathf::hypot(place.at.0, place.at.1 + 20.0);
    assert!(
        places.iter().all(|(_, place)| off(place) < 30.0),
        "a plant past the stand's reach"
    );
    let (near, far) =
        places
            .iter()
            .fold((0u32, 0u32), |(near, far), (_, place)| match off(place) {
                off if off < 15.0 => (near + 1, far),
                off if off > 22.5 => (near, far + 1),
                _ => (near, far),
            });
    // The rings near and far hold about as much field, but the far one has
    // thinned.
    assert!(far * 3 < near, "{near} near against {far} far");
}

/// Whatever way a ray crosses a stand — along its rows, across them, down
/// on it from above or up out of it — the walk meets the same nearest plant
/// as meeting every plant one by one, and calls it in the way only where it
/// is.
#[test]
fn a_rays_walk_meets_the_nearest_plant() {
    let (grids, fields) = land(30.0, 0.0);
    let prototypes = [prototype()];
    let geometry = geometry(&fields, &prototypes);
    let stand = stand((grids, &fields), (0.0, -25.0), (60.0, 80.0));
    let places = standing(&stand, &fields);
    let draw = |index: u32, salt: u32| unit(mix32(index.wrapping_mul(0x9e37_79b9) ^ salt));
    let mut met = 0;
    for index in 0..48 {
        let target = Vec3::new(
            36.0 * draw(index, 1) - 18.0,
            1.9 * draw(index, 2),
            36.0 * draw(index, 3) - 18.0,
        );
        let origin = match index % 3 {
            0 => Vec3::new(36.0 * draw(index, 4) - 18.0, 1.0 + draw(index, 5), -25.0),
            1 => Vec3::new(
                target.x + 4.0 * draw(index, 4) - 2.0,
                12.0,
                target.z + 4.0 * draw(index, 5) - 2.0,
            ),
            _ => Vec3::new(target.x, 0.2, target.z),
        };
        let dir = if index % 3 == 2 {
            Vec3::new(draw(index, 6) - 0.5, 0.7, draw(index, 7) - 0.5).normalized()
        } else {
            (target - origin).normalized()
        };
        let ray = Ray::new(origin, dir);
        let walked = stand
            .intersect(&ray, (1e-6, 1e9), geometry)
            .map(|hit| hit.t);
        let brute = nearest(&stand, &places, &ray, geometry);
        match (walked, brute) {
            (Some(walked), Some(brute)) => {
                assert!(
                    (walked - brute).abs() < 1e-9,
                    "ray {index}: walked {walked}, met one by one {brute}"
                );
                met += 1;
            }
            (None, None) => {}
            _ => panic!("ray {index}: walked {walked:?}, met one by one {brute:?}"),
        }
        assert_eq!(
            stand.occludes(&ray, (1e-6, 1e9), geometry),
            brute.is_some(),
            "ray {index}"
        );
    }
    assert!(met > 24, "only {met} of 48 rays met a plant");
}

/// On a trellis a post stands true in place of every fifth plant, turned to
/// its row, and every plant trained along the row is turned to it either
/// way round.
#[test]
fn a_trellis_stands_its_posts_true_and_its_plants_along_it() {
    let (grids, fields) = land(30.0, 0.0);
    let post = Plant {
        reach: 0.3,
        ..PLANT
    };
    let sowing = Sowing {
        planting: Planting {
            jitter: (0.0, 0.0),
            lean: 0.0,
            trained: Some(0.06),
            come_up: 1.0,
            tramlines: false,
        },
        apart: 1.1,
        drill: Drill::of(2.2, grows()),
        posts: Some((5, post)),
        ..sowing((0.0, -25.0), (60.0, 80.0))
    };
    let stand = Stand::new((grids, &fields), &field(), &sowing).expect("a stand");
    let drill = sowing.drill;
    let row = drill.place((0.0, 1.0));
    let row = Vec3::new(row.0, 0.0, row.1);
    let places = standing(&stand, &fields);
    let posts = places.iter().filter(|(_, place)| place.post).count();
    assert!(
        posts * 6 > places.len() && posts * 4 < places.len(),
        "{posts} posts of {}",
        places.len()
    );
    for &(_, place) in &places {
        let stood = stand.plant(place, &fields).expect("a plant");
        let along = stood.pose.frame.x.dot(row);
        assert!(
            along.abs() > mathf::cos(0.06) - 1e-9,
            "a plant turned {along} to its row"
        );
        if place.post {
            assert!(
                along > 1.0 - 1e-9 && (stood.scale - 1.0).abs() < 1e-12,
                "a post set askew"
            );
            let off = drill.along(place.at) / 1.1 - 0.5;
            assert!(
                (off - mathf::round(off)).abs() < 1e-9,
                "a post off its place"
            );
        }
    }
}

/// A trellis that sets no plant apart stands no post, rather than dividing
/// its rows by nought.
#[test]
fn a_trellis_setting_no_plant_apart_stands_no_post() {
    let (grids, fields) = land(30.0, 0.0);
    let sowing = Sowing {
        posts: Some((0, PLANT)),
        ..sowing((0.0, -25.0), (60.0, 80.0))
    };
    let stand = Stand::new((grids, &fields), &field(), &sowing).expect("a stand");
    assert_eq!(stand.posts(&fields).expect("its posts").len(), 0);
    assert!(
        standing(&stand, &fields)
            .iter()
            .all(|(_, place)| !place.post),
        "a post stood"
    );
}

/// A hit on a stand's plant is placed back on the plant it met.
#[test]
fn a_hit_on_a_plant_is_placed_on_it() {
    let (grids, fields) = land(30.0, 0.0);
    let prototypes = [prototype()];
    let geometry = geometry(&fields, &prototypes);
    let stand = stand((grids, &fields), (0.0, -25.0), (60.0, 80.0));
    let ray = Ray::new(
        Vec3::new(0.3, 1.5, -25.0),
        Vec3::new(0.0, -0.02, 1.0).normalized(),
    );
    let hit = stand
        .intersect(&ray, (1e-6, 1e9), geometry)
        .expect("a plant in the way");
    let cell = hit.member.expect("the cell it met");
    let (pose, _) = stand.placing(cell, &fields).expect("its placing");
    let at = ray.at(hit.t);
    assert!(
        mathf::hypot(at.x - pose.at.x, at.z - pose.at.z) < 0.5,
        "met at {at:?}, placed at {:?}",
        pose.at
    );
}

/// Where snow lies, a plant is rooted in the ground beneath it, within the
/// stand's bounds, and a ray down through the snow still meets it.
#[test]
fn a_stands_plants_root_beneath_the_snow() {
    let (grids, fields) = land(30.0, 0.3);
    let prototypes = [prototype()];
    let geometry = geometry(&fields, &prototypes);
    let stand = stand((grids, &fields), (0.0, -25.0), (60.0, 80.0));
    let ground = grids.beneath_snow(&fields, 0.0, 0.0);
    assert!(ground < -0.25, "the ground lies at {ground}");
    let places = standing(&stand, &fields);
    assert!(!places.is_empty(), "nothing stands in the snow");
    for (cell, place) in places {
        let stood = stand.plant(place, &fields).expect("a plant");
        assert!(
            (stood.pose.at.y - (ground - ROOTED)).abs() < 1e-9,
            "{cell:?} rooted at {}",
            stood.pose.at.y
        );
        assert!(
            stood.pose.at.y >= stand.bounds().min.y,
            "{cell:?} rooted beneath its stand's bounds"
        );
    }
    let ray = Ray::new(
        Vec3::new(0.3, 1.5, -25.0),
        Vec3::new(0.0, -0.02, 1.0).normalized(),
    );
    assert!(
        stand.intersect(&ray, (1e-6, 1e9), geometry).is_some(),
        "no plant in the way"
    );
}

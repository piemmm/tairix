//! Host tests of the radiosity records: what a record keeps of the light its
//! rays bring back, checked against the light integrated directly; where it
//! holds; and that laying records down comes out the same on any number of
//! threads.

use alloc::vec::Vec;

use tairix_parallel::{Threaded, SERIAL};

use super::*;
use crate::camera::Camera;
use crate::detail::Detail;
use crate::light::{Light, Limb};
use crate::material::{Finish, Material};
use crate::pigment::Pigment;
use crate::scene::{Exposure, Object, Parts};
use crate::shape::Shape;
use crate::sky::{Dome, Gradient, Sky};
use crate::vector::{Pose, Ray};

/// The hemisphere a record's sums are checked over: the finer detail's.
const RECORDS: &Records = &Detail::Maximum.densities().records;

/// A site at the origin facing up, far enough from the eye that the picture's
/// bounds on a record's radius never bind.
const SITE: Site = Site {
    point: Vec3::ZERO,
    facing: Vec3::new(0.0, 0.0, 1.0),
    normal: Vec3::new(0.0, 0.0, 1.0),
    travelled: 1000.0,
    span: 1000.0,
    seed: 1,
};

/// A luminous panel in the plane `x = 1`, over `-1..1` in `y` and `0..1.5`
/// in `z`: what a ray from `from` along `dir` brings back, the sky dark.
fn panel(from: Vec3, dir: Vec3) -> Cell {
    if dir.x <= 1e-12 {
        return Cell::DARK;
    }
    let t = (1.0 - from.x) / dir.x;
    let at = Ray::new(from, dir).at(t);
    if at.y.abs() <= 1.0 && (0.0..=1.5).contains(&at.z) {
        Cell {
            light: Vec3::ONE,
            distance: t,
        }
    } else {
        Cell::DARK
    }
}

/// The cells a record at `site` fills from `light`: each the mean of the
/// light over its cell, so a record built from them stands or falls by its
/// own sums rather than by where a sharp edge happens to cut the cells; the
/// distance its centre's ray met.
fn cells_of(site: &Site, light: impl Fn(Vec3, Vec3) -> Cell) -> Vec<Cell> {
    const ACROSS: usize = 4;
    let frame = Frame::around(site.normal);
    let mut cells = alloc::vec![Cell::DARK; RECORDS.cells()];
    for (index, cell) in cells.iter_mut().enumerate() {
        let mut sum = Vec3::ZERO;
        for row in 0..ACROSS {
            for column in 0..ACROSS {
                let within = (
                    (real(row) + 0.5) / real(ACROSS),
                    (real(column) + 0.5) / real(ACROSS),
                );
                sum += light(
                    site.point,
                    frame.to_world(direction(RECORDS, index, within)),
                )
                .light;
            }
        }
        let centre = light(
            site.point,
            frame.to_world(direction(RECORDS, index, (0.5, 0.5))),
        );
        *cell = Cell {
            light: sum * (1.0 / real(ACROSS * ACROSS)),
            distance: centre.distance,
        };
    }
    cells
}

/// The mean radiance `light` sends a point at `point` whose normal is
/// `normal`, over the cosine-weighted hemisphere, integrated finely.
fn integrated(point: Vec3, normal: Vec3, light: impl Fn(Vec3, Vec3) -> Cell) -> f64 {
    let frame = Frame::around(normal);
    let (rows, columns) = (600usize, 1200usize);
    let mut sum = 0.0;
    for row in 0..rows {
        for column in 0..columns {
            let sin2 = (real(row) + 0.5) / real(rows);
            let (sin, cos) = (mathf::sqrt(sin2), mathf::sqrt(1.0 - sin2));
            let azimuth = TAU * (real(column) + 0.5) / real(columns);
            let dir = frame.to_world(Vec3::new(
                sin * mathf::cos(azimuth),
                sin * mathf::sin(azimuth),
                cos,
            ));
            sum += light(point, dir).light.x;
        }
    }
    sum / real(rows * columns)
}

fn record(point: Vec3, normal: Vec3, radius: f64) -> Record {
    Record {
        point,
        normal,
        light: Vec3::splat(0.5),
        radius,
        turning: [Vec3::ZERO; 3],
        moving: [Vec3::ZERO; 3],
    }
}

fn cache(records: Vec<Record>) -> Radiosity {
    let mut radiosity = Radiosity {
        records,
        ..Radiosity::default()
    };
    let mut builder = radiosity.indexing().expect("the hierarchy builds");
    while !builder.step(1) {}
    radiosity.bvh = builder.finish();
    radiosity
}

#[test]
fn an_even_sky_leaves_a_record_its_light_and_no_slope() {
    let cells = cells_of(&SITE, |_, _| Cell {
        light: Vec3::new(0.2, 0.4, 0.6),
        distance: f64::INFINITY,
    });
    let record = Record::new(&SITE, &cells, RECORDS);
    assert!((record.light - Vec3::new(0.2, 0.4, 0.6)).length() < 1e-12);
    for gradient in record.turning.iter().chain(&record.moving) {
        assert!(gradient.length() < 1e-12, "{gradient:?}");
    }
    assert!(
        (record.radius - FARTHEST * SITE.span).abs() < 1e-9,
        "nothing near: it holds far"
    );
}

/// With one half of the hemisphere lit, turning the normal toward it by an
/// angle adds half that angle to the mean radiance: the integral of the
/// cosine's change over the lit half, over π.
#[test]
fn turning_toward_the_lit_half_of_the_sky_brightens_by_half_the_angle() {
    let cells = cells_of(&SITE, |_, dir| Cell {
        light: if dir.x > 0.0 { Vec3::ONE } else { Vec3::ZERO },
        distance: f64::INFINITY,
    });
    let record = Record::new(&SITE, &cells, RECORDS);
    // Turning the normal toward +x turns it about +y.
    let turning = record.turning[0];
    assert!((turning.y - 0.5).abs() < 1e-9, "{turning:?}");
    assert!(
        turning.x.abs() < 1e-9 && turning.z.abs() < 1e-9,
        "{turning:?}"
    );
    assert!((record.light.x - 0.5).abs() < 1e-9);
}

/// Beside a luminous panel, a record's light and the way it changes as the
/// point moves toward the panel and the normal turns to face it agree with
/// the light integrated directly at the point and beside it.
#[test]
fn a_records_light_and_slopes_agree_with_the_light_integrated_directly() {
    let at = Vec3::new(0.2, 0.1, 0.0);
    let site = Site { point: at, ..SITE };
    let record = Record::new(&site, &cells_of(&site, panel), RECORDS);
    let up = SITE.normal;
    let here = integrated(at, up, panel);
    assert!(
        (record.light.x - here).abs() < 0.03 * here,
        "{} against {here}",
        record.light.x
    );

    let step = 0.02;
    let toward = (integrated(at + Vec3::new(step, 0.0, 0.0), up, panel)
        - integrated(at - Vec3::new(step, 0.0, 0.0), up, panel))
        / (2.0 * step);
    let moving = record.moving[0];
    assert!(toward > 0.0);
    assert!(
        (moving.x - toward).abs() < 0.2 * toward,
        "moving {moving:?} against {toward}"
    );

    let angle = 0.05;
    let tilted = |angle: f64| Vec3::new(mathf::sin(angle), 0.0, mathf::cos(angle));
    let turning = (integrated(at, tilted(angle), panel) - integrated(at, tilted(-angle), panel))
        / (2.0 * angle);
    let predicted = record.turning[0].dot(up.cross(tilted(1.0)).normalized());
    assert!(turning > 0.0);
    assert!(
        (predicted - turning).abs() < 0.1 * turning,
        "turning {predicted} against {turning}"
    );
}

#[test]
fn a_record_holds_near_itself_and_not_behind_askew_or_beyond_its_radius() {
    let up = Vec3::new(0.0, 0.0, 1.0);
    let radiosity = cache(alloc::vec![record(Vec3::ZERO, up, 1.0)]);
    let light = |point: Vec3, normal: Vec3| radiosity.light(point, normal, normal);
    assert_eq!(light(Vec3::ZERO, up), Some(Vec3::splat(0.5)));
    assert!(light(Vec3::new(0.5, 0.0, 0.0), up).is_some());
    assert!(
        light(Vec3::new(1.1, 0.0, 0.0), up).is_none(),
        "beyond its radius"
    );
    assert!(
        light(Vec3::new(0.0, 0.0, 0.3), up).is_some(),
        "in front of its surface"
    );
    assert!(
        light(Vec3::new(0.0, 0.0, -0.3), up).is_none(),
        "behind its surface"
    );
    let turned = |degrees: f64| {
        let angle = degrees.to_radians();
        Vec3::new(mathf::sin(angle), 0.0, mathf::cos(angle))
    };
    // Turned well within its fifteen degrees, a lone record still weighs
    // enough to hold.
    assert!(
        light(Vec3::ZERO, turned(10.0)).is_some(),
        "turned ten degrees"
    );
    for degrees in [16.0, 25.0, 40.0] {
        assert!(
            light(Vec3::ZERO, turned(degrees)).is_none(),
            "turned {degrees} degrees away"
        );
    }
    assert!(radiosity.holds(Vec3::new(0.2, 0.2, 0.0), up));
    assert!(!radiosity.holds(Vec3::new(3.0, 0.0, 0.0), up));
}

#[test]
fn records_blend_by_their_weights_and_slopes_carry_light_but_never_below_nothing() {
    let up = Vec3::new(0.0, 0.0, 1.0);
    let mut sloped = record(Vec3::ZERO, up, 2.0);
    sloped.moving = [Vec3::new(1.0, 0.0, 0.0); 3];
    let radiosity = cache(alloc::vec![sloped]);
    let carried = radiosity
        .light(Vec3::new(0.25, 0.0, 0.0), up, up)
        .expect("it holds");
    assert!((carried.x - 0.75).abs() < 1e-12, "{carried:?}");
    let below = radiosity
        .light(Vec3::new(-0.9, 0.0, 0.0), up, up)
        .expect("it holds");
    assert_eq!(below, Vec3::ZERO);

    let mut bright = record(Vec3::new(1.0, 0.0, 0.0), up, 2.0);
    bright.light = Vec3::splat(1.5);
    let pair = cache(alloc::vec![record(Vec3::ZERO, up, 2.0), bright]);
    let middle = pair
        .light(Vec3::new(0.5, 0.0, 0.0), up, up)
        .expect("both hold");
    assert!(
        (middle.x - 1.0).abs() < 1e-12,
        "equal weights meet half way: {middle:?}"
    );
    let nearer = pair
        .light(Vec3::new(0.8, 0.0, 0.0), up, up)
        .expect("both hold");
    assert!(nearer.x > 1.0, "the nearer record weighs more: {nearer:?}");
}

/// A floor lit by a low sun, a wall standing on it and a roof over part of
/// it, seen from the side: light bounces floor to wall to roof.
fn courtyard() -> Scene {
    let matte = |grey: f64| Material::new(Pigment::Solid(Vec3::splat(grey)), Finish::Matte);
    let object = |shape: Shape, material: usize| Object {
        shape,
        material,
        texture: Pose::new(Vec3::ZERO, Frame::WORLD),
        light: None,
        filter: None,
        in_view: true,
    };
    let sky = Sky {
        dome: Dome::Gradient(Gradient {
            zenith: Vec3::splat(0.2),
            horizon: Vec3::splat(0.3),
            ground: Vec3::splat(0.1),
        }),
        stars: None,
        low: None,
        high: None,
    };
    Scene::new(Parts {
        objects: alloc::vec![
            object(
                Shape::Plane {
                    normal: Vec3::UP,
                    offset: 0.0
                },
                0
            ),
            object(
                Shape::Quad {
                    corner: Vec3::new(-2.0, 2.0, -2.0),
                    edge_u: Vec3::new(0.0, 0.0, 4.0),
                    edge_v: Vec3::new(4.0, 0.0, 0.0),
                },
                1,
            ),
            object(
                Shape::Quad {
                    corner: Vec3::new(2.0, 0.0, -2.0),
                    edge_u: Vec3::new(0.0, 2.0, 0.0),
                    edge_v: Vec3::new(0.0, 0.0, 4.0),
                },
                1,
            ),
        ],
        faces: Vec::new(),
        fields: Vec::new(),
        prototypes: Vec::new(),
        lawns: Vec::new(),
        materials: alloc::vec![matte(0.6), matte(0.8)],
        lights: alloc::vec![Light::Sun {
            toward: Vec3::new(-0.6, 0.5, 0.2).normalized(),
            cos_radius: 0.99999,
            radiance: Vec3::splat(4000.0),
            limb: Limb::Even,
        }],
        sky,
        shades: None,
        camera: Camera::looking(
            Vec3::new(-3.0, 1.0, 0.5),
            Vec3::new(1.0, 1.2, 0.0),
            1.2,
            1.5,
            (0.0, 1.0),
        ),
        exposure: Exposure::Fixed(1.0),
    })
    .expect("a scene")
}

/// The picture the courtyard's records are laid over.
const PICTURE: (u32, u32) = (96, 64);

fn gathered(scene: &Scene, runner: &dyn JobRunner, detail: Detail) -> Radiosity {
    let mut gathering =
        Gathering::new(PICTURE, &detail.densities().records).expect("room to gather");
    while !gathering.step(scene, runner).expect("room to gather") {}
    gathering.finish()
}

/// However many cores share the work — so however a record's rows fall
/// across units, one begun in one unit and finished in another — the same
/// records are laid.
#[test]
fn gathering_lays_the_same_records_on_one_thread_as_on_several() {
    let scene = courtyard();
    for detail in Detail::ALL {
        let alone = gathered(&scene, &SERIAL, detail);
        assert!(!alone.records.is_empty());
        let runners: [&dyn JobRunner; 2] = [&Threaded::new(3), &tairix_parallel::Reversed::new(5)];
        for runner in runners {
            let shared = gathered(&scene, runner, detail);
            assert_eq!(alone.records.len(), shared.records.len());
            for (a, b) in alone.records.iter().zip(&shared.records) {
                assert_eq!(a.point, b.point);
                assert_eq!(a.light, b.light);
                assert_eq!(a.radius.to_bits(), b.radius.to_bits());
                assert_eq!((a.turning, a.moving), (b.turning, b.moving));
            }
        }
    }
}

#[test]
fn the_records_stand_for_what_every_sample_would_trace() {
    let scene = courtyard();
    let radiosity = gathered(&scene, &Threaded::new(4), Detail::Maximum);
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, PICTURE, 0);
    let mut compared = 0;
    for y in (3..PICTURE.1).step_by(7) {
        for x in (3..PICTURE.0).step_by(7) {
            let Some(site) = tracer.site((x, y)) else {
                continue;
            };
            let Some(recorded) = radiosity.light(site.point, site.normal, site.normal) else {
                continue;
            };
            let mut cells = alloc::vec![Cell::DARK; RECORDS.cells()];
            let mean = |seed: u32, cells: &mut [Cell]| {
                for (row, piece) in cells.chunks_mut(RECORDS.columns).enumerate() {
                    tracer.gather(&Site { seed, ..site }, (RECORDS, row), piece);
                }
                cells.iter().map(|cell| cell.light.x).sum::<f64>() / real(RECORDS.cells())
            };
            let gathered = (0..8u32).map(|seed| mean(seed, &mut cells)).sum::<f64>() / 8.0;
            assert!(
                (recorded.x - gathered).abs() < 0.15 * gathered.max(0.02),
                "at {x},{y}: recorded {recorded:?} against {gathered}"
            );
            compared += 1;
        }
    }
    assert!(compared > 20, "{compared} points compared");
}

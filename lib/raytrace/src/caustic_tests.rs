//! Host tests of the caustics: beams laid as a known field and gathered
//! against the light that field must bring, and the survey, laying and
//! gathering over real water.

use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_parallel::{Threaded, SERIAL};
use tairix_util::mathf;

use super::*;
use crate::camera::Camera;
use crate::detail::Detail;
use crate::light::{Light, Limb};
use crate::material::{Finish, Material, Relief, Wind};
use crate::pigment::Pigment;
use crate::scene::{Exposure, Object, Parts};
use crate::sky::{Dome, Gradient, Sky};
use crate::vector::{Frame, Pose};

/// Caustics over still water at height nought, its tiles at `places` (in
/// order) each cut to `finest`, every beam drifting as `drift` has it where
/// it leaves the surface, a level surface's beams as `level`, the sun a
/// point and every wave resolved.
fn laid(
    places: &[(i32, i32)],
    finest: u32,
    level: [[f64; 2]; 2],
    drift: impl Fn(f64, f64) -> [[f64; 2]; 2],
) -> Caustics {
    let flat = Flat {
        drift: level.map(|drift| drift.map(|value| f64::from(single(value)))),
        flux: [1.0; 2],
        disc: [(0.0, 0.0); 2],
        bend: [1.0; 2],
        swing: [0.0; 2],
        cast: true,
    };
    let mut caustics = Caustics {
        toward: Vec3::UP,
        way: (1.0, 0.0),
        texel: 1.0,
        ..Caustics::default()
    };
    let (mut beams, mut bounds) = (0, 0);
    for &place in places {
        caustics.tiles.push(Tile {
            place,
            finest,
            layers: caustics.layers.len(),
        });
        for level in 1..=finest {
            let layer = Layer {
                level,
                beams,
                bounds,
            };
            beams += layer.vertices();
            bounds += nodes(layer.cells());
            caustics.layers.push(layer);
        }
    }
    caustics.beams = vec![Beam::DRY; beams];
    caustics.bounds = vec![Bounds::EMPTY; bounds];
    for tile in &caustics.tiles {
        for layer in &caustics.layers[tile.layers..tile.layers + tile.finest as usize] {
            let side = layer.cells() + 1;
            let (corner, step) = (tile.corner(), cell(layer.level));
            for row in 0..side {
                for column in 0..side {
                    let at = (corner.0 + step * real(column), corner.1 + step * real(row));
                    caustics.beams[layer.beams + row * side + column] = Beam {
                        height: 0.0,
                        drift: drift(at.0, at.1).map(|drift| drift.map(single)),
                        flux: [1.0; 2],
                    };
                }
            }
            let end = layer.bounds + nodes(layer.cells());
            let own = &mut caustics.bounds[layer.bounds..end];
            seal_foot(layer, &caustics.beams, 0, own);
            seal_rungs(layer, own);
        }
    }
    caustics.sheets.push(Sheet {
        object: 0,
        ior: 1.333,
        top: 1.0,
        flat,
        tilt: 0.0,
        slope: 0.0,
        clear: 0.0,
        unresolved: [0.0; FINEST as usize],
        curvature: [1.0; FINEST as usize],
        tiles: 0..places.len(),
        stray: [[0.0; 2]; 2],
        heights: (f64::INFINITY, f64::NEG_INFINITY),
    });
    caustics.measure();
    caustics
}

/// A footprint fine enough that a point gathers at a tile's finest level.
const FINE: f64 = 1e-6;

/// A refracted beam's drift across x per metre it sinks, under ripples of
/// `amplitude` along x of length `length` over a drift of `mean`.
fn rippled(mean: f64, amplitude: f64, length: f64) -> impl Fn(f64, f64) -> [[f64; 2]; 2] {
    move |x, _| {
        [
            [mean + amplitude * mathf::sin(TAU * x / length), 0.0],
            [0.0, 0.0],
        ]
    }
}

/// Where along x a beam leaving `x` lands `depth` down under `ripple`.
fn landing(x: f64, depth: f64, (mean, amplitude, length): (f64, f64, f64)) -> f64 {
    x + depth * (mean + amplitude * mathf::sin(TAU * x / length))
}

/// The light a box `half` either way of `target` along x gathers, `depth`
/// down under `ripple`, against a level surface's: the share of a million
/// surface points across `span` whose beams land within it.
fn boxed(target: f64, half: f64, depth: f64, ripple: (f64, f64, f64), span: (f64, f64)) -> f64 {
    let points = 1_000_000u32;
    let step = (span.1 - span.0) / f64::from(points);
    let landed = (0..points)
        .filter(|&point| {
            let x = span.0 + step * (f64::from(point) + 0.5);
            (landing(x, depth, ripple) - target).abs() <= half
        })
        .count();
    real(landed) * step / (2.0 * half)
}

/// A sun's disc as wide as blurs a point `half` either way at `depth`.
fn disc_for(half: f64, depth: f64) -> [(f64, f64); 2] {
    let disc = half / (DISC_TO_BOX * depth);
    [(disc, disc); 2]
}

/// Over water whose every beam drifts as a level surface's, every point at
/// every depth takes exactly a level surface's light, across tile seams too.
#[test]
fn a_level_surface_sends_every_point_its_own_light() {
    let level = [[0.31, -0.17], [-1.2, 0.8]];
    let caustics = laid(&[(0, 0), (0, 1), (1, 0), (1, 1)], 6, level, |_, _| level);
    for depth in [0.05, 0.4, 1.3, 3.0] {
        for step in 0..40 {
            let (x, z) = (0.9 + 0.055 * f64::from(step), 1.7 + 0.017 * f64::from(step));
            let beneath = caustics.beneath(0, Vec3::new(x, -depth, z), depth, FINE);
            let over = caustics.over(0, Vec3::new(x, depth, z), depth, FINE);
            assert!(
                (beneath - 1.0).abs() < 1e-9,
                "{depth} m down at {x}: {beneath}"
            );
            assert!((over - 1.0).abs() < 1e-9, "{depth} m up at {x}: {over}");
        }
    }
}

/// Short of a focus and past it, a point takes the light of every beam that
/// reaches it, gathered or spread as the waves' slopes bend the beams about
/// it: the light a trough focuses and a crest spreads, and past the focus
/// the beams of every surface point that has crossed to it.
#[test]
fn every_beam_reaching_a_point_brings_it_light_before_a_focus_and_past_it() {
    // Past a focus a fold's curved edge is cut into the beams' straight ones,
    // which a box a few cells wide averages out as the sun's disc would.
    let cases = [
        ((0.1, 0.02, 0.5), 0.5, 0.01, (0.9, 1.1), 3e-3),
        ((0.05, 0.2, 0.5), 1.0, 0.02, (0.5, 2.0), 1e-2),
    ];
    for (ripple, depth, half, lit, tolerance) in cases {
        let mut caustics = laid(
            &[(0, 0)],
            FINEST,
            [[ripple.0, 0.0], [0.0, 0.0]],
            rippled(ripple.0, ripple.1, ripple.2),
        );
        caustics.sheets[0].flat.disc = disc_for(half, depth);
        let (mut brightest, mut dimmest) = (0.0f64, f64::INFINITY);
        for step in 0..40 {
            let target = 0.8 + 0.01 * f64::from(step);
            let expected = boxed(target, half, depth, ripple, (0.3, 1.7));
            let gathered = caustics.beneath(0, Vec3::new(target, -depth, 1.0), depth, FINE);
            assert!(
                (gathered - expected).abs() < tolerance * expected.max(1.0),
                "{depth} m down at {target}: {gathered} against {expected}"
            );
            brightest = brightest.max(gathered);
            dimmest = dimmest.min(gathered);
        }
        assert!(
            brightest > lit.1 && dimmest < lit.0,
            "{dimmest}..{brightest}"
        );
    }
}

/// Blurred by the sun's disc, the waves still pass all the light a level
/// surface would: across a whole wave's breadth of the bed the light comes
/// to a level surface's, before the focus and past it.
#[test]
fn the_waves_move_the_sun_s_light_but_lose_none_of_it() {
    for (amplitude, depth) in [(0.02, 0.5), (0.2, 1.0)] {
        let ripple = (0.05, amplitude, 0.5);
        let mut caustics = laid(
            &[(0, 0)],
            FINEST,
            [[ripple.0, 0.0], [0.0, 0.0]],
            rippled(ripple.0, ripple.1, ripple.2),
        );
        caustics.sheets[0].flat.disc = [(0.01, 0.01); 2];
        let points = 2000;
        let total: f64 = (0..points)
            .map(|step| {
                let target = 0.75 + ripple.2 * (f64::from(step) + 0.5) / f64::from(points);
                caustics.beneath(0, Vec3::new(target, -depth, 1.0), depth, FINE)
            })
            .sum();
        let mean = total / f64::from(points);
        assert!(
            (mean - 1.0).abs() < 2e-3,
            "{amplitude} at {depth} m: {mean}"
        );
    }
}

/// A point near a tile's edge gathers beams from the tile beside it as from
/// its own, so the light runs on across the seam.
#[test]
fn the_light_runs_on_across_a_seam_between_tiles() {
    let ripple = (0.1, 0.03, 0.37);
    let depth = 0.8;
    let mut caustics = laid(
        &[(0, 0), (1, 0)],
        8,
        [[ripple.0, 0.0], [0.0, 0.0]],
        rippled(ripple.0, ripple.1, ripple.2),
    );
    let half = 0.01;
    caustics.sheets[0].flat.disc = disc_for(half, depth);
    for step in 0..60 {
        let target = 1.9 + 0.005 * f64::from(step);
        let expected = boxed(target, half, depth, ripple, (1.0, 3.0));
        let gathered = caustics.beneath(0, Vec3::new(target, -depth, 0.7), depth, FINE);
        assert!(
            (gathered - expected).abs() < 3e-3 * expected,
            "{target}: {gathered} against {expected}"
        );
    }
}

/// As the footprint a point is seen over grows, it gathers at coarser
/// levels, blending each into the next with no step between them.
#[test]
fn the_detail_a_point_gathers_changes_smoothly_with_its_footprint() {
    let ripple = (0.1, 0.03, 0.11);
    let caustics = laid(
        &[(0, 0)],
        FINEST,
        [[ripple.0, 0.0], [0.0, 0.0]],
        rippled(ripple.0, ripple.1, ripple.2),
    );
    let depth = 0.6;
    let at = Vec3::new(1.03, -depth, 1.0);
    let steps = 2000u32;
    let mut previous: Option<f64> = None;
    let mut worst = 0.0f64;
    for step in 0..=steps {
        // From level eight down to nought, a level surface's.
        let level = 8.0 * (1.0 - f64::from(step) / f64::from(steps));
        let footprint = TILE / mathf::exp(level * core::f64::consts::LN_2);
        let gathered = caustics.beneath(0, at, depth, footprint);
        if let Some(previous) = previous {
            worst = worst.max((gathered - previous).abs());
        }
        previous = Some(gathered);
    }
    assert!(worst < 5e-3, "a step of {worst}");
    assert!(
        (caustics.beneath(0, at, depth, 4.0 * TILE) - 1.0).abs() < 1e-12,
        "coarser than a tile, a level surface's light"
    );
}

/// Where no beams are laid, a point takes a level surface's light; and at a
/// laid tile's edge, what lies beyond it is filled with the same.
#[test]
fn beyond_the_beams_laid_a_point_takes_a_level_surface_s_light() {
    let level = [[0.2, 0.0], [0.0, 0.0]];
    let caustics = laid(&[(0, 0)], 5, level, |_, _| level);
    for x in [-3.0, -0.0001, 1.9999, 2.0, 2.0003, 9.0] {
        let gathered = caustics.beneath(0, Vec3::new(x, -0.5, 1.0), 0.5, FINE);
        assert!((gathered - 1.0).abs() < 1e-9, "{x}: {gathered}");
    }
    assert!((caustics.beneath(7, Vec3::new(1.0, -0.5, 1.0), 0.5, FINE) - 1.0).abs() < 1e-12);
}

/// A triangle is clipped to the box exactly: whole within it, nothing
/// without, and the share a box's side cuts off.
#[test]
fn a_box_clips_what_it_gathers_to_its_own_area() {
    let gauge = Gauge::new((0.0, 0.0), (1.0, 0.0), (1.0, 0.5));
    assert!((gauge.share([(-0.5, -0.2), (0.5, -0.2), (0.0, 0.3)]) - 1.0).abs() < 1e-12);
    assert!(gauge.share([(3.0, 0.0), (4.0, 0.0), (3.5, 1.0)]).abs() < 1e-12);
    // A right triangle of area 2 from (0, 0) to (2, 0) and (0, 2), which the
    // box meets in the rectangle a metre by half a metre.
    let cut = gauge.share([(0.0, 0.0), (2.0, 0.0), (0.0, 2.0)]);
    assert!((cut - 0.25).abs() < 1e-12, "{cut}");
    // Turned a quarter, the box's long side lies along z.
    let turned = Gauge::new((0.0, 0.0), (0.0, 1.0), (1.0, 0.5));
    assert!((turned.covered((-0.5, -1.0), 1.0) - 1.0).abs() < 1e-12);
    assert!((turned.covered((-3.0, -3.0), 6.0) - turned.area()).abs() < 1e-12);
}

/// Asked for together, two asks bring the most bending, the finest footprint
/// and the narrowest patch either brought.
#[test]
fn two_asks_of_a_tile_ask_as_much_as_either() {
    let one = Asked {
        bent: 0.5,
        fine: 0.01,
        patch: 0.2,
    };
    let other = Asked {
        bent: 3.0,
        fine: 0.03,
        patch: 0.05,
    };
    for joined in [one.joined(other), other.joined(one)] {
        assert!((joined.bent - 3.0).abs() < 1e-12);
        assert!((joined.fine - 0.01).abs() < 1e-12);
        assert!((joined.patch - 0.05).abs() < 1e-12);
    }
}

/// Tiles asked for again are laid once each, every sheet's in order of
/// place, and cut as finely as the finest footprint any ask of them brought.
#[test]
fn tiles_asked_for_twice_are_laid_once_for_the_most_asked() {
    let scene = pool(0.004);
    let focus = &Detail::Simple.densities().focus;
    let mut focusing = Focusing::new(&scene, (64, 64), focus).expect("a focusing");
    let lit = |at, footprint| Lit {
        sheet: 0,
        way: Way::Sunk,
        at,
        gone: 0.5,
        footprint,
        seen: 0.0,
    };
    for asked in [
        lit((1.0, 1.0), 0.02),
        lit((9.0, 5.0), 0.02),
        lit((-3.0, 1.0), 0.02),
        lit((1.0, 1.0), 0.004),
    ] {
        let (reach, wanted) = focusing.laid.wanted(&asked).expect("tiles asked for");
        focusing.ask(reach, wanted).expect("asked");
    }
    focusing.plan().expect("planned");
    let places: Vec<_> = focusing.laid.tiles.iter().map(|tile| tile.place).collect();
    assert_eq!(places, vec![(-2, 0), (0, 0), (4, 2)]);
    let finest: Vec<_> = focusing.laid.tiles.iter().map(|tile| tile.finest).collect();
    assert_eq!(finest, vec![7, 9, 7]);
    assert_eq!(focusing.laid.sheets[0].tiles, 0..3);
}

/// A run of points asking for the same tiles asks for them just as each
/// point asking alone would.
#[test]
fn a_run_of_points_asks_as_each_point_alone_would() {
    let (scene, size) = (pool(0.004), (64, 64));
    let focus = &Detail::Simple.densities().focus;
    let mut surveyed = Focusing::new(&scene, size, focus).expect("a focusing");
    let points = surveyed.points();
    surveyed
        .survey(&scene, 0..points, &SERIAL)
        .expect("surveyed");
    let mut alone = Focusing::new(&scene, size, focus).expect("a focusing");
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, size, 0);
    let pixel = scene.camera.pixel_angle(size.1);
    for point in 0..points {
        let at = alone.pixel(u32::try_from(point).expect("a point"));
        let lits = alone.laid.lit(&scene, tracer.eye_ray(at), pixel);
        for lit in lits.into_iter().flatten() {
            if let Some((reach, wanted)) = alone.laid.wanted(&lit) {
                alone.ask(reach, wanted).expect("asked");
            }
        }
    }
    let held = |focusing: &Focusing| {
        let mut held: Vec<_> = focusing
            .asked
            .iter()
            .map(|(&key, asked)| {
                let Asked { bent, fine, patch } = *asked;
                (key, [bent, fine, patch].map(f64::to_bits))
            })
            .collect();
        held.sort_unstable();
        held
    };
    let folded = held(&surveyed);
    assert!(folded.len() > 2, "{} tiles", folded.len());
    assert_eq!(folded, held(&alone));
}

/// A point seen over a footprint that says nothing takes a level surface's
/// light, never the finest detail laid.
#[test]
fn a_point_seen_over_no_footprint_takes_a_level_surface_s_light() {
    let ripple = (0.1, 0.03, 0.37);
    let mut caustics = laid(
        &[(0, 0)],
        FINEST,
        [[ripple.0, 0.0], [0.0, 0.0]],
        rippled(ripple.0, ripple.1, ripple.2),
    );
    caustics.sheets[0].flat.disc = disc_for(0.01, 0.8);
    let at = Vec3::new(1.0, -0.8, 1.0);
    let detailed = caustics.beneath(0, at, 0.8, FINE);
    assert!((detailed - 1.0).abs() > 0.01, "a ripple's light {detailed}");
    caustics.sheets[0].flat.swing = [f64::NAN; 2];
    assert!((caustics.beneath(0, at, 0.8, f64::NAN) - 1.0).abs() < 1e-12);
}

/// A level surface bends the sun's beam as Snell's law has it and passes
/// all but what Fresnel's reflects; a low sun's reflected glitter, under
/// steep waves, is left to its mean.
#[test]
fn a_level_surface_bends_and_shares_the_sun_s_light_as_its_index_has_it() {
    let elevation: f64 = 0.9;
    let toward = Vec3::new(mathf::cos(elevation), mathf::sin(elevation), 0.0);
    let flat = Flat::new(toward, 0.0046, 1.333, 0.002).expect("a level surface");
    let sin_t = mathf::cos(elevation) / 1.333;
    let tan_t = sin_t / mathf::sqrt(1.0 - sin_t * sin_t);
    // Kept to a single's precision, as a beam keeps its drift.
    assert!((flat.drift[0][0] + tan_t).abs() < 1e-6, "{:?}", flat.drift);
    assert!((flat.drift[1][0] + mathf::cos(elevation) / mathf::sin(elevation)).abs() < 1e-6);
    let reflected = fresnel(toward.y, 1.333);
    assert!((flat.flux[0] - (1.0 - reflected) * toward.y).abs() < 1e-12);
    assert!((flat.flux[1] - reflected * toward.y).abs() < 1e-12);
    assert!(flat.cast, "a sun this high resolves its reflection");
    let low = Vec3::new(mathf::cos(0.08), mathf::sin(0.08), 0.0);
    assert!(!Flat::new(low, 0.0046, 1.333, 0.03).expect("a low sun").cast);
    assert!(Flat::new(
        Vec3::new(1.0, -0.01, 0.0).normalized(),
        0.0046,
        1.333,
        0.002
    )
    .is_none());
}

/// A pool of clear water over sand, seen from above in a high sun, its
/// surface ruffled to `slope_variance`.
fn pool(slope_variance: f64) -> crate::scene::Scene {
    let sky = Sky {
        dome: Dome::Gradient(Gradient {
            zenith: Vec3::new(0.15, 0.25, 0.5),
            horizon: Vec3::new(0.4, 0.5, 0.7),
            ground: Vec3::splat(0.2),
        }),
        stars: None,
        low: None,
        high: None,
    };
    let waves = Relief::waves(
        Wind {
            slope_variance,
            lengths: (2.0, 0.03),
            spread: 0.7,
            gusts: (1.0, 50.0),
        },
        7,
    )
    .expect("waves");
    let water = Material::new(
        Pigment::Solid(Vec3::ONE),
        Finish::Glass {
            ior: 1.333,
            absorb: Vec3::new(0.3, 0.06, 0.045),
            glow: Vec3::ZERO,
            roughness: 0.0,
            dispersion: 0.0,
            foam: None,
        },
    )
    .with_relief(waves);
    let sand = Material::new(Pigment::Solid(Vec3::new(0.75, 0.68, 0.5)), Finish::Matte);
    let flat = Pose::new(Vec3::ZERO, Frame::WORLD);
    let plane = |offset: f64, material: usize, filter: Option<Vec3>| Object {
        shape: Shape::Plane {
            normal: Vec3::UP,
            offset,
        },
        material,
        texture: flat,
        light: None,
        filter,
        in_view: true,
    };
    let toward = Vec3::new(0.35, 0.82, 0.46).normalized();
    let cos_radius = mathf::cos(0.27f64.to_radians());
    let solid = TAU * (1.0 - cos_radius);
    crate::scene::Scene::new(Parts {
        objects: vec![plane(-0.5, 1, None), plane(0.0, 0, Some(Vec3::splat(0.98)))],
        faces: Vec::new(),
        fields: Vec::new(),
        prototypes: Vec::new(),
        lawns: Vec::new(),
        materials: vec![water, sand],
        lights: vec![Light::Sun {
            toward,
            cos_radius,
            radiance: Vec3::splat(3.0 / solid),
            limb: Limb::Even,
        }],
        sky,
        shades: None,
        camera: Camera::looking(
            Vec3::new(0.0, 1.6, -1.2),
            Vec3::new(0.0, -0.5, 0.6),
            0.9,
            1.0,
            (0.0, 1.0),
        ),
        exposure: Exposure::Fixed(0.6),
    })
    .expect("a pool")
}

/// The caustics `scene` lays for a picture of `size` at the plainer detail
/// across `runner`.
fn focused(scene: &crate::scene::Scene, size: (u32, u32), runner: &dyn JobRunner) -> Caustics {
    let focus = &Detail::Simple.densities().focus;
    let mut focusing = Focusing::new(scene, size, focus).expect("a focusing");
    while !focusing.step(scene, runner).expect("a step") {}
    focusing.finish()
}

/// Where the eye's ray through pixel `at` meets the pool's sand, how deep
/// beneath the surface it lies there, and how finely the eye resolves it.
fn bed(scene: &crate::scene::Scene, size: (u32, u32), at: (u32, u32)) -> Option<(Vec3, f64, f64)> {
    let encoder = Encoder::new()?;
    let tracer = Tracer::new(scene, &encoder, size, 0);
    let ray = tracer.eye_ray(at);
    let (_, hit) = scene.closest(&ray, f64::INFINITY, Sight::Eye)?;
    let inward = refract(ray.dir, Vec3::UP, 1.333)?;
    let inside = Ray::new(lift(ray.at(hit.t), -Vec3::UP), inward);
    let (_, under) = scene.closest(&inside, f64::INFINITY, Sight::Bounce)?;
    let point = inside.at(under.t);
    let pixel = scene.camera.pixel_angle(size.1);
    Some((
        point,
        -point.y,
        resolved((hit.t + under.t) * pixel, Vec3::UP, -inward),
    ))
}

/// The survey lays beams under the water the picture looks into, and
/// none over still water, whose light needs none.
#[test]
fn beams_are_laid_where_the_picture_looks_into_rippled_water() {
    let size = (48, 48);
    let rippled = pool(0.004);
    let caustics = focused(&rippled, size, &SERIAL);
    assert_eq!(caustics.sheets.len(), 1);
    assert!(caustics.tiles.len() >= 2, "{} tiles", caustics.tiles.len());
    assert!(caustics.beams.iter().any(|beam| !beam.height.is_nan()));
    // No tile lies wholly behind the eye, which looks toward +z.
    for tile in &caustics.tiles {
        assert!(
            TILE * f64::from(tile.place.1 + 1) > -1.2 - TILE,
            "{:?}",
            tile.place
        );
    }
    let still = pool(0.0);
    assert!(focused(&still, size, &SERIAL).tiles.is_empty());
}

/// Real waves over the pool focus the sun's light into a net of bright lines
/// on its bed and leave dimmer cells between, and pass all of it on average.
#[test]
fn real_waves_draw_a_net_of_light_on_the_bed_and_lose_none_of_it() {
    let size = (128, 128);
    let mut scene = pool(0.004);
    scene.caustics = focused(&scene, size, &SERIAL);
    let mut factors = Vec::new();
    for y in (16..128).step_by(4) {
        for x in (0..128).step_by(4) {
            let Some((point, depth, footprint)) = bed(&scene, size, (x, y)) else {
                continue;
            };
            factors.push(scene.caustics.beneath(1, point, depth, footprint));
        }
    }
    assert!(factors.len() > 500, "{} points", factors.len());
    let mean = factors.iter().sum::<f64>() / real(factors.len());
    let spread = mathf::sqrt(
        factors
            .iter()
            .map(|factor| (factor - mean) * (factor - mean))
            .sum::<f64>()
            / real(factors.len()),
    );
    let brightest = factors.iter().copied().fold(0.0, f64::max);
    assert!((mean - 1.0).abs() < 0.03, "a mean of {mean}");
    assert!(spread > 0.12, "a spread of {spread}");
    assert!(brightest > 1.6, "the brightest line only {brightest}");
}

/// The beams come out the same however their filling is divided among
/// cores.
#[test]
fn beams_are_laid_the_same_on_any_runner() {
    let size = (40, 40);
    let scene = pool(0.004);
    let alone = focused(&scene, size, &SERIAL);
    let shared = focused(&scene, size, &Threaded::new(4));
    assert_eq!(alone.tiles.len(), shared.tiles.len());
    assert_eq!(alone.beams.len(), shared.beams.len());
    let same = |a: &Beam, b: &Beam| {
        a.height.to_bits() == b.height.to_bits()
            && a.flux.map(f32::to_bits) == b.flux.map(f32::to_bits)
            && a.drift.map(|drift| drift.map(f32::to_bits))
                == b.drift.map(|drift| drift.map(f32::to_bits))
    };
    assert!(alone
        .beams
        .iter()
        .zip(&shared.beams)
        .all(|(a, b)| same(a, b)));
    assert!(alone
        .bounds
        .iter()
        .zip(&shared.bounds)
        .all(|(a, b)| a.height.map(f32::to_bits) == b.height.map(f32::to_bits)));
}

/// The pool with its water's waves framed by `frame`.
fn framed(frame: Frame) -> crate::scene::Scene {
    let mut scene = pool(0.004);
    scene.objects[1].texture = Pose::new(Vec3::new(0.3, 0.0, -0.7), frame);
    scene
}

/// A stretch of one of `scene`'s rows of finest beams as laid, and as read
/// afresh at each of its points.
fn laid_and_afresh(scene: &crate::scene::Scene) -> (Vec<Beam>, Vec<Beam>) {
    let focus = &Detail::Simple.densities().focus;
    let focusing = Focusing::new(scene, (32, 32), focus).expect("a focusing");
    let (sheet, toward) = (&focusing.laid.sheets[0], focusing.laid.toward);
    let tile = Tile {
        place: (1, 0),
        finest: FINEST,
        layers: 0,
    };
    let layer = Layer {
        level: FINEST,
        beams: 0,
        bounds: 0,
    };
    let (row, column) = (211, 40);
    let mut laid = vec![Beam::DRY; layer.cells() + 1 - column];
    let stretch = Stretch {
        tile: &tile,
        layer: &layer,
        row,
        column,
    };
    lay(
        scene,
        (sheet, toward),
        stretch,
        &mut laid,
        &mut Sweep::new(),
    );
    let object = &scene.objects[sheet.object];
    let waves = scene.water(sheet.object).expect("water").waves;
    let (corner, step) = (tile.corner(), cell(FINEST));
    let afresh = (0..laid.len())
        .map(|index| {
            let at = (
                corner.0 + step * real(column + index),
                corner.1 + step * real(row),
            );
            surface(scene, (object, sheet.top), at).map_or(Beam::DRY, |(point, smooth)| {
                let p = object.texture.point_to_local(point);
                let normal = waves.tilt(smooth, p, cutoff(FINEST)).normal;
                beam(sheet, toward, point, normal)
            })
        })
        .collect();
    (laid, afresh)
}

/// A beam's every value, bit for bit.
fn bits(beam: &Beam) -> [u32; 7] {
    let [[sunk_x, sunk_z], [cast_x, cast_z]] = beam.drift;
    let [sunk, cast] = beam.flux;
    [beam.height, sunk_x, sunk_z, cast_x, cast_z, sunk, cast].map(f32::to_bits)
}

/// Swept along a row, level waves turned to any heading lay the beams that
/// reading the waves afresh at each point would.
#[test]
fn beams_swept_along_a_row_are_those_read_afresh() {
    let turned = Pose::new(Vec3::ZERO, Frame::turned(0.7, 0.0));
    assert!(lies_level(&turned), "turned about the vertical alone");
    let (laid, afresh) = laid_and_afresh(&framed(turned.frame));
    assert!(laid.iter().any(|beam| !beam.height.is_nan()));
    let close = |a: f32, b: f32| (a.is_nan() && b.is_nan()) || (a - b).abs() <= 1e-5;
    for (a, b) in laid.iter().zip(&afresh) {
        let (a, b) = (bits(a).map(f32::from_bits), bits(b).map(f32::from_bits));
        assert!(
            a.iter().zip(&b).all(|(&a, &b)| close(a, b)),
            "{a:?} against {b:?}"
        );
    }
}

/// Under waves framed off level, whose crests a row's rise and fall would
/// move unevenly, every beam is read afresh.
#[test]
fn beams_under_waves_framed_off_level_are_read_afresh() {
    let tilted = Frame::turned(0.7, 0.2);
    assert!(!lies_level(&Pose::new(Vec3::ZERO, tilted)));
    let (laid, afresh) = laid_and_afresh(&framed(tilted));
    assert!(laid.iter().any(|beam| !beam.height.is_nan()));
    assert!(laid.iter().zip(&afresh).all(|(a, b)| bits(a) == bits(b)));
}

/// However the sealing is shared among cores, every block at a pyramid's foot
/// bounds the nine beams about it, and every node above the four below it.
#[test]
fn every_pyramid_bounds_the_beams_beneath_it() {
    let scene = pool(0.004);
    let caustics = focused(&scene, (128, 128), &Threaded::new(4));
    assert!(
        caustics
            .layers
            .iter()
            .any(|layer| (layer.cells() / 2).div_ceil(foot_rows(layer)) > 1),
        "a level whose foot is shared among jobs"
    );
    let same = |a: &Bounds, b: &Bounds| {
        a.height.map(f32::to_bits) == b.height.map(f32::to_bits)
            && a.drift.map(|way| way.map(|axis| axis.map(f32::to_bits)))
                == b.drift.map(|way| way.map(|axis| axis.map(f32::to_bits)))
    };
    for layer in &caustics.layers {
        let own = &caustics.bounds[layer.bounds..layer.bounds + nodes(layer.cells())];
        let side = layer.cells() + 1;
        let mut blocks = layer.cells() / 2;
        for row in 0..blocks {
            for column in 0..blocks {
                let expected = (0..9).fold(Bounds::EMPTY, |bounds, vertex| {
                    let (c, r) = (2 * column + vertex % 3, 2 * row + vertex / 3);
                    bounds.with(&caustics.beams[layer.beams + r * side + c])
                });
                assert!(
                    same(&own[row * blocks + column], &expected),
                    "level {}",
                    layer.level
                );
            }
        }
        let (mut below, mut start) = (0, blocks * blocks);
        while blocks > 1 {
            let above = blocks / 2;
            for row in 0..above {
                for column in 0..above {
                    let expected = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().fold(
                        Bounds::EMPTY,
                        |bounds, &(dc, dr)| {
                            bounds.join(&own[below + (2 * row + dr) * blocks + 2 * column + dc])
                        },
                    );
                    assert!(same(&own[start + row * above + column], &expected));
                }
            }
            below = start;
            start += above * above;
            blocks = above;
        }
    }
}

/// A water grid's steady fall shifts every beam alike, by its size, and only
/// how its slope varies about that fall spreads them.
#[test]
fn a_grids_steady_fall_is_told_apart_from_its_spread() {
    let grid = |height: &dyn Fn(f64, f64) -> f64| {
        let mut field = Heightfield::new(64, (0.0, 0.0), 0.25, false).expect("a grid");
        let (((origin_x, origin_z), step), side) = (field.placing(), field.side());
        for (start, band) in field.bands(0..side, 3) {
            for (offset, row) in band.chunks_mut(side).enumerate() {
                let z = origin_z + step * real(start + offset);
                for (column, cell) in row.iter_mut().enumerate() {
                    *cell = single(height(origin_x + step * real(column), z));
                }
            }
        }
        field.seal();
        field
    };
    let falling = grid(&|x, z| 0.02 * x + 0.01 * z);
    let (tilt, variance) = slope_spread(&falling);
    assert!(
        (tilt - mathf::sqrt(0.02 * 0.02 + 0.01 * 0.01)).abs() < 1e-6,
        "{tilt}"
    );
    assert!(variance < 1e-9, "{variance}");
    // Ripples on the same fall still count: a sine of slope amplitude 0.2,
    // read across a quarter-metre step, varies by about a hundredth.
    let rippled = grid(&|x, z| 0.02 * x + 0.05 * mathf::sin(4.0 * z));
    let (_, variance) = slope_spread(&rippled);
    assert!(variance > 0.008, "{variance}");
}

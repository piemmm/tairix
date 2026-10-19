//! What the painter puts down, in what order, and what it costs.

use tairix_raster::mesh::{
    Depth, DepthBuffer, DepthPlane, DepthTest, EdgeRecord, MeshVertex, Triangle,
};
use tairix_raster::shape::{Placed, Shape};
use tairix_raster::surface::Surface;
use tairix_raster::view::Perspective;
use tairix_raster::Color;
use tairix_util::mathf;
use tairix_wintersun_net::value::Facing;

use super::{
    cost, draw, figure, figure_occluded, ground, ground_occluded, Brush, Veil, MAX_FIGURE_POINTS,
};
use crate::frame::Lens;
use crate::humanoid;
use crate::mesh::{self, LEVELS};
use crate::pose::Pose;
use crate::reference::Reference;
use crate::rig::{Placement, Stance};
use crate::testing::human;
use crate::tint::Tint;

const SIDE: u32 = 96;

fn placed(scale: f64) -> Placement {
    let rig = human();
    let rigging = humanoid::rigging(&rig).expect("the humanoid rigging");
    let posture = rigging.posture(&Pose::REST).expect("a rest posture");
    let light = Reference::light().expect("a real light");
    let lens = Lens::Oblique {
        scale,
        at: (48.0, 92.0),
    };
    let stance = Stance::new(Facing(0x2000), lens, light).expect("a real stance");
    let mut placement = Placement::new();
    posture
        .place(&stance, &[], &mut placement)
        .expect("it places");
    placement
}

/// The shadow is on the ground and the figure stands on it, so it must be
/// under every part rather than over the nearest one.
#[test]
fn the_shadow_is_painted_under_the_whole_figure() {
    let placement = placed(0.8);
    let mut brush = Brush::new();
    let shadow = Placed {
        x: 48.0,
        y: 92.0,
        turn: 0.0,
        shape: Shape::Superellipse {
            rx: 40.0,
            ry: 40.0,
            square: 0.0,
        },
        color: Color::rgb(0x10, 0x10, 0x10),
        seed: 0,
    };

    let mut over = Surface::new(SIDE, SIDE).expect("a surface");
    draw(&mut over, &[shadow], &placement, &mut brush, Veil::NONE);
    let mut bare = Surface::new(SIDE, SIDE).expect("a surface");
    draw(&mut bare, &[], &placement, &mut brush, Veil::NONE);

    let mut covered = 0;
    for y in 0..SIDE {
        for x in 0..SIDE {
            let (a, b) = (
                bare.get(x, y).expect("a pixel"),
                over.get(x, y).expect("a pixel"),
            );
            if a.a == u8::MAX {
                covered += 1;
                assert_eq!(a, b, "the shadow showed through the figure at {x},{y}");
            }
        }
    }
    assert!(covered > 0, "the figure must cover some pixels at all");
}

/// Nothing is drawn where nothing was placed, so an empty placement is not a
/// cleared surface either.
#[test]
fn an_empty_placement_paints_nothing() {
    let mut surface = Surface::new(SIDE, SIDE).expect("a surface");
    let mut brush = Brush::new();
    draw(&mut surface, &[], &Placement::new(), &mut brush, Veil::NONE);
    assert!(surface.pixels().iter().all(|pixel| pixel.a == 0));
}

/// The budget the shipped figure is held to, so a rig cannot quietly become
/// the frame's cost centre.
#[test]
fn the_shipped_figure_stays_inside_its_outline_budget() {
    let measured = cost(&placed(0.8));
    assert!(
        measured.points <= MAX_FIGURE_POINTS,
        "the humanoid traces {} outline points",
        measured.points
    );
    assert!(measured.points > 0 && measured.fill_area > 0.0);
}

/// The cost is the scan converter's, so it scales with the drawn size the
/// way the filling does: twice the scale is four times the area and the same
/// outline points.
#[test]
fn the_fill_cost_follows_the_drawn_size_and_the_point_count_does_not() {
    let small = cost(&placed(0.5));
    let large = cost(&placed(1.0));
    assert_eq!(small.points, large.points, "points do not depend on size");
    let ratio = large.fill_area / small.fill_area;
    // Every outline point is snapped to the converter's own sub-pixel grid,
    // so the areas agree to that quantisation rather than exactly.
    assert!(
        (ratio - 4.0).abs() < 0.05,
        "doubling the scale multiplied the area by {ratio}"
    );
}

/// Every colour the figure paints with is one of its declared tones at one
/// of the painter's own shading steps — which is what keeps the palette from
/// drifting a shade at a time and what lets a harness check it by equality.
#[test]
fn the_figure_paints_only_in_shades_of_its_own_palette() {
    let tints = human().tints();
    for strip in placed(0.8).strips() {
        let known = Tint::ALL
            .iter()
            .map(|tint| tints.get(*tint))
            .flat_map(|base| (0..LEVELS).map(move |level| mesh::shaded(base, level)))
            .any(|tone| tone == strip.color);
        assert!(known, "{:?} is no shade of a declared tone", strip.color);
    }
}

/// A surface turned toward the light is drawn lighter than the same surface
/// turned away from it, which is the whole of what makes a limb read as
/// round rather than flat.
#[test]
fn a_surface_is_lighter_where_it_faces_the_light() {
    let placement = placed(0.8);
    let mut lightest = 0u32;
    let mut darkest = u32::MAX;
    for strip in placement.strips() {
        let sum = u32::from(strip.color.r) + u32::from(strip.color.g) + u32::from(strip.color.b);
        lightest = lightest.max(sum);
        darkest = darkest.min(sum);
    }
    assert!(
        lightest > darkest,
        "every strip came out at one tone, so nothing is shaded"
    );
}

/// A veil mixes every stroke toward its tone by its amount and keeps each
/// stroke's own alpha; clear air changes nothing.
#[test]
fn a_veil_mixes_every_stroke_toward_its_tone() {
    let stroke = Color::rgba(200, 100, 0, 180);
    assert_eq!(Veil::NONE.over(stroke), stroke);
    let mist = Veil {
        tone: Color::rgb(0, 100, 200),
        amount: 255,
    };
    assert_eq!(mist.over(stroke), Color::rgba(0, 100, 200, 180));
    let half = Veil {
        tone: Color::rgb(0, 100, 200),
        amount: 128,
    };
    let seen = half.over(stroke);
    assert!(seen.r < 200 && seen.r > 0 && seen.b > 0 && seen.b < 200);
    assert_eq!((seen.g, seen.a), (100, 180));

    // Drawn through a veil, a figure's pixels move toward the tone.
    let figure = Reference::new(
        &crate::reference::identity(crate::species::Species::Human).expect("a record"),
    )
    .expect("builds");
    let mut placement = Placement::new();
    let cell = crate::reference::Cell {
        kind: crate::motion::Kind::Idle,
        step: 0,
        facing: Facing(0x4000),
    };
    figure
        .place(cell, 0.5, (32.0, 58.0), &mut placement)
        .expect("places");
    let mut clear = Surface::new(64, 64).expect("a surface");
    let mut veiled = Surface::new(64, 64).expect("a surface");
    let mut brush = Brush::new();
    draw(&mut clear, &[], &placement, &mut brush, Veil::NONE);
    draw(&mut veiled, &[], &placement, &mut brush, mist);
    let toward = |surface: &Surface| {
        surface
            .pixels()
            .iter()
            .filter(|pixel| pixel.a == 255)
            .map(|pixel| u64::from(pixel.b))
            .sum::<u64>()
    };
    assert!(
        toward(&veiled) > toward(&clear),
        "the veil did not reach the figure"
    );
}

/// The reference human at rest on level ground, seen through a perspective
/// camera that frames it in a [`SIDE`]-pixel square.
fn seen(camera: &Perspective) -> Placement {
    let rig = human();
    let rigging = humanoid::rigging(&rig).expect("the humanoid rigging");
    let posture = rigging.posture(&Pose::REST).expect("a rest posture");
    let light = Reference::light().expect("a real light");
    let lens = Lens::Perspective {
        camera,
        ground: [0.0, 0.0, 0.0],
        units: 24.0,
    };
    let stance = Stance::new(Facing(0x2000), lens, light).expect("a real stance");
    let mut placement = Placement::new();
    posture
        .place(&stance, &[], &mut placement)
        .expect("it places");
    placement
}

/// A camera six metres south of the origin and above it, looking at it.
fn framing() -> Perspective {
    let (sin, cos) = (mathf::sin(0.5), mathf::cos(0.5));
    let distance = 6144.0;
    Perspective::new(
        [0.0, distance * cos, distance * sin],
        [0.0, -cos, -sin],
        [1.0, 0.0, 0.0],
        [0.0, sin, -cos],
        150.0,
        (96, 140),
        256.0,
    )
    .expect("a camera")
}

/// Make every pixel of `rows` hold the nearest depth there is.
fn nearest_over(depth: &mut DepthBuffer, rows: u32) {
    let corner = |x: u32, y: u32| MeshVertex {
        x: i32::try_from(x * 256).expect("small"),
        y: i32::try_from(y * 256).expect("small"),
        depth: Depth::NEAREST,
    };
    let height = depth.height();
    for mut band in depth.bands_mut(height) {
        for corners in [
            [corner(0, 0), corner(SIDE, 0), corner(0, rows)],
            [corner(SIDE, 0), corner(SIDE, rows), corner(0, rows)],
        ] {
            Triangle::new(corners).expect("a triangle").fill(
                &mut band,
                DepthTest::OPAQUE,
                EdgeRecord::Skip,
                |_| true,
            );
        }
    }
}

/// In a scene a figure is drawn exactly where nothing nearer stands, and
/// not at all where something does.
#[test]
fn a_figure_in_a_scene_hides_behind_what_is_nearer() {
    let camera = framing();
    let placement = seen(&camera);
    assert!(!placement.is_empty());
    let mut brush = Brush::new();
    let mut alone = Surface::new(SIDE, SIDE).expect("a surface");
    figure(&mut alone, &placement, &mut brush, Veil::NONE);

    let mut scene = DepthBuffer::new();
    assert!(scene.reset(SIDE, SIDE));
    let mut open = Surface::new(SIDE, SIDE).expect("a surface");
    figure_occluded(&mut open, &placement, &scene, &mut brush, Veil::NONE);
    assert_eq!(
        open.pixels(),
        alone.pixels(),
        "an empty scene hid something"
    );

    let half = SIDE / 2;
    nearest_over(&mut scene, half);
    let mut hidden = Surface::new(SIDE, SIDE).expect("a surface");
    figure_occluded(&mut hidden, &placement, &scene, &mut brush, Veil::NONE);
    let (mut shown, mut covered) = (0, 0);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let got = hidden.get(x, y).expect("a pixel");
            if y < half {
                assert_eq!(got.a, 0, "drawn behind something nearer at {x},{y}");
                covered += usize::from(alone.get(x, y).expect("a pixel").a > 0);
            } else {
                assert_eq!(got, alone.get(x, y).expect("a pixel"), "at {x},{y}");
                shown += usize::from(got.a > 0);
            }
        }
    }
    assert!(
        shown > 0 && covered > 0,
        "the figure must straddle the split"
    );
}

/// A shadow in a scene lies on its ground and is hidden by anything nearer.
#[test]
fn a_shadow_in_a_scene_hides_behind_what_is_nearer() {
    let shadow = Placed {
        x: 48.0,
        y: 48.0,
        turn: 0.0,
        shape: Shape::Superellipse {
            rx: 30.0,
            ry: 30.0,
            square: 0.0,
        },
        color: Color::rgb(0x10, 0x10, 0x10),
        seed: 0,
    };
    let mut brush = Brush::new();
    let mut scene = DepthBuffer::new();
    assert!(scene.reset(SIDE, SIDE));
    let plane = DepthPlane::flat(Depth(1000));
    let mut alone = Surface::new(SIDE, SIDE).expect("a surface");
    ground(&mut alone, &[shadow], &mut brush, Veil::NONE);
    nearest_over(&mut scene, SIDE / 2);
    let mut hidden = Surface::new(SIDE, SIDE).expect("a surface");
    ground_occluded(
        &mut hidden,
        &[shadow],
        plane,
        &scene,
        &mut brush,
        Veil::NONE,
    );
    for y in 0..SIDE {
        for x in 0..SIDE {
            let got = hidden.get(x, y).expect("a pixel");
            if y < SIDE / 2 {
                assert_eq!(got.a, 0, "a shadow showed through at {x},{y}");
            } else {
                assert_eq!(got, alone.get(x, y).expect("a pixel"), "at {x},{y}");
            }
        }
    }
}

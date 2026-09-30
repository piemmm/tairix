//! Host tests of the wireframe renderer: hulls built from their corners, the
//! camera's projection, culling and clipping, and the display replayed.

use alloc::vec::Vec;

use tairix_parallel::Reversed;
use tairix_raster::ScanScratch;
use tairix_util::space::{Frame, Pose, Vec3};
use tairix_wm::{Rect, Scale, Surface};

use super::{
    clip_segment, glow_strength, guard, guard_polygon, Camera, Display, Hull, HullId, Ink, Models,
    Part, Stage, NEAR,
};
use crate::saver::retro_games::{draw_craft, Moment, Rgb, View, CAMERA};

const SCREEN: (u32, u32) = (480, 270);

static INK: Ink = Ink {
    edge: Rgb::new(240.0, 200.0, 120.0),
    face: Rgb::new(10.0, 10.0, 20.0),
};

fn view() -> View {
    View::new(SCREEN, Scale::ONE).expect("a view")
}

fn cube(half: f64) -> Vec<Vec3> {
    let mut corners = Vec::new();
    for x in [-half, half] {
        for y in [-half, half] {
            for z in [-half, half] {
                corners.push(Vec3::new(x, y, z));
            }
        }
    }
    corners
}

/// Every face of `hull` is flat, turned outwards, and has every corner on or
/// behind it; every edge joins two faces; and it has as many corners, edges
/// and faces as a closed convex solid must.
fn assert_sound(hull: &Hull) {
    let used: Vec<usize> = (0..hull.points.len())
        .filter(|at| hull.rings.iter().any(|&corner| usize::from(corner) == *at))
        .collect();
    for (at, face) in hull.faces.iter().enumerate() {
        let ring = hull.ring(face);
        assert!(ring.len() >= 3, "face {at} has {} corners", ring.len());
        let on = hull.points[usize::from(ring[0])];
        for &corner in ring {
            let apart = face.normal.dot(hull.points[usize::from(corner)] - on);
            assert!(apart.abs() < 1e-6, "face {at} is flat");
        }
        for point in &hull.points {
            assert!(
                face.normal.dot(*point - on) < 1e-6,
                "face {at} bounds the hull"
            );
        }
        assert!(
            face.normal.dot(hull.centre - on) < 0.0,
            "face {at} is turned outwards"
        );
        let (a, b, c) = (
            hull.points[usize::from(ring[0])],
            hull.points[usize::from(ring[1])],
            hull.points[usize::from(ring[2])],
        );
        assert!(
            (b - a).cross(c - a).dot(face.normal) > 0.0,
            "face {at} winds anticlockwise"
        );
    }
    let (v, e, f) = (used.len(), hull.edges.len(), hull.faces.len());
    assert_eq!(v + f, e + 2, "Euler: {v} corners, {e} edges, {f} faces");
}

#[test]
fn a_cube_is_six_squares_and_twelve_edges() {
    let hull = Hull::of(&cube(0.5), &[]).expect("a hull");
    assert_eq!(hull.faces.len(), 6);
    assert_eq!(hull.edges.len(), 12);
    for face in &hull.faces {
        assert_eq!(hull.ring(face).len(), 4);
        let axes = [face.normal.x, face.normal.y, face.normal.z];
        assert_eq!(
            axes.iter()
                .filter(|axis| (axis.abs() - 1.0).abs() < 1e-9)
                .count(),
            1
        );
    }
    assert_sound(&hull);
}

/// A corner inside the hull, or inside a face or an edge of it, is no corner
/// of any face.
#[test]
fn points_within_a_hull_are_no_corners_of_it() {
    let mut points = cube(1.0);
    points.extend([
        Vec3::ZERO,
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.2, -0.4, 0.9),
    ]);
    let hull = Hull::of(&points, &[]).expect("a hull");
    assert_eq!(hull.faces.len(), 6);
    assert!(hull.faces.iter().all(|face| hull.ring(face).len() == 4));
    assert_sound(&hull);
}

#[test]
fn corners_enclosing_nothing_make_no_hull() {
    let flat: Vec<Vec3> = (0..6)
        .map(|at| Vec3::new(f64::from(at), f64::from(at * at % 5), 0.0))
        .collect();
    assert!(Hull::of(&flat, &[]).is_none(), "all in one plane");
    assert!(Hull::of(&cube(1.0)[..3], &[]).is_none(), "too few");
    let crowd: Vec<Vec3> = (0..60)
        .map(|at| Vec3::new(f64::from(at), f64::from(at % 7), f64::from(at % 3)))
        .collect();
    assert!(Hull::of(&crowd, &[]).is_none(), "too many");
}

/// A line drawn on a face is seen with it; one lying on none is always seen.
#[test]
fn a_line_belongs_to_the_face_it_lies_on() {
    let on_top = [Vec3::new(-0.3, 0.5, 0.0), Vec3::new(0.3, 0.5, 0.2)];
    let loose = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 2.0, 0.0)];
    let hull = Hull::of(&cube(0.5), &[on_top, loose]).expect("a hull");
    let top = hull.lines[0].face.expect("on a face");
    assert!(hull.faces[usize::from(top)].normal.y > 0.99, "the top face");
    assert_eq!(hull.lines[1].face, None);
}

/// Every solid the craft are built from is a sound hull.
#[test]
fn every_craft_solid_is_a_closed_convex_hull() {
    let models = Models::new().expect("the solids");
    for id in HullId::ALL {
        let hull = models.get(id).expect("built");
        assert_sound(hull);
        assert!(hull.faces.len() >= 5, "{id:?}");
    }
}

/// Everything that rolls on the ground stands lower than the camera, so it
/// never rises above the horizon and never needs the sky laid back.
#[test]
fn nothing_on_the_ground_stands_as_tall_as_the_camera() {
    let models = Models::new().expect("the solids");
    for id in [
        HullId::TankBody,
        HullId::TankTurret,
        HullId::TankBarrel,
        HullId::BikeBody,
        HullId::BikeRider,
    ] {
        let hull = models.get(id).expect("built");
        assert!(
            hull.points.iter().all(|point| point.y < CAMERA * 0.7),
            "{id:?}"
        );
    }
}

/// The camera projects the floor where the floor draws it: a point on the
/// floor `depth` cells ahead lands `focal / depth` rows below the horizon.
#[test]
fn the_camera_projects_the_floor_where_the_floor_lies() {
    let view = view();
    let moment = Moment::at(12.0, 0.8);
    let camera = Camera::at(&view, moment);
    for depth in [1.5, 4.0, 30.0] {
        let world = Vec3::new(moment.sway + 2.0, 0.0, moment.flown + depth);
        let (x, y) = camera.project(camera.relative(world));
        assert!((y - (f64::from(view.horizon) + view.depth(depth))).abs() < 1e-9);
        assert!((x - (view.centre + 2.0 * view.focal / depth)).abs() < 1e-9);
    }
    assert!(camera.sees(Vec3::new(moment.sway, 0.0, moment.flown + 5.0), 0.0));
    assert!(
        !camera.sees(Vec3::new(moment.sway, 0.0, moment.flown - 1.0), 0.0),
        "behind"
    );
    assert!(
        !camera.sees(Vec3::new(moment.sway + 50.0, 0.0, moment.flown + 5.0), 0.0),
        "aside"
    );
}

#[test]
fn a_line_behind_the_camera_is_cut_at_its_near_plane() {
    let (ahead, behind) = (Vec3::new(1.0, 0.0, 4.0), Vec3::new(-1.0, 0.0, -2.0));
    let [from, to] = clip_segment([ahead, behind]).expect("part is ahead");
    assert_eq!(from, ahead);
    assert!((to.z - NEAR).abs() < 1e-12);
    let [from, _] = clip_segment([behind, ahead]).expect("part is ahead");
    assert!((from.z - NEAR).abs() < 1e-12);
    assert!(clip_segment([behind, behind * 2.0]).is_none());
}

/// However far off the screen a polygon reaches, what is kept of it lies
/// within the guarded screen, and one wholly within is kept whole.
#[test]
fn a_polygon_is_kept_to_the_guarded_screen() {
    let view = view();
    let [left, top, right, bottom] = guard(&view);
    let huge = [(-1e7, -3e6), (5e6, 40.0), (100.0, 9e6)];
    let kept = guard_polygon(&huge, &view);
    assert!(kept.len() >= 3);
    for &(x, y) in &kept {
        assert!(
            (left - 1e-6..=right + 1e-6).contains(&x) && (top - 1e-6..=bottom + 1e-6).contains(&y)
        );
    }
    let small = [(10.0, 10.0), (50.0, 12.0), (30.0, 60.0)];
    assert_eq!(guard_polygon(&small, &view).as_slice(), &small[..]);
}

/// A cube seen square on shows one face, drawn dark, and the four edges about
/// it, each a glow and a core; turned towards the camera it shows two, and
/// the seven edges beside them.
#[test]
fn only_what_faces_the_camera_is_drawn() {
    let view = view();
    let camera = Camera::at(&view, Moment::default());
    let hull = Hull::of(&cube(0.5), &[]).expect("a hull");
    let mut display = Display::new(&view, 64).expect("a display");
    let square = Pose::new(camera.position() + Vec3::new(0.0, 0.0, 6.0), Frame::WORLD);
    display.solid(&hull, &square, &INK, 1.0, &camera);
    assert_eq!(display.shapes.len(), 1 + 4 * 2);
    display.clear();
    let turned = Pose::new(
        camera.position() + Vec3::new(0.0, 0.0, 6.0),
        Frame::turned(core::f64::consts::FRAC_PI_4, 0.0),
    );
    display.solid(&hull, &turned, &INK, 1.0, &camera);
    assert_eq!(display.shapes.len(), 2 + 7 * 2);
}

/// A thing is ordered by how far off it stands, the farthest drawn first,
/// whatever order the acts set them out in; and the box it reaches above the
/// horizon is noted.
#[test]
fn things_are_drawn_far_to_near() {
    let view = view();
    let models = Models::new().expect("the solids");
    let mut stage = Stage::new(&view, 16).expect("a stage");
    let camera = Camera::at(&view, Moment::default());
    stage.reset(camera);
    let at = |z: f64| camera.position() + Vec3::new(0.0, 0.5, z);
    for depth in [3.0, 9.0, 5.0] {
        stage.begin(at(depth));
        stage.push(Part::Glow {
            base: at(depth),
            tip: at(depth),
            radius: 0.3,
            light: Rgb::new(255.0, 255.0, 255.0),
            alpha: 1.0,
        });
    }
    let mut display = Display::new(&view, 16).expect("a display");
    let mut sky = Vec::new();
    stage.draw(&models, &mut display, &mut sky);
    let bases: Vec<f64> = display
        .shapes
        .iter()
        .map(|shape| match shape.fill {
            super::Fill::Glow { radius, .. } => radius,
            super::Fill::Flat => 0.0,
        })
        .collect();
    assert_eq!(bases.len(), 3);
    assert!(
        bases[0] < bases[1] && bases[1] < bases[2],
        "the nearer is wider and later: {bases:?}"
    );
    assert_eq!(
        sky.len(),
        1,
        "glows this near one another are laid back as one"
    );
    let horizon = i32::try_from(view.horizon).expect("small");
    assert!(sky[0].bottom() <= horizon, "above the horizon alone");
    for shape in &display.shapes {
        let top = i32::try_from(shape.rows.0).expect("small");
        assert!(top >= sky[0].top(), "the box holds every glow");
    }
}

/// Boxes a frame's craft reach above the horizon are gathered with those they
/// come near, and those far apart kept apart.
#[test]
fn boxes_near_one_another_are_gathered() {
    let mut sky = Vec::new();
    super::gather(&mut sky, Rect::new(10, 10, 20, 20));
    super::gather(&mut sky, Rect::new(200, 10, 20, 20));
    assert_eq!(sky.len(), 2, "far apart");
    super::gather(&mut sky, Rect::new(40, 15, 10, 10));
    assert_eq!(sky.len(), 2, "near the first");
    assert!(sky.contains(&Rect::new(10, 10, 40, 20)));
    super::gather(&mut sky, Rect::new(60, 10, 130, 5));
    assert_eq!(
        sky,
        [Rect::new(10, 10, 210, 20)],
        "a bridge gathers all three"
    );
    super::gather(&mut sky, Rect::EMPTY);
    assert_eq!(sky.len(), 1, "nothing adds nothing");
}

/// A display replayed a band at a time on any number of cores draws what it
/// draws replayed whole, and nothing outside the boxes its things reach.
#[test]
fn a_display_replayed_in_bands_draws_what_it_draws_whole() {
    let view = view();
    let models = Models::new().expect("the solids");
    let mut stage = Stage::new(&view, 64).expect("a stage");
    let camera = Camera::at(&view, Moment::default());
    stage.reset(camera);
    for (hull, x, y, z) in [
        (HullId::ShipFuselage, -1.0, 0.6, 7.0),
        (HullId::TankBody, 1.0, -1.0, 2.5),
        (HullId::SaucerLens, 0.5, 1.8, 4.0),
    ] {
        let pose = Pose::new(
            camera.position() + Vec3::new(x, y, z),
            Frame::turned(0.7, 0.2),
        );
        stage.begin(pose.at);
        stage.push(Part::Solid {
            hull,
            pose,
            ink: &INK,
            alpha: 1.0,
        });
        stage.push(Part::Glow {
            base: pose.at,
            tip: pose.at - pose.frame.z * 1.2,
            radius: 0.2,
            light: Rgb::new(90.0, 160.0, 255.0),
            alpha: 0.7,
        });
    }
    let mut display = Display::new(&view, 256).expect("a display");
    let mut sky = Vec::new();
    stage.draw(&models, &mut display, &mut sky);
    let mut one = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    display.replay(&mut one, &(0..SCREEN.1), &mut ScanScratch::new());
    let mut banded = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    let mut scratch: Vec<ScanScratch> = (0..8).map(|_| ScanScratch::new()).collect();
    let runner = Reversed::new(8);
    draw_craft(&mut banded, &runner, &display, &mut scratch);
    assert_eq!(banded, one);
    assert!(runner.widest() > 1, "the work was split");
    assert!(
        one.pixels().iter().any(|pixel| pixel.a > 0),
        "something was drawn"
    );
    let rows = display.rows();
    for y in 0..SCREEN.1 {
        for x in 0..SCREEN.0 {
            if one.get(x, y).is_some_and(|pixel| pixel.a > 0) {
                assert!(rows.contains(&y), "({x}, {y}) lies in the display's rows");
            }
        }
    }
}

/// The box a thing reaches holds every pixel it draws.
#[test]
fn a_thing_draws_nothing_outside_the_box_it_reaches() {
    let view = view();
    let models = Models::new().expect("the solids");
    let camera = Camera::at(&view, Moment::default());
    let pose = Pose::new(
        camera.position() + Vec3::new(0.4, 0.9, 6.0),
        Frame::turned(2.1, -0.3),
    );
    for part in [
        Part::Solid {
            hull: HullId::SaucerDome,
            pose,
            ink: &INK,
            alpha: 1.0,
        },
        Part::Ring {
            centre: pose.at,
            frame: pose.frame,
            radius: 1.3,
            sides: 12,
            ink: &INK,
            alpha: 1.0,
        },
        Part::Glow {
            base: pose.at,
            tip: pose.at + Vec3::new(2.0, -0.5, 1.0),
            radius: 0.4,
            light: Rgb::new(255.0, 90.0, 40.0),
            alpha: 1.0,
        },
    ] {
        let mut display = Display::new(&view, 128).expect("a display");
        display.part(&part, &models, &camera);
        let reach = display.reach();
        let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
        display.replay(&mut surface, &(0..SCREEN.1), &mut ScanScratch::new());
        let mut drew = false;
        for y in 0..SCREEN.1 {
            for x in 0..SCREEN.0 {
                if surface.get(x, y).is_some_and(|pixel| pixel.a > 0) {
                    drew = true;
                    let inside = Rect::new(
                        i32::try_from(x).expect("small"),
                        i32::try_from(y).expect("small"),
                        1,
                        1,
                    );
                    assert!(
                        !reach.intersection(&inside).is_empty(),
                        "{part:?} at ({x}, {y})"
                    );
                }
            }
        }
        assert!(drew, "{part:?} drew");
    }
}

/// A glow is strongest at its base and gone at its reach; drawn out, it fades
/// towards its tip; and as the camera comes to look along it the flame
/// shrinks smoothly into its round glow.
#[test]
fn a_glow_fades_out_from_its_base() {
    let base = (100.0, 100.0);
    assert_eq!(glow_strength(base, base, (1.0, 0.0), 0.0, 10.0), 255);
    assert_eq!(
        glow_strength((111.0, 100.0), base, (1.0, 0.0), 0.0, 10.0),
        0
    );
    let along: Vec<u8> = (0..40)
        .map(|at| glow_strength((100.0 + f64::from(at), 100.0), base, (1.0, 0.0), 40.0, 10.0))
        .collect();
    assert!(along.windows(2).all(|pair| pair[0] >= pair[1]), "{along:?}");
    assert!(along[30] > 0, "a flame reaches past its disc");
    assert_eq!(
        glow_strength((141.0, 100.0), base, (1.0, 0.0), 40.0, 10.0),
        0
    );
    let point = (104.0, 103.0);
    let round = glow_strength(point, base, (1.0, 0.0), 0.0, 10.0);
    let short = glow_strength(point, base, (1.0, 0.0), 0.5, 10.0);
    assert!(short.abs_diff(round) <= 2, "{short} against {round}");
}

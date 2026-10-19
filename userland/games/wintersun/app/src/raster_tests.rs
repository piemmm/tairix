//! The terrain pass draws what the camera sees: nearer ground over farther,
//! the ground the eye stands over clipped rather than dropped, and the same
//! picture however the rows are cut.
#![allow(
    clippy::float_arithmetic,
    reason = "cameras and probes are placed through the perspective, which \
              takes reals"
)]

use alloc::vec::Vec;

use tairix_parallel::SERIAL;
use tairix_raster::mesh::Depth;
use tairix_raster::view::Perspective;
use tairix_wintersun_art::splat::Materials;
use tairix_wintersun_net::value::ChunkCoord;
use tairix_wintersun_world::geom::CELL_SUB_UNITS;
use tairix_wintersun_world::ground::Ground;

use super::{draw, Carried, Drawing, GBuffer, Pass};
use crate::mesh::TerrainMesh;
use crate::testing::mesh;

const WIDTH: u32 = 96;
const HEIGHT: u32 = 64;

/// A camera at `eye`, looking north and down by the angle whose cosine and
/// sine are `tilt`.
fn camera(eye: [f64; 3], tilt: (f64, f64)) -> Perspective {
    let (level, down) = tilt;
    Perspective::new(
        eye,
        [0.0, -level, -down],
        [1.0, 0.0, 0.0],
        [0.0, down, -level],
        60.0,
        (WIDTH, HEIGHT),
        256.0,
    )
    .expect("a camera")
}

/// `meshes` drawn through `camera` in bands of `rows`, a skirt hung from
/// every edge.
fn drawn(meshes: &[&TerrainMesh], camera: &Perspective, rows: u32) -> GBuffer {
    hung(meshes, [true; 4], camera, rows)
}

/// `meshes` drawn through `camera` in bands of `rows`, a skirt hung from each
/// edge `skirts` names.
fn hung(meshes: &[&TerrainMesh], skirts: [bool; 4], camera: &Perspective, rows: u32) -> GBuffer {
    let mut carried: Vec<Carried> = meshes.iter().map(|_| Carried::default()).collect();
    for (mesh, held) in meshes.iter().zip(carried.iter_mut()) {
        held.carry(mesh, skirts, camera).expect("room");
    }
    let drawings: Vec<Drawing<'_>> = meshes
        .iter()
        .zip(&carried)
        .map(|(mesh, carried)| Drawing {
            mesh,
            carried,
            skirts,
        })
        .collect();
    let materials = Materials::flat();
    let pass = Pass {
        camera,
        materials: &materials,
    };
    let mut buffer = GBuffer::new();
    buffer.reset(WIDTH, HEIGHT).expect("room");
    draw(&mut buffer, &drawings, &pass, rows, &SERIAL).expect("room");
    buffer
}

/// Every pixel's depth.
fn depths(buffer: &GBuffer) -> Vec<Depth> {
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| buffer.at(x, y).expect("inside").0)
        .collect()
}

/// A chunk's middle, in world sub-units.
fn middle(coord: ChunkCoord) -> [f64; 3] {
    let side = f64::from(crate::terrain::CHUNK_SUB_UNITS);
    let half = f64::from(CELL_SUB_UNITS) * 0.5 + side * 0.5;
    [
        f64::from(coord.x) * side + half,
        f64::from(coord.y) * side + half,
        0.0,
    ]
}

#[test]
fn nothing_drawn_leaves_every_pixel_clear() {
    let view = camera([0.0, 0.0, 10_000.0], (0.0, 1.0));
    let buffer = drawn(&[], &view, HEIGHT);
    assert!(depths(&buffer).iter().all(|depth| *depth == Depth::CLEAR));
}

#[test]
fn ground_below_the_eye_covers_the_screen_it_fills() {
    let coord = ChunkCoord { x: 0, y: 0 };
    let ground = mesh(coord, 4, Ground::Meadow, |_, _| 0);
    let [x, y, _] = middle(coord);
    // Straight down from close enough that the chunk fills the view.
    let view = camera([x, y, 20_000.0], (0.0, 1.0));
    let buffer = drawn(&[&ground], &view, HEIGHT);
    let seen = depths(&buffer);
    assert!(
        seen.iter().all(|depth| *depth != Depth::CLEAR),
        "a hole in the ground"
    );
    // Flat ground seen straight down is one distance away everywhere it is
    // under the screen's middle.
    let centre = buffer.at(WIDTH / 2, HEIGHT / 2).expect("inside").0;
    let expected = view.depth(20_000.0);
    assert!((f64::from(centre.0) - expected).abs() < expected * 1e-3);
    let (_, albedo, texel, _) = buffer.at(3, 3).expect("inside");
    assert_eq!(albedo.a, u8::MAX, "the ground is opaque");
    assert_eq!(texel.elevation, 0);
}

/// A hill hides the ground behind it: a probe beyond a ridge draws the ridge.
#[test]
fn a_ridge_hides_the_ground_behind_it() {
    let (near, far) = (ChunkCoord { x: 0, y: 1 }, ChunkCoord { x: 0, y: 0 });
    let ridge = mesh(near, 4, Ground::Granite, |_, j| {
        // Rising to a crest across the chunk's middle row.
        let from_crest = (i32::try_from(j).expect("small") - 8).abs();
        (8 - from_crest) * 4 * CELL_SUB_UNITS
    });
    let beyond = mesh(far, 4, Ground::Meadow, |_, _| 0);
    let [x, y, _] = middle(near);
    // South of the ridge, low, looking north across it.
    let view = camera([x, y + 80_000.0, 6_000.0], (0.996_194_698, 0.087_155_743));
    let both = drawn(&[&ridge, &beyond], &view, HEIGHT);
    let alone = drawn(&[&beyond], &view, HEIGHT);
    let mut probes = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (behind, _, _, _) = alone.at(x, y).expect("inside");
            let (seen, _, texel, _) = both.at(x, y).expect("inside");
            if behind == Depth::CLEAR || seen == behind {
                continue;
            }
            // Where the ridge stands in front of the ground, the ridge is drawn.
            assert!(
                seen > behind,
                "farther ground drawn over the ridge at {x},{y}"
            );
            assert!(texel.elevation > 0, "the probe did not land on the ridge");
            probes += 1;
        }
    }
    assert!(probes > 0, "the ridge hid no ground at all");
}

/// Ground passing under the eye reaches nearer than the near plane, and is
/// clipped there rather than dropped: the screen's foot is still ground.
#[test]
fn ground_through_the_near_plane_is_clipped_not_dropped() {
    let coord = ChunkCoord { x: 0, y: 0 };
    let ground = mesh(coord, 16, Ground::Meadow, |_, _| 0);
    let [x, y, _] = middle(coord);
    // Just above the ground, looking along it.
    let view = camera([x, y, 300.0], (0.996_194_698, 0.087_155_743));
    let buffer = drawn(&[&ground], &view, HEIGHT);
    for x in 0..WIDTH {
        let (depth, _, _, _) = buffer.at(x, HEIGHT - 1).expect("inside");
        assert_ne!(
            depth,
            Depth::CLEAR,
            "a hole at the screen's foot, column {x}"
        );
    }
}

#[test]
fn the_picture_does_not_depend_on_how_the_rows_are_cut() {
    let coord = ChunkCoord { x: 0, y: 0 };
    let ground = mesh(coord, 2, Ground::Meadow, |i, j| {
        i32::try_from((i * 37 + j * 91) % 400).expect("small") * 64
    });
    let [x, y, _] = middle(coord);
    let view = camera([x, y + 30_000.0, 20_000.0], (0.8, 0.6));
    let whole = drawn(&[&ground], &view, HEIGHT);
    for rows in [1, 7, 13] {
        let cut = drawn(&[&ground], &view, rows);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                assert_eq!(
                    whole.at(x, y),
                    cut.at(x, y),
                    "{rows} rows a band, at {x},{y}"
                );
            }
        }
    }
}

/// A skirt reaches the rows its edge's own corners stand at as well as those
/// its hung corners do, so a band holding only its upper part draws it.
#[test]
fn a_skirt_is_drawn_in_every_band_it_reaches() {
    let coord = ChunkCoord { x: 0, y: 0 };
    let ground = mesh(coord, 8, Ground::Meadow, |_, _| 0);
    let [x, y, _] = middle(coord);
    let cell = f64::from(CELL_SUB_UNITS);
    // Thirty cells south of the chunk's south edge and six above it, so the
    // skirt that edge hangs faces the eye across a dozen rows. Only that edge
    // hangs one, so no other skirt's corners reach the rows its own do.
    let view = camera([x, y + 62.0 * cell, 6.0 * cell], (0.96, 0.28));
    let south = [false, true, false, false];
    let whole = hung(&[&ground], south, &view, HEIGHT);
    assert!(
        (28..40).any(|row| whole
            .at(WIDTH / 2, row)
            .is_some_and(|at| at.0 != Depth::CLEAR)),
        "the skirt is not on screen above its hung corners"
    );
    let cut = hung(&[&ground], south, &view, 1);
    assert_eq!(
        depths(&cut),
        depths(&whole),
        "a band held part of the skirt and drew none"
    );
}

/// Gentle ground reads the ground plane, a face reads the vertical plane it
/// faces, and about forty-five degrees the two share the dither.
#[test]
fn a_steep_face_reads_the_plane_it_faces() {
    use super::Reading;
    let every = |normal: [i64; 2]| (0..16).map(move |dither| Reading::of(normal, dither));
    assert!(every([0, 0]).all(|reading| reading == Reading::Ground));
    assert!(
        every([60, -20]).all(|reading| reading == Reading::Ground),
        "a slope under thirty degrees"
    );
    assert!(
        every([126, 5]).all(|reading| reading == Reading::Meridian),
        "a face toward the east"
    );
    assert!(
        every([-8, -125]).all(|reading| reading == Reading::Parallel),
        "a face toward the north"
    );
    // Forty-five degrees toward the east: the up and east components match.
    let shared: Vec<Reading> = every([90, 0]).collect();
    assert!(shared.contains(&Reading::Ground) && shared.contains(&Reading::Meridian));
}

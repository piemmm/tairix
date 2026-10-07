//! Host tests of cracked mud: its plates curl up at their rims over an
//! undercut wall, part along cracks as wide as the crust is thick, and any
//! two tiles meet edge to edge in whole plates without a seam.

use super::*;
use crate::heightfield::{Grows, QUANTITIES};
use crate::prototype::Prototype;
use crate::vector::{share, Ray};

const CRUST: Crust = Crust {
    side: 3.0,
    plates: 16,
    thick: 0.015,
};

fn grown(crust: Crust, variant: u32) -> Prototype {
    tile(crust, 1, (0x5eed, variant)).expect("a tile").whole()
}

/// Whether a ray straight down at `(x, z)` meets `tile` placed `offset`
/// along x.
fn covers(tile: &Prototype, (x, z): (f64, f64), offset: f64) -> bool {
    tile.intersect(
        &Ray::new(Vec3::new(x - offset, 1.0, z), -Vec3::UP),
        (1e-9, 2.0),
        None,
    )
    .is_some()
}

/// The share of a strip `across` either side of `x` that lies open, a crack
/// between plates, on `tile`.
fn open(tile: &Prototype, x: f64, across: f64) -> f64 {
    let half = 0.5 * CRUST.side;
    let (mut gaps, mut samples) = (0, 0);
    for step in 0..800u32 {
        let z = -half + CRUST.side * (f64::from(step) + 0.5) / 800.0;
        for side in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            gaps += usize::from(!covers(tile, (x + side * across, z), 0.0));
            samples += 1;
        }
    }
    share(gaps, samples)
}

#[test]
fn two_tiles_meet_in_whole_plates_without_a_seam() {
    let (left, right) = (grown(CRUST, 0), grown(CRUST, 1));
    let half = 0.5 * CRUST.side;
    let (mut gaps, mut samples) = (0, 0);
    for step in 0..800u32 {
        let z = -half + CRUST.side * (f64::from(step) + 0.5) / 800.0;
        for across in [-0.04, -0.02, 0.0, 0.02, 0.04] {
            let x = half + across;
            let (a, b) = (
                covers(&left, (x, z), 0.0),
                covers(&right, (x, z), CRUST.side),
            );
            assert!(!(a && b), "both tiles lay a plate at {x} {z}");
            gaps += usize::from(!a && !b);
            samples += 1;
        }
    }
    let (seam, within) = (share(gaps, samples), open(&left, 0.0, 0.04));
    assert!(within > 0.0, "cracks part the plates");
    assert!(
        seam < 1.6 * within + 0.01,
        "the seam opens no wider than a crack: {seam} beside {within}"
    );
}

#[test]
fn every_tile_shares_its_edge_points_and_differs_within() {
    let lattice = |variant| Lattice {
        plates: 16,
        cell: 3.0 / 16.0,
        side: 3.0,
        seed: 9,
        variant,
    };
    let (a, b) = (lattice(0), lattice(1));
    for row in 0..16 {
        for column in 0..16 {
            let banded = column < 2 || row < 2 || column >= 14 || row >= 14;
            assert_eq!(
                a.point((column, row)) == b.point((column, row)),
                banded,
                "{column} {row}"
            );
        }
    }
    // A point a tile over is its own image a side along.
    let ((p, id), (q, other)) = (a.point((-1, 3)), a.point((15, 3)));
    assert_eq!(id, other);
    assert!((p.0 + 3.0 - q.0).abs() < 1e-12 && (p.1 - q.1).abs() < 1e-12);
}

#[test]
fn a_plate_curls_up_at_its_rim_over_an_undercut_wall() {
    let lattice = Lattice {
        plates: 16,
        cell: 0.2,
        side: 3.2,
        seed: 4,
        variant: 0,
    };
    let square = Polygon::square((1.0, 1.0), 0.1);
    let mut assembly = Assembly::default();
    lay(&mut assembly, (&lattice, &square, 1.0), (0.015, 1, 7)).expect("laid");
    let plate = assembly.finish().expect("a plate").whole();
    // Its middle, then its rings out to its rim, then its foot, each a point
    // to each piece of each side.
    let around = u32::try_from(4 * PIECES).expect("a count");
    let rings = u32::try_from(RINGS.len()).expect("a count");
    let at = |ring: u32, point: u32| plate.vertex(1 + ring * around + point).expect("a vertex");
    let middle = plate.vertex(0).expect("its middle");
    for point in 0..around {
        let (inner, rim, foot) = (at(0, point), at(rings - 1, point), at(rings, point));
        assert!(
            rim.y > inner.y && inner.y > middle.y,
            "curled up to its rim"
        );
        assert!(
            rim.y > 0.015 && foot.y < 0.0,
            "as thick as its crust, footed below the ground"
        );
        assert!(
            mathf::hypot(foot.x - middle.x, foot.z - middle.z)
                < mathf::hypot(rim.x - middle.x, rim.z - middle.z),
            "undercut beneath its curl"
        );
    }
    // Its corners curl the highest.
    let (corner, side) = (at(rings - 1, 0), at(rings - 1, 1));
    assert!(corner.y > side.y, "{corner:?} {side:?}");
    // Lit from above, a plate's face faces up.
    let hit = plate
        .intersect(
            &Ray::new(Vec3::new(middle.x + 0.01, 1.0, middle.z + 0.02), -Vec3::UP),
            (1e-9, 2.0),
            None,
        )
        .expect("on its face");
    assert!(hit.shading.y > 0.9, "{:?}", hit.shading);
}

#[test]
fn a_cracks_wandering_repeats_with_its_tile() {
    let lattice = Lattice {
        plates: 16,
        cell: 0.2,
        side: 3.2,
        seed: 4,
        variant: 0,
    };
    for step in 0..50u32 {
        let at = (0.13 * f64::from(step), 0.07 * f64::from(step) - 1.0);
        let (here, over) = (lattice.wander(at), lattice.wander((at.0 + 3.2, at.1 - 3.2)));
        assert!((here.0 - over.0).abs() < 1e-9 && (here.1 - over.1).abs() < 1e-9);
        assert!(here.0.abs() <= WANDER * 0.2 + 1e-12);
    }
}

#[test]
fn a_thicker_crust_cracks_wider() {
    assert!(crack_width(0.025) > crack_width(0.006));
    let thin = grown(
        Crust {
            thick: 0.004,
            ..CRUST
        },
        0,
    );
    let thick = grown(
        Crust {
            thick: 0.025,
            ..CRUST
        },
        0,
    );
    assert!(open(&thick, 0.0, 0.5) > open(&thin, 0.0, 0.5));
}

#[test]
fn a_plates_face_dries_pale_over_its_damp_wall() {
    let mud = Mud {
        dry: Vec3::new(0.4, 0.33, 0.24),
        damp: Vec3::new(0.12, 0.09, 0.06),
    };
    let spot = |(inward, down): (f64, f64)| Spot {
        p: Vec3::new(0.3, 0.0, 0.7),
        normal: Vec3::UP,
        height: 0.0,
        width: 0.01,
        mark: 0x1234,
        along: 0.0,
        uv: (inward * 0.2, down * 0.2),
        girth: 0.2,
        instance: 0,
        front: true,
        ground: [0.0; QUANTITIES],
        grows: Grows::default(),
        thatch: 0.0,
        cover: None,
    };
    let (face, wall) = (mud.colour(&spot((0.5, 0.0))), mud.colour(&spot((0.0, 1.0))));
    assert!(
        wall.luminance() < 0.5 * face.luminance(),
        "{face:?} {wall:?}"
    );
    // No two plates quite alike.
    let shade = |mark: u32| {
        mud.colour(&Spot {
            mark,
            ..spot((0.5, 0.0))
        })
        .luminance()
    };
    assert!((0..16).any(|mark| (shade(mark) - shade(0)).abs() > 0.02));
}

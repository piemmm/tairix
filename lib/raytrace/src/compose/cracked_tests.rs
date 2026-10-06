//! Host tests of cracked mud laid over a land: it dries only on level, bare
//! silt, in patches, its tiles square to the land's own lattice and drawn
//! from a few tiles that meet any other.

use alloc::vec::Vec;

use super::*;
use crate::compose::testland::{grounded, land};
use crate::detail::Detail;
use crate::heightfield::Attributes;

/// Silt a flood laid and left, bare of growth; and the same grassed over.
const SILT: Attributes = [60, 230, 0, 25, 0];
const GRASSED: Attributes = [60, 230, 0, 255, 0];

/// The eye, a little off a cell's corner.
const EYE: Vec3 = Vec3::new(0.3, 1.7, 0.4);

/// Where the tiles of mud on a stage of `land`'s, carrying `lie` and lying as
/// `profile` has it, stand, and the prototypes they are drawn from.
fn cracked(profile: &dyn Fn(f64, f64) -> f64, lie: Attributes) -> (Vec<Vec3>, Vec<u32>) {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let land = land(&mut stage, profile, lie);
    grounded(&mut stage);
    let mut dice = Dice::keyed(3, 0);
    crack(&mut stage, &mut dice, (&land, EYE), &wash).expect("cracked");
    let mut prototypes = Vec::new();
    let tiles = stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance {
                pose, prototype, ..
            } => {
                prototypes.push(prototype);
                Some(pose.at)
            }
            _ => None,
        })
        .collect();
    prototypes.sort_unstable();
    prototypes.dedup();
    (tiles, prototypes)
}

#[test]
fn mud_dries_in_patches_of_tiles_square_to_the_land() {
    let (tiles, prototypes) = cracked(&|_, _| 0.0, SILT);
    let reach = Detail::Simple.densities().mud;
    let cells = core::f64::consts::PI * reach * reach / (TILE * TILE);
    let laid = crate::vector::real(tiles.len());
    assert!(
        laid > 0.15 * cells && laid < 0.95 * cells,
        "{laid} of about {cells}: in patches"
    );
    for at in &tiles {
        let (column, row) = (at.x / TILE - 0.5, at.z / TILE - 0.5);
        assert!(
            (column - mathf::round(column)).abs() < 1e-9 && (row - mathf::round(row)).abs() < 1e-9,
            "square to the lattice: {at:?}"
        );
        assert!(mathf::hypot(at.x - EYE.x, at.z - EYE.z) <= reach && at.y.abs() < 1e-9);
    }
    assert!(!prototypes.is_empty() && prototypes.len() <= VARIANTS);
}

#[test]
fn mud_dries_only_on_level_bare_silt() {
    assert!(cracked(&|_, _| 0.0, GRASSED).0.is_empty(), "grassed over");
    assert!(
        cracked(&|x, _| 0.3 * x, SILT).0.is_empty(),
        "too steep to pool"
    );
    // Ground gentle enough to pool but bowed across a tile lays no crust
    // there.
    let swell = core::f64::consts::TAU / 12.0;
    let bowed = |x: f64, z: f64| 0.15 * mathf::sin(swell * x) * mathf::sin(swell * z);
    assert!(cracked(&bowed, SILT).0.len() < cracked(&|_, _| 0.0, SILT).0.len() / 4);
    let dry = crate::land::Lie {
        height: 0.0,
        upright: 1.0,
        wet: 0.5,
        sediment: 0.4,
        road: 0.0,
        path: 0.0,
        green: 0.1,
        snow: 0.0,
    };
    assert!(wash(&dry) && bank(&dry));
    assert!(
        !bank(&crate::land::Lie { wet: 0.95, ..dry }),
        "still sodden"
    );
    assert!(
        !wash(&crate::land::Lie {
            sediment: -0.3,
            ..dry
        }),
        "scoured, not laid"
    );
}

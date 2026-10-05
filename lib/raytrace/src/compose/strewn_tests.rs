//! Host tests of strewn stones: they gather rather than sprinkle evenly,
//! scree lies below a crag and the larger rolled the further, pebbles drift
//! only where water washed the ground, and a detail strewing more shifts no
//! other draw.

use alloc::vec::Vec;

use super::*;
use crate::course::Courses;
use crate::detail::Detail;
use crate::ground::{Ground, Palette};
use crate::heightfield::{Attributes, Heightfield};
use crate::land::NESTS;
use crate::material::{Finish, Material};
use crate::pigment::Pigment;
use crate::shape::Shape;
use crate::vector::{Frame, Pose};

/// A land of one grid 400 m across whose height `profile` gives, carrying
/// `lie` everywhere, on `stage`.
fn land(stage: &mut Stage, profile: &dyn Fn(f64, f64) -> f64, lie: Attributes) -> Land {
    let mut field = Heightfield::new(400, (-200.0, -200.0), 1.0, false).expect("a grid");
    let side = field.side();
    assert!(field.carry_attributes());
    {
        let (heights, attributes) = field.rows_mut(0..side);
        for (index, slot) in heights.iter_mut().enumerate() {
            let (column, row) = (index % side, index / side);
            *slot = crate::vector::single(profile(
                -200.0 + crate::vector::real(column),
                -200.0 + crate::vector::real(row),
            ));
        }
        attributes.fill(lie);
    }
    field.seal();
    stage.fields.push(field);
    Land {
        far: 0,
        nests: [None; NESTS],
        water: None,
        near_water: None,
        horizon: None,
        rivers: Courses::none(),
        form: None,
        roads: Courses::none(),
        road: None,
        crossings: Vec::new(),
        sea: None,
        centre: (0.0, 0.0),
        reach: 200.0,
    }
}

/// Dry ground neither worn nor built up, on no road, bare of snow.
const DRY: Attributes = [0, 128, 0, 255, 0];

/// The stage's stones, as where each lies and how large.
fn stones(stage: &Stage) -> Vec<((f64, f64), f64)> {
    stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance { pose, scale, .. } => Some(((pose.at.x, pose.at.z), 2.0 * scale)),
            _ => None,
        })
        .collect()
}

/// `land`'s ground, as a stage holds a land's: a material the land's own
/// grid is laid in, whose rock its stones are of.
fn grounded(stage: &mut Stage) {
    let palette = Palette {
        grass: Vec3::splat(0.2),
        dry: Vec3::splat(0.3),
        moss: Vec3::splat(0.2),
        earth: Vec3::splat(0.3),
        silt: Vec3::splat(0.4),
        rock: Vec3::splat(0.4),
        strata: Vec3::splat(0.3),
        lichen: Vec3::splat(0.5),
        sand: Vec3::splat(0.6),
        snow: Vec3::ONE,
    };
    let ground = Ground {
        palette,
        shore: -1e3,
        snow_line: 1e5,
        cliff: 0.6,
        bedding: 2.0,
        seed: 1,
        road: None,
        floor: None,
    };
    let material = stage
        .material(Material::new(Pigment::Ground(ground), Finish::Matte))
        .expect("a material");
    stage
        .add(
            Shape::Land { field: 0 },
            material,
            Pose::new(Vec3::ZERO, Frame::WORLD),
            false,
        )
        .expect("the land");
}

/// An eye at the land's middle, looking along +z.
const MIDDLE: Vantage = Vantage {
    eye: Vec3::new(0.0, 1.7, 0.0),
    heading: 0.0,
};

/// Strew `strewing`, all the way round the eye at the land's middle out to
/// 180 m, over `land` on `stage`, where the ground stands no steeper than a
/// stone rests on.
fn strew_over(stage: &mut Stage, land: &Land, boulders: (u32, (f64, f64)), seed: u64) -> u64 {
    let mut dice = Dice::keyed(seed, 0);
    let strewing = Strewing {
        boulders,
        reach: (0.0, 180.0),
        spread: core::f64::consts::PI,
    };
    strew(stage, &mut dice, (land, &MIDDLE), strewing, &|lie: &Lie| {
        lie.upright > 0.7
    })
    .expect("strewn");
    dice.wide()
}

#[test]
fn stones_gather_in_patches_and_never_sprinkle_evenly() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let land = land(&mut stage, &|_, _| 0.0, DRY);
    grounded(&mut stage);
    strew_over(&mut stage, &land, (1500, (0.2, 0.6)), 9);
    // Counted in squares 20 m a side out to 120 m, an even sprinkle's counts
    // spread about as far as their mean; gathered stones' far further.
    let placed: Vec<_> = stones(&stage)
        .into_iter()
        .filter(|&((x, z), _)| x.abs() < 120.0 && z.abs() < 120.0)
        .collect();
    assert!(placed.len() > 300, "{} strewn", placed.len());
    let mut counts = [0u32; 144];
    for &((x, z), _) in &placed {
        let square = |at: f64| crate::vector::whole(mathf::floor((at + 120.0) / 20.0)).min(11);
        if let Some(slot) = counts.get_mut(square(z) * 12 + square(x)) {
            *slot += 1;
        }
    }
    let mean = counts.iter().map(|&count| f64::from(count)).sum::<f64>() / 144.0;
    let variance = counts
        .iter()
        .map(|&count| (f64::from(count) - mean) * (f64::from(count) - mean))
        .sum::<f64>()
        / 144.0;
    assert!(
        variance > 3.0 * mean,
        "gathered: {variance} about a mean of {mean}"
    );
}

#[test]
fn scree_lies_below_a_crag_and_the_larger_rolled_the_further() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    // A cliff 30 m high at z = 0, the talus below it falling a little more
    // gently than a half.
    let land = land(
        &mut stage,
        &|_, z| {
            if z > 0.0 {
                30.0 + 0.1 * z
            } else if z > -4.0 {
                30.0 + 7.0 * z
            } else {
                2.0 + 0.6 * (z + 4.0)
            }
        },
        DRY,
    );
    let below = |z: f64| {
        let lie = land.lie(&stage.fields, 5.0, z);
        scree(&land, &stage.fields, ((5.0, z), &lie))
    };
    let (near, far) = (
        below(-8.0).expect("scree at its foot"),
        below(-24.0).expect("further out"),
    );
    assert!(
        near.0 > far.0,
        "likelier nearer the crag: {near:?} against {far:?}"
    );
    assert!(
        far.1 > near.1,
        "the larger rolled further: {near:?} against {far:?}"
    );
    assert!(below(-80.0).is_none(), "none far out on the flat");
    assert!(below(60.0).is_none(), "none above the crag");
}

#[test]
fn pebbles_drift_only_where_water_washed_the_ground() {
    let washed: Attributes = [140, 160, 0, 255, 0];
    for (lie, drifted) in [(DRY, false), (washed, true)] {
        let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
        let land = land(&mut stage, &|_, _| 0.0, lie);
        grounded(&mut stage);
        strew_over(&mut stage, &land, (0, (0.2, 0.6)), 4);
        let placed = stones(&stage);
        assert_eq!(!placed.is_empty(), drifted, "{lie:?}: {}", placed.len());
        let reach = stage.densities.strewn.pebbles + DRIFT;
        for &((x, z), size) in &placed {
            assert!((PEBBLES.0..=PEBBLES.1).contains(&size), "{size}");
            assert!(mathf::hypot(x, z) <= reach + 1e-9, "near the eye");
        }
    }
}

#[test]
fn a_detail_strewing_more_shifts_no_other_draw() {
    let after = |detail: Detail| {
        let mut stage = Stage::new(detail.densities()).expect("a stage");
        let land = land(&mut stage, &|_, _| 0.0, DRY);
        grounded(&mut stage);
        let next = strew_over(&mut stage, &land, (60, (0.2, 1.4)), 3);
        (next, stones(&stage).len())
    };
    let (simple, maximum) = (after(Detail::Simple), after(Detail::Maximum));
    assert_eq!(simple.0, maximum.0, "the draws after the stones");
    assert!(maximum.1 > simple.1, "{} against {}", maximum.1, simple.1);
}

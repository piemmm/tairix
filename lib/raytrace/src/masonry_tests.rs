//! Host tests of masonry's colour: each unit its own shade, and each face
//! weathered as water and age leave it.

use super::*;
use crate::heightfield::CHANNELS;

fn masonry(unit: Unit) -> Masonry {
    Masonry {
        bases: [Vec3::new(0.62, 0.55, 0.42), Vec3::new(0.5, 0.45, 0.34)],
        flecks: [Vec3::new(0.4, 0.36, 0.3), Vec3::new(0.7, 0.66, 0.6)],
        grain: 300.0,
        shade: 0.1,
        weathering: 0.0,
        damp: 0.0,
        foot: 0.0,
        unit,
        seed: 3,
    }
}

fn spot(mark: u32, (p, normal): (Vec3, Vec3), (along, uv): (f64, (f64, f64))) -> Spot {
    Spot {
        p,
        normal,
        height: p.y,
        // Far enough that its grain settles to its mean.
        width: 0.05,
        mark,
        along,
        uv,
        girth: 0.0,
        instance: 0,
        front: true,
        ground: [0.0; CHANNELS],
        thatch: 0.0,
        cover: None,
    }
}

fn face(mark: u32) -> Spot {
    spot(
        mark,
        (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
        (2.0, (0.0, 0.0)),
    )
}

fn saturation(colour: Vec3) -> f64 {
    let (high, low) = (
        colour.x.max(colour.y).max(colour.z),
        colour.x.min(colour.y).min(colour.z),
    );
    (high - low) / high.max(1e-9)
}

#[test]
fn each_unit_wears_its_own_shade() {
    let stone = masonry(Unit::Stone);
    let mut shades: alloc::vec::Vec<u64> = (0..64)
        .map(|mark| stone.colour(&face(mark)).x.to_bits())
        .collect();
    shades.sort_unstable();
    shades.dedup();
    assert!(shades.len() > 60, "{} shades among 64 stones", shades.len());
}

/// Field stones, gathered from many beds, differ more one from the next than
/// quarried stones do, and each is blotched across its own face where a
/// quarried stone is even.
#[test]
fn a_field_stone_is_blotched_and_wanders_further_in_shade_than_a_quarried_one() {
    let (quarried, gathered) = (masonry(Unit::Stone), masonry(Unit::Field));
    let range = |shades: &mut dyn Iterator<Item = f64>| {
        shades.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), shade| (low.min(shade), high.max(shade)))
    };
    let between = |stone: &Masonry| {
        let (low, high) = range(&mut (0..64).map(|mark| stone.colour(&face(mark)).luminance()));
        high - low
    };
    assert!(
        between(&gathered) > 1.4 * between(&quarried),
        "{} between field stones, {} between quarried ones",
        between(&gathered),
        between(&quarried)
    );
    let across = |stone: &Masonry| {
        let (low, high) = range(&mut (0..64).map(|index| {
            let draw = |salt: u32| unit(mix32(index ^ salt)) - 0.5;
            let p = Vec3::new(0.3 * draw(1), 1.0 + 0.2 * draw(2), 0.0);
            stone.colour(&spot(7, (p, Vec3::new(0.0, 0.0, 1.0)), (2.0, (0.0, 0.0)))).luminance()
        }));
        (high - low) / high
    };
    assert!(across(&gathered) > 0.15, "a field stone's face is even: {}", across(&gathered));
    assert!(across(&quarried) < 0.02, "a quarried stone's face is blotched: {}", across(&quarried));
}

#[test]
fn age_greys_and_darkens_a_face_and_crusts_what_faces_down() {
    let (new, old) = (
        masonry(Unit::Stone),
        Masonry {
            weathering: 1.0,
            ..masonry(Unit::Stone)
        },
    );
    let mean = |stone: &Masonry, normal: Vec3| {
        (0..64).fold(Vec3::ZERO, |sum, mark| {
            sum + stone.colour(&spot(
                mark,
                (Vec3::new(0.0, 1.0, 0.0), normal),
                (2.0, (0.0, 0.0)),
            ))
        }) * (1.0 / 64.0)
    };
    let (fresh, aged) = (
        mean(&new, Vec3::new(0.0, 0.0, 1.0)),
        mean(&old, Vec3::new(0.0, 0.0, 1.0)),
    );
    assert!(
        aged.luminance() < fresh.luminance(),
        "{aged:?} is no darker than {fresh:?}"
    );
    assert!(
        saturation(aged) < saturation(fresh),
        "{aged:?} is no greyer than {fresh:?}"
    );
    let under = mean(&old, -Vec3::UP);
    assert!(
        under.luminance() < 0.6 * aged.luminance(),
        "a sheltered face is not crusted: {under:?}"
    );
}

#[test]
fn algae_greens_the_foot_of_a_damp_wall() {
    let stone = Masonry {
        weathering: 1.0,
        damp: 1.0,
        foot: 0.5,
        ..masonry(Unit::Stone)
    };
    let green = |y: f64| {
        let colour = stone.colour(&spot(
            9,
            (Vec3::new(0.3, y, 0.0), Vec3::new(0.0, 0.0, 1.0)),
            (2.0, (0.0, 0.0)),
        ));
        colour.y / colour.x
    };
    assert!(
        green(0.1) > green(3.0),
        "{} at the foot, {} above",
        green(0.1),
        green(3.0)
    );
}

#[test]
fn a_brick_is_burnt_at_its_end_and_keeps_old_mortar_at_its_edges() {
    let red = |unit| Masonry {
        bases: [Vec3::new(0.36, 0.11, 0.06), Vec3::new(0.28, 0.08, 0.05)],
        flecks: [Vec3::new(0.2, 0.06, 0.03), Vec3::new(0.45, 0.2, 0.12)],
        ..masonry(unit)
    };
    let burnt = red(Unit::Brick {
        burnt: 1.0,
        reclaimed: 0.0,
    });
    let colour = |stone: &Masonry, along: f64, uv: (f64, f64)| {
        // The point on the face moves with where on the brick it is.
        let p = Vec3::new(0.1 + 0.1 * uv.0, 1.0 + 0.03 * uv.1, 0.0);
        stone
            .colour(&spot(5, (p, Vec3::new(0.0, 0.0, 1.0)), (along, uv)))
            .luminance()
    };
    assert!(
        colour(&burnt, 0.0, (0.0, 0.0)) < 0.5 * colour(&burnt, 2.0, (0.0, 0.0)),
        "a header's end is not burnt"
    );
    let reclaimed = red(Unit::Brick {
        burnt: 0.0,
        reclaimed: 1.0,
    });
    let (middle, edges) = (
        colour(&reclaimed, 2.0, (0.0, 0.0)),
        (0..32)
            .map(|index| colour(&reclaimed, 2.0, (-0.9 + 0.06 * f64::from(index), 0.99)))
            .fold(0.0, f64::max),
    );
    assert!(
        edges > middle * 1.2,
        "{edges} at its edges against {middle} in its middle"
    );
}

//! The drop shadow held to its own definition: the silhouette, dropped by the
//! reach, blurred by the kernel and evaluated directly — at each level, and
//! crossfaded between them.

use alloc::vec;
use alloc::vec::Vec;

use super::{
    kernel_ramp, mixed_pixel, shadow_reach, shadow_row, Caster, DropShadow, LevelRow, MAX_REACH,
    SHADOW_ONE,
};
use crate::color::{Color, Pixel};
use crate::dither::DitherRow;
use crate::round::RoundRect;

/// The two shadows a surface eases between: a further, darker one standing
/// in front, and a shorter, lighter one resting behind.
fn levels() -> [DropShadow; 2] {
    [
        DropShadow::new(6, Color::rgba(0, 0, 0, 0x8c)),
        DropShadow::new(4, Color::rgba(0, 0, 0, 0x50)),
    ]
}

/// A caster at `(left, top)`, `width` × `height`, cut by `radius` (square for
/// nought) and drawn at `opacity`.
fn caster(
    (left, top): (i32, i32),
    (width, height): (u32, u32),
    radius: u32,
    opacity: u8,
) -> Caster {
    let wide = |extent: u32| i32::try_from(extent).expect("a small extent");
    Caster {
        left,
        top,
        right: left + wide(width),
        bottom: top + wide(height),
        silhouette: (radius > 0).then_some(RoundRect {
            width,
            height,
            radius,
        }),
        opacity,
    }
}

/// The kernel's weight at offset `t`, as a share of its whole.
fn tap(reach: u32, t: i64) -> f64 {
    let weight = |t: u32| {
        let span = f64::from(reach) + 1.0;
        let t = f64::from(t);
        let base = span * span - t * t;
        base * base
    };
    let Some(t) = u32::try_from(t.unsigned_abs()).ok().filter(|t| *t <= reach) else {
        return 0.0;
    };
    let whole = weight(0) + 2.0 * (1..=reach).map(weight).sum::<f64>();
    weight(t) / whole
}

/// What the shadow is by definition at `(x, y)`, on the weight scale: every
/// pixel of the silhouette dropped by the reach and spread by the kernel in
/// both axes, laid only where the silhouette leaves the pixel uncovered, and
/// scaled by the caster's opacity.
fn defined_weight(reach: u32, caster: &Caster, (x, y): (i32, i32)) -> f64 {
    let coverage = |lx: u32, ly: u32| {
        caster
            .silhouette
            .map_or(255, |silhouette| silhouette.coverage(lx, ly))
    };
    let width = u32::try_from(caster.right - caster.left).expect("a width");
    let height = u32::try_from(caster.bottom - caster.top).expect("a height");
    let mut intensity = 0.0;
    for ly in 0..height {
        let vertical = tap(
            reach,
            i64::from(y) - i64::from(reach) - i64::from(caster.top) - i64::from(ly),
        );
        if vertical == 0.0 {
            continue;
        }
        for lx in 0..width {
            let horizontal = tap(reach, i64::from(x) - i64::from(caster.left) - i64::from(lx));
            intensity += f64::from(coverage(lx, ly)) / 255.0 * horizontal * vertical;
        }
    }
    let covered = u32::try_from(x - caster.left)
        .ok()
        .zip(u32::try_from(y - caster.top).ok())
        .filter(|&(lx, ly)| lx < width && ly < height)
        .map_or(0, |(lx, ly)| coverage(lx, ly));
    intensity * f64::from(255 - covered) * f64::from(caster.opacity) / 255.0
}

/// Every point a shadow of `reach` from `caster` could reach, and a margin of
/// points it must not.
fn around(caster: &Caster, reach: u32) -> impl Iterator<Item = (i32, i32)> {
    let reach = i32::try_from(reach).expect("a small reach");
    let (left, top, right, bottom) = (caster.left, caster.top, caster.right, caster.bottom);
    ((top - 2)..(bottom + 2 * reach + 2))
        .flat_map(move |y| ((left - reach - 2)..(right + reach + 2)).map(move |x| (x, y)))
}

fn weight_at(row: Option<&LevelRow<'_>>, x: i32) -> u8 {
    row.map_or(0, |row| row.weight(x))
}

#[test]
fn the_kernel_rises_to_one_through_a_symmetric_ramp() {
    for reach in [1, 6, 12, MAX_REACH] {
        let ramp = kernel_ramp(reach).expect("a ramp");
        let taps = usize::try_from(2 * reach + 1).expect("a tap count");
        assert_eq!(ramp.len(), taps);
        assert_eq!(ramp.last().copied(), Some(SHADOW_ONE), "reach {reach}");
        assert!(
            ramp.windows(2).all(|pair| pair[0] <= pair[1]),
            "reach {reach}: a running sum never falls"
        );
        // The kernel is even, so what has accumulated `d` in from one end is
        // what remains `d` in from the other.
        for (early, late) in ramp.iter().zip(ramp.iter().rev().skip(1)) {
            assert!(
                (early + late).abs_diff(SHADOW_ONE) <= 1,
                "reach {reach}: {early} + {late}"
            );
        }
    }
    assert!(kernel_ramp(0).is_none());
}

#[test]
fn a_reach_is_held_to_what_the_kernel_is_sized_for_and_a_clear_colour_casts_none() {
    let dark = Color::rgba(0, 0, 0, 0x8c);
    assert_eq!(shadow_reach(6, dark), 6);
    assert_eq!(shadow_reach(MAX_REACH + 9, dark), MAX_REACH);
    assert_eq!(shadow_reach(6, Color::rgba(0, 0, 0, 0)), 0);
    let clear = DropShadow::new(6, Color::rgba(0, 0, 0, 0));
    assert_eq!(clear.reach(), 0);
    assert!(clear.ramp().is_empty());
    let unreaching = DropShadow::new(0, dark);
    let caster = caster((10, 10), (20, 20), 0, 255);
    for shadow in [&clear, &unreaching, &DropShadow::NONE] {
        assert!((0..60).all(|y| shadow_row(shadow, shadow, &caster, 140, y).is_none()));
    }
}

#[test]
fn a_shadow_is_its_silhouette_dropped_and_blurred() {
    // A level reaches every value through a separable product less corner
    // tiles; the definition sums the silhouette itself. They agree to the
    // rounding of the fixed point, at every point, at both levels, for a
    // square silhouette, rounded ones, and a part-opaque surface.
    for mut shadow in levels() {
        let reach = shadow.reach();
        for (radius, opacity) in [(0, 255), (6, 255), (9, 255), (9, 140)] {
            let caster = caster((20, 10), (26, 20), radius, opacity);
            shadow.ensure_tile(caster.silhouette.map_or(0, RoundRect::corner_reach));
            let mut worst = 0.0f64;
            for (x, y) in around(&caster, reach) {
                let row = shadow.row(&caster, y);
                let drawn = weight_at(row.as_ref(), x);
                let defined = defined_weight(reach, &caster, (x, y));
                worst = worst.max((f64::from(drawn) - defined).abs());
                if defined == 0.0 {
                    assert_eq!(drawn, 0, "r={radius} at ({x}, {y}) draws where none falls");
                }
            }
            assert!(
                worst <= 1.5,
                "reach {reach}, r={radius} at {opacity}: off by up to {worst}"
            );
        }
    }
}

#[test]
fn a_blended_row_lays_exactly_what_each_pixel_samples() {
    // However a row is laid — a solid span beneath a caster, a ramp read from
    // the straight-edge profile across it, a column evaluated one at a time
    // near a corner, or a crossfade between the levels — it is the per-pixel
    // sample laid over what was there, bit for bit, for a tall caster, a
    // part-opaque one, one too narrow for its ramps to see only their own
    // edge, and one past the surface's left edge.
    let [mut lifted, mut resting] = levels();
    for shadow in [&mut lifted, &mut resting] {
        shadow.ensure_tile(6);
    }
    let reach = lifted.reach().max(resting.reach());
    let wide = i32::try_from(reach).expect("a small reach");
    let under: Pixel = Color::rgb(0x70, 0x90, 0xb0).premultiply();
    let (mut straight, mut mixed) = (0, 0);
    for lift in [u8::MAX, 0, 140] {
        for (at, size, opacity) in [
            ((30, 12), (60, 80), 255),
            ((30, 12), (60, 80), 140),
            ((30, 12), (4, 80), 255),
            ((-40, 12), (60, 80), 255),
            ((-40, 12), (60, 80), 140),
        ] {
            let caster = caster(at, size, 6, opacity);
            for y in caster.top..caster.bottom + 2 * wide {
                let Some(row) = shadow_row(&lifted, &resting, &caster, lift, y) else {
                    continue;
                };
                straight += usize::from(row.is_straight());
                mixed += usize::from(row.is_mixed());
                let first = caster.left - wide - 3;
                let width =
                    usize::try_from(caster.right - caster.left + 2 * wide + 6).expect("a width");
                let mut blended: Vec<Pixel> = vec![under; width];
                let dither = DitherRow::at(y.cast_unsigned());
                let _ = row.blend_into(&mut blended, first, dither);
                for (x, got) in (first..).zip(&blended) {
                    let bias = dither.bias(x.cast_unsigned());
                    let expected = row
                        .sample(x, bias)
                        .map_or(under, |shadow| shadow.over_biased(under, bias));
                    assert_eq!(*got, expected, "lift {lift}, {caster:?}: ({x}, {y})");
                }
            }
        }
    }
    assert!(straight > 0, "no row took the straight-edge profile");
    assert!(mixed > 0, "no row crossfaded");
}

/// At either end of an ease the crossfade is that level's own shadow, and in
/// between it holds each in proportion: a level's share of the darkening is
/// its share of the lift.
#[test]
fn a_shadow_between_levels_is_the_two_crossfaded() {
    let [mut lifted, mut resting] = levels();
    for shadow in [&mut lifted, &mut resting] {
        shadow.ensure_tile(6);
    }
    let caster = caster((30, 12), (60, 40), 6, 255);
    let y = caster.bottom + 2;
    let high = lifted.row(&caster, y);
    let low = resting.row(&caster, y);
    for x in caster.left - 8..caster.right + 8 {
        let alone = |row: Option<&LevelRow<'_>>| {
            row.and_then(|row| row.sample(x, 128))
                .map_or(0, |pixel| pixel.a)
        };
        let (high_alone, low_alone) = (alone(high.as_ref()), alone(low.as_ref()));
        let at = |lift| mixed_pixel(high.as_ref(), low.as_ref(), lift, x, 128).a;
        assert_eq!(
            at(u8::MAX),
            high_alone,
            "at {x} the lifted end is the lifted shadow"
        );
        assert_eq!(
            at(0),
            low_alone,
            "at {x} the resting end is the resting shadow"
        );
        let half = at(128);
        assert!(
            half >= high_alone.min(low_alone) && half <= high_alone.max(low_alone),
            "at {x}: {half} between {low_alone} and {high_alone}"
        );
    }
}

#[test]
fn a_notch_is_built_once_per_radius_and_kept_only_while_wanted() {
    let [mut shadow, _] = levels();
    assert!(
        shadow.notch(9).is_none(),
        "nothing is built before it is asked for"
    );
    shadow.ensure_tile(9);
    shadow.ensure_tile(9);
    shadow.ensure_tile(0);
    let notch = shadow.notch(9).expect("the tile for 9");
    let side = notch.side;
    assert_eq!(
        side,
        9 + 2 * shadow.reach(),
        "the notch widened by the reach"
    );
    assert_eq!(
        notch.values.len(),
        usize::try_from(side * side).expect("a tile")
    );
    assert!(shadow.notch(0).is_none(), "a square corner has no notch");
    shadow.ensure_tile(4);
    shadow.retain_tiles(|radius| radius == 4);
    assert!(shadow.notch(9).is_none(), "a radius out of use is dropped");
    assert!(shadow.notch(4).is_some(), "a kept radius stays");
}

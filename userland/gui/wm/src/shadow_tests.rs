//! The shadow kit held to its own definition: the silhouette, dropped by the
//! reach, blurred by the kernel and evaluated directly — at each level, and
//! crossfaded between them.

use alloc::vec::Vec;

use super::{
    kernel_ramp, mixed_pixel, shadow_footprint, Elevation, Level, LevelRow, Lift, ShadowKit,
    ShadowRow, ONE,
};
use crate::color::{Color, DitherRow, Pixel};
use crate::corner::Corners;
use crate::geometry::{Rect, Scale};
use crate::surface::Surface;
use crate::window::{Window, WindowId, WindowShape};
use tairix_abi::driver::display::RefreshRate;
use tairix_colour::Rgba;
use tairix_theme::{Fade, Theme};

/// The display the shadows ease on.
const RATE: RefreshRate = RefreshRate::ASSUMED;

fn kit() -> ShadowKit {
    ShadowKit::new(Scale::ONE, &Theme::dark())
}

/// Both levels, lifted first.
const LEVELS: [Elevation; 2] = [Elevation::LIFTED, Elevation::RESTING];

/// The silhouette of a `corners`-shaped surface the size of `bounds`.
fn shape_of(bounds: Rect, corners: Corners) -> Option<WindowShape> {
    let surface = Surface::new(bounds.width, bounds.height).expect("a surface");
    let mut window = Window::new(WindowId(1), bounds.origin, surface);
    window.set_corners(corners);
    window.shape()
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

/// What the shadow is by definition at screen `(x, y)`, on the weight scale:
/// every pixel of the silhouette dropped by the reach and spread by the
/// kernel in both axes, laid only where the silhouette leaves the pixel
/// uncovered, and scaled by `opacity`.
fn defined_weight(
    reach: u32,
    bounds: Rect,
    shape: Option<WindowShape>,
    opacity: u8,
    (x, y): (i32, i32),
) -> f64 {
    let coverage = |lx: u32, ly: u32| shape.map_or(255, |shape| shape.coverage(lx, ly));
    let mut intensity = 0.0;
    for ly in 0..bounds.height {
        let vertical = tap(
            reach,
            i64::from(y) - i64::from(reach) - i64::from(bounds.top()) - i64::from(ly),
        );
        if vertical == 0.0 {
            continue;
        }
        for lx in 0..bounds.width {
            let horizontal = tap(
                reach,
                i64::from(x) - i64::from(bounds.left()) - i64::from(lx),
            );
            intensity += f64::from(coverage(lx, ly)) / 255.0 * horizontal * vertical;
        }
    }
    let covered = u32::try_from(i64::from(x) - i64::from(bounds.left()))
        .ok()
        .zip(u32::try_from(i64::from(y) - i64::from(bounds.top())).ok())
        .filter(|&(lx, ly)| lx < bounds.width && ly < bounds.height)
        .map_or(0, |(lx, ly)| coverage(lx, ly));
    intensity * f64::from(255 - covered) * f64::from(opacity) / 255.0
}

/// Every screen point a shadow of `reach` from `bounds` could reach, and a
/// margin of points it must not.
fn around(bounds: Rect, reach: u32) -> impl Iterator<Item = (i32, i32)> {
    let reach = i32::try_from(reach).expect("a small reach");
    let (left, top) = (bounds.left(), bounds.top());
    let (right, bottom) = (bounds.right(), bounds.bottom());
    ((top - 2)..(bottom + 2 * reach + 2))
        .flat_map(move |y| ((left - reach - 2)..(right + reach + 2)).map(move |x| (x, y)))
}

fn weight_at(row: Option<&LevelRow<'_>>, x: i32) -> u8 {
    row.map_or(0, |row| row.weight(x))
}

#[test]
fn the_kernel_rises_to_one_through_a_symmetric_ramp() {
    for reach in [1, 6, 12, 255] {
        let ramp = kernel_ramp(reach).expect("a ramp");
        let taps = usize::try_from(2 * reach + 1).expect("a tap count");
        assert_eq!(ramp.len(), taps);
        assert_eq!(ramp.last().copied(), Some(ONE), "reach {reach}");
        assert!(
            ramp.windows(2).all(|pair| pair[0] <= pair[1]),
            "reach {reach}: a running sum never falls"
        );
        // The kernel is even, so what has accumulated `d` in from one end is
        // what remains `d` in from the other.
        for (early, late) in ramp.iter().zip(ramp.iter().rev().skip(1)) {
            assert!(
                (early + late).abs_diff(ONE) <= 1,
                "reach {reach}: {early} + {late}"
            );
        }
    }
    assert!(kernel_ramp(0).is_none());
}

#[test]
fn a_shadow_is_its_silhouette_dropped_and_blurred() {
    // A level reaches every value through a separable product less corner
    // tiles; the definition sums the silhouette itself. They agree to the
    // rounding of the fixed point, at every point, at both levels, for a
    // square silhouette, the theme's own radius, a radius built on demand,
    // and a part-opaque surface.
    let mut kit = kit();
    let bounds = Rect::new(20, 10, 26, 20);
    for elevation in LEVELS {
        let level: &mut Level = kit.level_mut(elevation);
        let reach = level.reach;
        assert!(reach > 0, "the theme casts a shadow at {elevation:?}");
        for (corners, opacity) in [
            (Corners::Square, 255),
            (Corners::painted(6), 255),
            (Corners::painted(9), 255),
            (Corners::painted(9), 140),
        ] {
            let shape = shape_of(bounds, corners);
            level.ensure_tile(shape.map_or(0, WindowShape::corner_reach));
            let mut worst = 0.0f64;
            for (x, y) in around(bounds, reach) {
                let row = level.row(bounds, shape, opacity, y);
                let drawn = weight_at(row.as_ref(), x);
                let defined = defined_weight(reach, bounds, shape, opacity, (x, y));
                worst = worst.max((f64::from(drawn) - defined).abs());
                if defined == 0.0 {
                    assert_eq!(drawn, 0, "{corners:?} at ({x}, {y}) draws where none falls");
                }
            }
            assert!(
                worst <= 1.5,
                "{elevation:?}, {corners:?} at {opacity}: off by up to {worst}"
            );
        }
    }
}

#[test]
fn a_blended_row_lays_exactly_what_each_pixel_samples() {
    // However a row is laid — a solid span beneath a window, a ramp read from
    // the straight-edge profile across it, a column evaluated one at a time
    // near a corner, or a crossfade between the levels — it is the per-pixel
    // sample laid over what was there, bit for bit, for a tall window, a
    // part-opaque one, one too narrow for its ramps to see only their own
    // edge, and one past the screen's left edge, as a layer of one is baked.
    let kit = kit();
    let reach = i32::try_from(kit.reach()).expect("a small reach");
    let under: Pixel = Color::rgb(0x70, 0x90, 0xb0).premultiply();
    let (mut straight, mut mixed) = (0, 0);
    for elevation in [Elevation::LIFTED, Elevation::RESTING, Elevation(140)] {
        for (bounds, opacity) in [
            (Rect::new(30, 12, 60, 80), 255),
            (Rect::new(30, 12, 60, 80), 140),
            (Rect::new(30, 12, 4, 80), 255),
            (Rect::new(-40, 12, 60, 80), 255),
            (Rect::new(-40, 12, 60, 80), 140),
        ] {
            let shape = shape_of(bounds, Corners::painted(6));
            for y in bounds.top()..bounds.bottom() + 2 * reach {
                let Some(row) = kit.row(bounds, shape, opacity, elevation, y) else {
                    continue;
                };
                match &row {
                    ShadowRow::One(level) => {
                        straight += usize::from(level.straight_edge().is_some());
                    }
                    ShadowRow::Mixed { .. } => mixed += 1,
                }
                let first = bounds.left() - reach - 3;
                let width = usize::try_from(bounds.width + 2 * kit.reach() + 6).expect("a width");
                let mut blended: Vec<Pixel> = alloc::vec![under; width];
                let dither = DitherRow::at(y.cast_unsigned());
                let _ = row.blend_into(&mut blended, first, dither);
                for (x, got) in (first..).zip(&blended) {
                    let bias = dither.bias(x.cast_unsigned());
                    let expected = row
                        .sample(x, bias)
                        .map_or(under, |shadow| shadow.over_biased(under, bias));
                    assert_eq!(
                        *got, expected,
                        "{elevation:?}, {bounds:?} at {opacity}: ({x}, {y})"
                    );
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
    let kit = kit();
    let bounds = Rect::new(30, 12, 60, 40);
    let shape = shape_of(bounds, Corners::painted(6));
    let y = bounds.bottom() + 2;
    let lifted = kit.level(Elevation::LIFTED).row(bounds, shape, 255, y);
    let resting = kit.level(Elevation::RESTING).row(bounds, shape, 255, y);
    for x in bounds.left() - 8..bounds.right() + 8 {
        let alone = |row: Option<&LevelRow<'_>>| {
            row.and_then(|row| row.sample(x, 128))
                .map_or(0, |pixel| pixel.a)
        };
        let (high, low) = (alone(lifted.as_ref()), alone(resting.as_ref()));
        let at = |lift| mixed_pixel(lifted.as_ref(), resting.as_ref(), lift, x, 128).a;
        assert_eq!(
            at(u8::MAX),
            high,
            "at {x} the lifted end is the lifted shadow"
        );
        assert_eq!(at(0), low, "at {x} the resting end is the resting shadow");
        let half = at(128);
        assert!(
            half >= high.min(low) && half <= high.max(low),
            "at {x}: {half} between {low} and {high}"
        );
    }
}

#[test]
fn a_resting_window_casts_a_lighter_shorter_shadow() {
    let kit = kit();
    let (lifted, resting) = (kit.level(Elevation::LIFTED), kit.level(Elevation::RESTING));
    assert!(resting.reach < lifted.reach);
    assert_eq!(
        kit.reach(),
        lifted.reach,
        "the footprint is the further one's"
    );
    let bounds = Rect::new(30, 12, 60, 40);
    let y = bounds.bottom() + 1;
    let x = i32::midpoint(bounds.left(), bounds.right());
    let darkness = |level: &Level| {
        level
            .row(bounds, None, 255, y)
            .and_then(|row| row.sample(x, 128))
            .map_or(0, |pixel| pixel.a)
    };
    assert!(darkness(resting) < darkness(lifted));
    assert!(darkness(resting) > 0, "a resting window still casts one");
}

#[test]
fn a_tile_for_a_radius_nothing_rounds_by_is_not_kept() {
    let mut kit = kit();
    kit.ensure_tile(9);
    for elevation in LEVELS {
        assert!(kit.level(elevation).tile(9).is_some());
    }
    kit.retain_tiles(|radius| radius == 11);
    let theme = Theme::dark();
    for elevation in LEVELS {
        let level = kit.level(elevation);
        assert!(level.tile(9).is_none(), "a radius out of use is dropped");
        for radius in [
            theme.metrics().window_corner_radius,
            theme.metrics().popup_corner_radius,
        ] {
            assert!(
                level.tile(radius).is_some(),
                "the theme's own {radius} stays"
            );
        }
    }
}

#[test]
fn a_theme_without_a_shadow_casts_nothing_anywhere() {
    // Either token turns it off, at both levels: no reach, or a colour that
    // darkens nothing.
    let base = Theme::dark();
    let with = |metrics: tairix_theme::Metrics, shadow: Rgba| {
        Theme::new(
            base.id(),
            base.name(),
            base.appearance(),
            tairix_theme::Palette {
                drop_shadow: shadow,
                inactive_drop_shadow: shadow,
                ..*base.palette()
            },
            metrics,
            *base.fonts(),
            base.cursors().clone(),
            base.motion(),
            base.density(),
            base.contrast(),
        )
    };
    let unreaching = tairix_theme::Metrics {
        drop_shadow_reach: 0,
        inactive_drop_shadow_reach: 0,
        ..*base.metrics()
    };
    let clear = base.palette().drop_shadow.with_alpha(0);
    let bounds = Rect::new(10, 10, 20, 20);
    for theme in [
        with(unreaching, base.palette().drop_shadow),
        with(*base.metrics(), clear),
    ] {
        let kit = ShadowKit::new(Scale::ONE, &theme);
        assert_eq!(kit.reach(), 0);
        assert_eq!(shadow_footprint(bounds, Scale::ONE, &theme), bounds);
        for elevation in [Elevation::LIFTED, Elevation::RESTING, Elevation(90)] {
            assert!((0..60).all(|y| kit.row(bounds, None, 255, elevation, y).is_none()));
        }
    }
}

/// The ease the duration of `ms` takes, stepped a frame at a time from `start`:
/// the levels it drew, in order.
fn eased(lift: &mut Lift, start: u64, ms: u16) -> Vec<u8> {
    let mut drawn = Vec::new();
    let mut now = start;
    while let Some(wait) = lift.next_frame_in(now, RATE) {
        now = now.saturating_add(wait.max(1));
        lift.step(now, ms);
        drawn.push(lift.drawn().0);
        assert!(drawn.len() < 64, "an ease that never settles");
    }
    drawn
}

#[test]
fn a_shadow_eases_to_its_new_level_and_then_asks_for_nothing() {
    let mut lift = Lift::at(Elevation::LIFTED);
    assert_eq!(
        lift.next_frame_in(0, RATE),
        None,
        "a shadow at rest owes no frame"
    );
    lift.aim(Elevation::RESTING, false);
    assert_eq!(
        lift.drawn(),
        Elevation::LIFTED,
        "nothing moves until a step"
    );
    assert_eq!(
        lift.next_frame_in(0, RATE),
        Some(0),
        "a change is owed a step at once"
    );
    let drawn = eased(&mut lift, 0, 120);
    assert!(drawn.len() > 2, "the ease takes several frames: {drawn:?}");
    assert!(drawn.windows(2).all(|pair| pair[0] >= pair[1]), "{drawn:?}");
    assert_eq!(drawn.last().copied(), Some(0));
    assert_eq!(lift.drawn(), Elevation::RESTING);

    // Turned back part-way, it rises from where it is drawn.
    lift.aim(Elevation::LIFTED, false);
    lift.step(1_000_000_000, 120);
    let from = lift.drawn().0;
    lift.step(1_000_000_000 + 60 * 1_000_000, 120);
    let midway = lift.drawn().0;
    lift.aim(Elevation::RESTING, false);
    lift.step(1_000_000_000 + 61 * 1_000_000, 120);
    assert!(lift.drawn().0 <= midway && lift.drawn().0 > from);
}

#[test]
fn reduced_motion_steps_a_shadow_straight_to_its_level() {
    let mut lift = Lift::at(Elevation::LIFTED);
    lift.aim(Elevation::RESTING, false);
    assert!(lift.step(5, 0), "the level drawn moved");
    assert_eq!(lift.drawn(), Elevation::RESTING);
    assert_eq!(
        lift.next_frame_in(5, RATE),
        None,
        "and no frame or timer is owed"
    );
}

#[test]
fn a_shadow_nobody_can_see_takes_its_level_at_once() {
    let mut lift = Lift::at(Elevation::RESTING);
    lift.aim(Elevation::LIFTED, true);
    assert_eq!(lift.drawn(), Elevation::LIFTED);
    assert_eq!(lift.next_frame_in(0, RATE), None);
}

/// An ease is paced by the shared frame cadence and the timeline's own end,
/// so it never asks for a frame sooner than one is worth drawing.
#[test]
fn an_ease_is_paced_by_the_frame_cadence() {
    let mut lift = Lift::at(Elevation::LIFTED);
    lift.aim(Elevation::RESTING, false);
    lift.step(0, 120);
    let wait = lift.next_frame_in(0, RATE).expect("easing");
    assert!(wait > 0 && wait <= RATE.frame_ns());
    assert_eq!(
        Fade::start(0, 120, 255, 0).next_frame_in(0, RATE),
        Some(wait),
        "the shared fade's cadence, not one of its own"
    );
}

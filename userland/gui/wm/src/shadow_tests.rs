//! The shadow kit's policy: the theme's two shadows at the output's scale, the
//! tiles it keeps, and the ease between the levels. What a shadow is, is
//! `tairix_raster`'s own and tested there.

use alloc::vec;
use alloc::vec::Vec;

use super::{shadow_footprint, Elevation, Lift, ShadowKit};
use crate::color::{Color, DitherRow, Pixel};
use crate::corner::Corners;
use crate::geometry::{Rect, Scale};
use crate::surface::Surface;
use crate::window::{Window, WindowId};
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

/// How far the shadow a window at `bounds` with painted round corners,
/// standing at `elevation`, darkens an opaque white screen at `(x, y)`: the
/// green it takes away.
fn darkening(kit: &ShadowKit, bounds: Rect, elevation: Elevation, (x, y): (i32, i32)) -> u8 {
    let surface = Surface::new(bounds.width, bounds.height).expect("a surface");
    let mut window = Window::new(WindowId(1), bounds.origin, surface);
    window.set_corners(Corners::painted(6));
    let white: Pixel = Color::rgb(255, 255, 255).premultiply();
    let mut screen = vec![white];
    if let Some(row) = kit.row(bounds, window.shape(), 255, elevation, y) {
        let _ = row.blend_into(&mut screen, x, DitherRow::at(y.cast_unsigned()));
    }
    255 - screen[0].g
}

#[test]
fn a_resting_window_casts_a_lighter_shorter_shadow() {
    let kit = kit();
    assert!(kit.resting.reach() < kit.lifted.reach());
    assert_eq!(
        kit.reach(),
        kit.lifted.reach(),
        "the footprint is the further one's"
    );
    let bounds = Rect::new(30, 12, 60, 40);
    let beneath = (
        i32::midpoint(bounds.left(), bounds.right()),
        bounds.bottom() + 1,
    );
    let (lifted, resting) = (
        darkening(&kit, bounds, Elevation::LIFTED, beneath),
        darkening(&kit, bounds, Elevation::RESTING, beneath),
    );
    assert!(resting < lifted, "{resting} is not lighter than {lifted}");
    assert!(resting > 0, "a resting window still casts one");
    for elevation in LEVELS {
        let above = (beneath.0, bounds.top() - 1);
        assert_eq!(
            darkening(&kit, bounds, elevation, above),
            0,
            "nothing above"
        );
    }
}

#[test]
fn a_tile_for_a_radius_nothing_rounds_by_is_not_kept() {
    let mut kit = kit();
    kit.ensure_tile(9);
    for level in [&kit.lifted, &kit.resting] {
        assert!(level.notch(9).is_some());
    }
    kit.retain_tiles(|radius| radius == 11);
    let theme = Theme::dark();
    for level in [&kit.lifted, &kit.resting] {
        assert!(level.notch(9).is_none(), "a radius out of use is dropped");
        for radius in [
            theme.metrics().window_corner_radius,
            theme.metrics().popup_corner_radius,
        ] {
            assert!(
                level.notch(radius).is_some(),
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

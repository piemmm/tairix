//! Host tests of the screensaver: what covers the screen, the pointer it
//! hides, the frames it asks for, and the display it switches off and wakes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::{Display, DisplayFormat, DisplayMode, DisplayPower};
use tairix_abi::time::Duration64;
use tairix_abi::DriverError;
use tairix_theme::Theme;
use tairix_wallpaper::{ScreensaverKind, ScreensaverOptions, SlideOrder};
use tairix_window::WallpaperName;
use tairix_wm::{Color, Compositor, Point, Surface};

use super::{
    SaverIdentity, SaverSetup, Scene, Screensaver, SwitchedOff, Waking, PREVIEW_STEADY_NS,
    SAVER_FRAME_NS,
};
use crate::tests::compositor;

const SEC: u64 = 1_000_000_000;

/// A panel that records the switches asked of it.
struct Panel {
    can_switch: bool,
    refuse: Option<DriverError>,
    switches: Vec<DisplayPower>,
}

impl Panel {
    fn switchable() -> Self {
        Self {
            can_switch: true,
            refuse: None,
            switches: Vec::new(),
        }
    }

    fn fixed() -> Self {
        Self {
            can_switch: false,
            ..Self::switchable()
        }
    }
}

impl Display for Panel {
    fn mode_info(&self) -> Result<DisplayMode, DriverError> {
        Ok(DisplayMode {
            width_px: 1920,
            height_px: 1080,
            stride_bytes: 1920 * 4,
            format: DisplayFormat::Rgba8888,
        })
    }

    fn present(&mut self, _frame: &[u8]) -> Result<(), DriverError> {
        Ok(())
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if !self.can_switch {
            return Err(DriverError::Unsupported);
        }
        if let Some(refusal) = self.refuse {
            return Err(refusal);
        }
        self.switches.push(power);
        Ok(())
    }
}

/// What a screensaver in these tests draws from.
struct Fixture {
    identity: SaverIdentity,
    theme: Theme,
    catalog: Vec<WallpaperName>,
    options: ScreensaverOptions,
}

impl Fixture {
    /// No pictures, and every scene as it comes.
    fn new() -> Self {
        Self::with_slides(0)
    }

    /// `count` pictures, shown in order ten seconds apart.
    fn with_slides(count: usize) -> Self {
        let mut options = ScreensaverOptions::default();
        options.slideshow.interval = Duration64::from_secs(10);
        options.slideshow.order = SlideOrder::Sequential;
        Self {
            identity: SaverIdentity::default(),
            theme: Theme::dark(),
            catalog: (0..count)
                .map(|at| WallpaperName {
                    category: String::from("Nature"),
                    file: format!("{at}.jpg"),
                })
                .collect(),
            options,
        }
    }

    fn setup(&self) -> SaverSetup<'_> {
        SaverSetup {
            ground: None,
            catalog: &self.catalog,
            wall: None,
            identity: &self.identity,
            theme: &self.theme,
            options: &self.options,
        }
    }
}

/// Start `kind` on `comp` at `now_ns`, asserting it covered the screen.
fn start(saver: &mut Screensaver, kind: ScreensaverKind, comp: &mut Compositor, now_ns: u64) {
    assert!(saver.start(kind, Fixture::new().setup(), comp, now_ns));
}

fn centre(compositor: &Compositor) -> Point {
    let screen = compositor.screen_rect();
    Point::new(
        screen.left() + i32::try_from(screen.width / 2).expect("small"),
        screen.top() + i32::try_from(screen.height / 2).expect("small"),
    )
}

#[test]
fn a_screensaver_covers_the_screen_until_it_is_dismissed() {
    let mut comp = compositor();
    let behind = comp.add_window(Point::new(0, 0), Surface::new(64, 64).expect("a surface"));
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Blank, &mut comp, 0);
    assert!(saver.is_shown());
    let over = comp.window_at(centre(&comp)).expect("something is on top");
    assert_ne!(over, behind);
    assert_eq!(comp.window_at(Point::new(1, 1)), Some(over), "every pixel");
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
    assert!(!saver.is_shown());
    assert_eq!(comp.window_at(Point::new(1, 1)), Some(behind));
    assert_eq!(
        saver.dismiss(&mut comp, None),
        Ok(false),
        "nothing left to dismiss"
    );
}

#[test]
fn every_kind_covers_the_screen_and_hides_the_pointer() {
    for kind in ScreensaverKind::ALL {
        let mut comp = compositor();
        crate::tests::shell().refresh_cursor(&mut comp);
        assert!(comp.cursor_bounds().is_some(), "a pointer to hide");
        let mut saver = Screensaver::new();
        start(&mut saver, kind, &mut comp, 0);
        assert!(saver.window().is_some(), "{kind:?}");
        assert_eq!(comp.cursor_bounds(), None, "{kind:?}: no pointer over it");
        assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
        assert!(
            comp.cursor_bounds().is_some(),
            "{kind:?}: the pointer is back"
        );
    }
}

#[test]
fn a_window_raised_behind_the_screensaver_goes_back_beneath_it() {
    let mut comp = compositor();
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Blank, &mut comp, 0);
    let saver_window = comp.window_at(Point::new(1, 1));
    let late = comp.add_window(Point::new(0, 0), Surface::new(64, 64).expect("a surface"));
    comp.raise(late);
    saver.keep_topmost(&mut comp);
    assert_eq!(comp.window_at(Point::new(1, 1)), saver_window);
}

#[test]
fn a_dimmed_screensaver_is_the_ground_it_was_given() {
    let mut comp = compositor();
    let screen = comp.screen_rect();
    let mut ground = Surface::new(screen.width, screen.height).expect("a ground");
    ground.fill(Color::rgb(200, 200, 200));
    let mut saver = Screensaver::new();
    let fixture = Fixture::new();
    let setup = SaverSetup {
        ground: Some(ground),
        ..fixture.setup()
    };
    assert!(saver.start(ScreensaverKind::Dim, setup, &mut comp, 0));
    assert!(saver.is_shown());
    assert_eq!(
        saver.park_deadline_ns(0, u64::MAX),
        u64::MAX,
        "nothing to advance"
    );
}

#[test]
fn a_slideshow_asks_for_each_picture_in_turn_one_interval_apart() {
    let mut comp = compositor();
    let mut saver = Screensaver::new();
    let fixture = Fixture::with_slides(3);
    assert!(saver.start(ScreensaverKind::Slideshow, fixture.setup(), &mut comp, 100));
    assert_eq!(saver.park_deadline_ns(0, u64::MAX), 100);
    assert_eq!(
        saver.due_slide(100),
        Some(0),
        "the first is asked for at once"
    );
    assert_eq!(saver.due_slide(100), None);
    assert_eq!(
        saver.park_deadline_ns(100, u64::MAX),
        10 * SEC,
        "parked until the next is due"
    );
    let later = 100 + 10 * SEC;
    assert_eq!(saver.due_slide(later), Some(1));
    assert_eq!(saver.due_slide(later + 10 * SEC), Some(2));
    assert_eq!(saver.due_slide(later + 20 * SEC), Some(0), "and round");
}

#[test]
fn a_blank_screensaver_or_an_empty_catalog_asks_for_no_picture() {
    let mut comp = compositor();
    let fixture = Fixture::with_slides(3);
    let mut blank = Screensaver::new();
    assert!(blank.start(ScreensaverKind::Blank, fixture.setup(), &mut comp, 0));
    assert_eq!(blank.due_slide(u64::MAX), None);
    let mut empty = Screensaver::new();
    start(&mut empty, ScreensaverKind::Slideshow, &mut comp, 0);
    assert_eq!(empty.due_slide(u64::MAX), None);
    assert_eq!(empty.park_deadline_ns(0, u64::MAX), u64::MAX);
}

/// The hand that pressed Test is still on the mouse: its motion leaves the
/// preview up for a moment, while a key, a press or a scroll never does.
#[test]
fn a_preview_holds_through_motion_for_its_first_moment_only() {
    let mut comp = compositor();
    let fixture = Fixture::new();
    let mut saver = Screensaver::new();
    assert!(saver.start_preview(ScreensaverKind::Blank, fixture.setup(), &mut comp, 100));
    assert!(!saver.woken_by(Waking::Moved, 100));
    assert!(!saver.woken_by(Waking::Moved, 100 + PREVIEW_STEADY_NS - 1));
    assert!(saver.woken_by(Waking::Moved, 100 + PREVIEW_STEADY_NS));
    assert!(saver.woken_by(Waking::Acted, 100), "a deliberate gesture");
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));

    start(&mut saver, ScreensaverKind::Blank, &mut comp, 100);
    assert!(
        saver.woken_by(Waking::Moved, 100),
        "a screensaver the idle deadline started keeps no grace"
    );
}

/// A preview asked for over a screensaver already up changes nothing: not
/// the scene, and not the rule that takes it down.
#[test]
fn a_preview_over_a_running_screensaver_leaves_it_as_it_is() {
    let mut comp = compositor();
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Blank, &mut comp, 0);
    let window = saver.window();
    assert!(saver.start_preview(ScreensaverKind::Life, Fixture::new().setup(), &mut comp, 0));
    assert_eq!(saver.window(), window);
    assert!(saver.woken_by(Waking::Moved, 0));
}

/// An animated screensaver draws a frame and asks for the next one a saver
/// frame later — never sooner, so it cannot outrun the frames it draws.
#[test]
fn an_animated_screensaver_asks_for_its_next_frame() {
    for kind in [
        ScreensaverKind::Starfield,
        ScreensaverKind::Life,
        ScreensaverKind::Raytrace,
    ] {
        let mut comp = compositor();
        let mut saver = Screensaver::new();
        start(&mut saver, kind, &mut comp, 0);
        assert_eq!(
            saver.park_deadline_ns(0, u64::MAX),
            0,
            "{kind:?}: due at once"
        );
        saver.advance(0, &mut comp, &mut || None, &mut || 0);
        assert_eq!(
            saver.park_deadline_ns(0, u64::MAX),
            SAVER_FRAME_NS,
            "{kind:?}"
        );
        saver.advance(SAVER_FRAME_NS / 2, &mut comp, &mut || None, &mut || 0);
        assert_eq!(
            saver.park_deadline_ns(SAVER_FRAME_NS / 2, u64::MAX),
            SAVER_FRAME_NS / 2,
            "{kind:?}: an early wake draws nothing and moves nothing"
        );
    }
}

/// The retro horizon is painted whole as it goes up, so its first frame is
/// already drawn and the next is asked for a saver frame later; under reduced
/// motion it asks for none.
#[test]
fn the_retro_horizon_goes_up_drawn_and_flies_on_a_saver_frame_later() {
    let mut comp = compositor();
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Horizon, &mut comp, 0);
    comp.composite();
    let sky = comp.frame()[..4].to_vec();
    assert_ne!(sky, [0, 0, 0, 255], "the night is painted, not black");
    assert_eq!(saver.park_deadline_ns(0, u64::MAX), SAVER_FRAME_NS);
    saver.advance(SAVER_FRAME_NS, &mut comp, &mut || None, &mut || 0);
    assert_eq!(
        saver.park_deadline_ns(SAVER_FRAME_NS, u64::MAX),
        SAVER_FRAME_NS
    );
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));

    let fixture = Fixture {
        theme: Theme::dark().with_axes(tairix_theme::Accessibility {
            motion: tairix_theme::Motion::Reduced,
            ..tairix_theme::Accessibility::default()
        }),
        ..Fixture::new()
    };
    assert!(saver.start(ScreensaverKind::Horizon, fixture.setup(), &mut comp, 0));
    assert_eq!(saver.park_deadline_ns(0, u64::MAX), u64::MAX, "held still");
}

#[test]
fn a_display_that_can_switch_off_goes_dark_and_wakes_before_the_screensaver_goes() {
    let mut comp = compositor();
    let mut panel = Panel::switchable();
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Starfield, &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Off)
    );
    assert!(saver.is_dark());
    assert!(
        saver
            .shown
            .as_ref()
            .is_some_and(|shown| matches!(shown.scene, Scene::Still)),
        "a dark screen lets its scene go"
    );
    assert_eq!(
        saver.park_deadline_ns(0, u64::MAX),
        u64::MAX,
        "a dark screen draws nothing and arms nothing"
    );
    assert_eq!(saver.switch_display_off(&mut comp, Some(&mut panel)), None);

    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert_eq!(panel.switches, [DisplayPower::Off, DisplayPower::On]);
    assert!(!saver.is_dark());
    assert!(!saver.is_shown());
}

/// A display with no power control is not left lit and moving: the screen
/// goes black and still, and no timer is armed for it.
#[test]
fn a_display_that_cannot_switch_off_is_kept_black_and_still() {
    let mut comp = compositor();
    let mut panel = Panel::fixed();
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Life, &mut comp, 0);
    saver.advance(0, &mut comp, &mut || None, &mut || 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Blanked)
    );
    assert!(!saver.is_dark(), "a lit screen is still presented");
    assert!(saver.is_shown());
    assert_eq!(saver.park_deadline_ns(0, u64::MAX), u64::MAX);
    comp.composite();
    assert_eq!(
        comp.frame()[..4],
        [0, 0, 0, 255],
        "black where the cells were"
    );
    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert!(panel.switches.is_empty(), "nothing to switch back on");
}

#[test]
fn a_refused_switch_is_stated_and_the_screen_kept_black() {
    let mut comp = compositor();
    let mut panel = Panel {
        refuse: Some(DriverError::DeviceFault),
        ..Panel::switchable()
    };
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Clock, &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Refused(DriverError::DeviceFault))
    );
    assert!(!saver.is_dark());
}

/// A display that will not light again keeps the screensaver up and dark:
/// presenting to a screen nobody can see helps nobody, and the next input
/// asks again.
#[test]
fn a_display_that_will_not_wake_keeps_the_screensaver_up_and_asks_again() {
    let mut comp = compositor();
    let mut panel = Panel::switchable();
    let mut saver = Screensaver::new();
    start(&mut saver, ScreensaverKind::Blank, &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Off)
    );
    panel.refuse = Some(DriverError::DeviceFault);
    assert_eq!(
        saver.dismiss(&mut comp, Some(&mut panel)),
        Err(DriverError::DeviceFault)
    );
    assert!(saver.is_shown());
    assert!(saver.is_dark());
    panel.refuse = None;
    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert!(!saver.is_shown());
}

/// The display is the screensaver's to wake even when its surface never came
/// up: a machine short of memory still sleeps its display, and still wakes it.
#[test]
fn a_display_asleep_without_a_surface_is_still_woken() {
    let mut comp = compositor();
    let mut panel = Panel::switchable();
    let mut saver = Screensaver::new();
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Off)
    );
    assert!(saver.is_shown(), "the next input only wakes the screen");
    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert_eq!(panel.switches, [DisplayPower::Off, DisplayPower::On]);
}

/// With no screensaver up and a display that will not switch off, the desktop
/// is left lit and live: the next input is the user's, not swallowed.
#[test]
fn a_refused_switch_without_a_surface_leaves_the_desktop_live() {
    let mut comp = compositor();
    for mut panel in [
        Panel::fixed(),
        Panel {
            refuse: Some(DriverError::DeviceFault),
            ..Panel::switchable()
        },
    ] {
        let mut saver = Screensaver::new();
        assert!(saver
            .switch_display_off(&mut comp, Some(&mut panel))
            .is_some());
        assert!(!saver.is_shown(), "input reaches the desktop");
        assert!(!saver.is_dark());
    }
}

#[test]
fn nothing_asks_for_a_slide_while_the_display_sleeps() {
    let mut comp = compositor();
    let mut saver = Screensaver::new();
    let fixture = Fixture::with_slides(2);
    assert!(saver.start(ScreensaverKind::Slideshow, fixture.setup(), &mut comp, 0));
    let mut panel = Panel::switchable();
    let _ = saver.switch_display_off(&mut comp, Some(&mut panel));
    assert_eq!(saver.due_slide(u64::MAX), None);
    assert_eq!(saver.park_deadline_ns(0, u64::MAX), u64::MAX);
}

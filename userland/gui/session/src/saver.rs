//! The screensaver: one surface over the whole screen once the desktop has
//! sat idle, gone at the first input — and, a set time after it starts, the
//! display behind it switched off.
//!
//! It covers every window and hides the pointer, so nothing on screen is
//! legible while it is up. It is not a lock: the first input only takes it
//! down, and reaches nothing else. A lock the idle policy engaged beneath it
//! is what the user meets when they come back.
//!
//! Switching the display off is asked of the display itself. One that has no
//! power control of its own is kept black and still instead, so the desktop
//! spends nothing on it either way; only a display that is genuinely off is
//! presented nothing at all.

mod clock;
mod life;
mod starfield;

use tairix_abi::driver::display::{Display, DisplayPower};
use tairix_abi::time::WallClockReading;
use tairix_abi::DriverError;
use tairix_theme::{Theme, Timeline};
use tairix_wallpaper::ScreensaverKind;
use tairix_wm::{Color, Compositor, Surface, WindowId};

use crate::switchuser::park_within;

pub use clock::SaverIdentity;

use clock::ClockFace;
use life::Life;
use starfield::Starfield;

/// How long a slideshow shows one picture.
pub const SLIDE_INTERVAL_NS: u64 = 30_000_000_000;

/// How often an animated screensaver draws: every other frame the desktop
/// would. Each star draws the whole path it travelled over the frame, so the
/// motion reads as continuous at half the work.
pub const SAVER_FRAME_NS: u64 = 2 * Timeline::FRAME_NS;

/// How dark a dimmed screensaver lays black over the backdrop, out of 255:
/// enough that nothing reads as an invitation to click, not so much that the
/// picture is lost.
const DIM_ALPHA: u8 = 176;

/// What a screensaver may draw from, gathered by the embedder as one starts.
pub struct SaverSetup<'a> {
    /// The desktop's own backdrop at the screen's size, which the dimmed
    /// screensaver darkens; without one it is black.
    pub ground: Option<Surface>,
    /// How many pictures a slideshow can ask for; with none it stays black.
    pub slides: usize,
    /// The wall clock as the screensaver starts, which the clock tells.
    pub wall: Option<WallClockReading>,
    /// Who is signed in, and where, as the clock names them.
    pub identity: &'a SaverIdentity,
    /// The look in force: the clock's type, and whether motion is reduced.
    pub theme: &'a Theme,
}

/// What a screensaver draws, and what it needs to keep drawing.
enum Scene {
    /// Drawn once: black, or the dimmed backdrop.
    Still,
    /// The shipped pictures in turn.
    Slideshow {
        /// The catalog position shown next.
        next_slide: usize,
        /// Monotonic nanoseconds of the next picture, or `None` when there
        /// is none to show.
        due_ns: Option<u64>,
    },
    Clock(ClockFace),
    Starfield(Starfield),
    Life(Life),
}

/// One screensaver on the screen.
struct Shown {
    wm: WindowId,
    kind: ScreensaverKind,
    size: (u32, u32),
    scene: Scene,
}

/// How the display behind the screensaver sleeps, if it does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Sleep {
    Awake,
    /// Switched off: nothing presented can be seen until it wakes.
    Off,
    /// A display that would not switch off, its screensaver kept black and
    /// still instead.
    Blanked,
}

/// What asking the display to switch off came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SwitchedOff {
    /// The display is off.
    Off,
    /// The display has no power control; the screensaver is kept black and
    /// still in its place.
    Blanked,
    /// The display refused for this reason; the screensaver is kept black and
    /// still in its place.
    Refused(DriverError),
}

/// The session's screensaver.
pub struct Screensaver {
    shown: Option<Shown>,
    sleep: Sleep,
}

impl Default for Screensaver {
    fn default() -> Self {
        Self::new()
    }
}

impl Screensaver {
    /// No screensaver up.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            shown: None,
            sleep: Sleep::Awake,
        }
    }

    /// Whether the screensaver has the screen: its surface is up, or the
    /// display behind it is asleep.
    ///
    /// While it does, the embedder drains the seat's next input into nothing
    /// and takes the screensaver down: the gesture that wakes the screen acts
    /// on nothing behind it.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown.is_some() || self.sleep != Sleep::Awake
    }

    /// Whether the display is switched off, so the embedder presents
    /// nothing: no frame it sends can be seen.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.sleep == Sleep::Off
    }

    /// Cover the screen with `kind` at monotonic `now_ns`, and hide the
    /// pointer.
    ///
    /// Answers whether the screen is covered: a surface the heap would not
    /// give covers nothing, and the desktop stays as it was.
    pub fn start(
        &mut self,
        kind: ScreensaverKind,
        setup: SaverSetup<'_>,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> bool {
        if self.shown.is_some() {
            return true;
        }
        let screen = compositor.screen_rect();
        let size = (screen.width, screen.height);
        let scale = compositor.scale();
        let calm = setup.theme.motion().reduced_motion();
        let frame = match (kind, setup.ground) {
            (ScreensaverKind::Dim, Some(mut ground)) => {
                let (width, height) = (ground.width(), ground.height());
                // Composited over the ground: a plain fill would replace it.
                ground.fill_round_rect(0, 0, width, height, 0, Color::rgba(0, 0, 0, DIM_ALPHA));
                Some(ground)
            }
            _ => black(size),
        };
        let Some(mut frame) = frame else {
            return false;
        };
        let scene = match kind {
            ScreensaverKind::Blank | ScreensaverKind::Dim => Scene::Still,
            ScreensaverKind::Slideshow => Scene::Slideshow {
                next_slide: 0,
                due_ns: (setup.slides > 0).then_some(now_ns),
            },
            ScreensaverKind::Clock => {
                let face = ClockFace::new(
                    setup.identity,
                    setup.theme,
                    scale,
                    size,
                    (setup.wall, now_ns),
                );
                face.paint(&mut frame);
                Scene::Clock(face)
            }
            // A scene the heap will not give is a black screen instead: the
            // screen is still covered, which is what a screensaver owes.
            ScreensaverKind::Starfield => {
                Starfield::new(size, scale, calm, now_ns).map_or(Scene::Still, Scene::Starfield)
            }
            ScreensaverKind::Life => {
                Life::new(size, scale, calm, now_ns).map_or(Scene::Still, Scene::Life)
            }
        };
        let wm = compositor.add_window(screen.origin, frame);
        compositor.raise(wm);
        compositor.set_cursor_hidden(true);
        self.shown = Some(Shown {
            wm,
            kind,
            size,
            scene,
        });
        true
    }

    /// Take the screensaver down and wake the display behind it, answering
    /// whether anything was up.
    ///
    /// The pointer comes back where the device put it meanwhile, since the
    /// drain that woke the screen followed it.
    ///
    /// # Errors
    ///
    /// The display's refusal to switch back on. The screensaver then stays
    /// up and dark, so the next input asks again rather than presenting to
    /// a screen nobody can see. With no `display` to ask — the session has
    /// handed the screen to another — the display service lights it for
    /// whoever owns it next.
    pub fn dismiss(
        &mut self,
        compositor: &mut Compositor,
        display: Option<&mut dyn Display>,
    ) -> Result<bool, DriverError> {
        if self.sleep == Sleep::Off {
            if let Some(display) = display {
                display.set_power(DisplayPower::On)?;
            }
        }
        let was_up = self.is_shown();
        self.sleep = Sleep::Awake;
        if let Some(shown) = self.shown.take() {
            let _ = compositor.remove(shown.wm);
            let _ = compositor.set_cursor_hidden(false);
        }
        Ok(was_up)
    }

    /// Switch the display behind the screensaver off, or keep the
    /// screensaver black and still where it cannot be, answering what came
    /// of it; `None` when it already sleeps.
    ///
    /// A screensaver the heap would not give still switches the display off:
    /// that needs no memory. With none up and the display refusing, the
    /// desktop stays as it is and takes input as it did.
    pub fn switch_display_off(
        &mut self,
        compositor: &mut Compositor,
        display: Option<&mut dyn Display>,
    ) -> Option<SwitchedOff> {
        if self.sleep != Sleep::Awake {
            return None;
        }
        let answer = display.map_or(Err(DriverError::Unsupported), |display| {
            display.set_power(DisplayPower::Off)
        });
        let refusal = match answer {
            Ok(()) => {
                self.sleep = Sleep::Off;
                return Some(SwitchedOff::Off);
            }
            Err(refusal) => refusal,
        };
        if self.shown.is_some() {
            self.sleep = Sleep::Blanked;
            self.blank(compositor);
        }
        Some(match refusal {
            DriverError::Unsupported | DriverError::NotImplemented => SwitchedOff::Blanked,
            refusal => SwitchedOff::Refused(refusal),
        })
    }

    /// Lay black over whatever the screensaver shows and stop it moving.
    fn blank(&mut self, compositor: &mut Compositor) {
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        shown.scene = Scene::Still;
        if shown.kind != ScreensaverKind::Blank {
            if let Some(frame) = black(shown.size) {
                let _ = compositor.set_surface(shown.wm, frame);
            }
        }
    }

    /// The screensaver's window while it is up, which the lock keeps
    /// directly beneath.
    #[must_use]
    pub fn window(&self) -> Option<WindowId> {
        self.shown.as_ref().map(|shown| shown.wm)
    }

    /// Raise the screensaver over everything, the lock included, so a window
    /// opened or raised behind it cannot surface over it.
    pub fn keep_topmost(&self, compositor: &mut Compositor) {
        if let Some(shown) = self.shown.as_ref() {
            let _ = compositor.raise(shown.wm);
        }
    }

    /// Step whatever the screensaver animates to `now_ns`, drawing the frame
    /// that is due, if one is. `wall` reads the wall clock, and is asked only
    /// when the clock's minute has turned.
    pub fn advance(
        &mut self,
        now_ns: u64,
        compositor: &mut Compositor,
        wall: &mut dyn FnMut() -> Option<WallClockReading>,
    ) {
        if self.sleep != Sleep::Awake {
            return;
        }
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        match &mut shown.scene {
            Scene::Clock(face) => face.advance(now_ns, shown.wm, compositor, wall),
            Scene::Starfield(field) => field.advance(now_ns, shown.wm, compositor),
            Scene::Life(life) => life.advance(now_ns, shown.wm, compositor),
            Scene::Still | Scene::Slideshow { .. } => {}
        }
    }

    /// The catalog position a slideshow wants shown at `now_ns`, if its next
    /// picture is due; the one after it is due a slide interval later. None
    /// is due while the display sleeps.
    pub fn due_slide(&mut self, now_ns: u64, slides: usize) -> Option<usize> {
        if self.sleep != Sleep::Awake {
            return None;
        }
        let Scene::Slideshow { next_slide, due_ns } = &mut self.shown.as_mut()?.scene else {
            return None;
        };
        let due = (*due_ns)?;
        if now_ns < due || slides == 0 {
            return None;
        }
        let index = *next_slide % slides;
        *next_slide = (index + 1) % slides;
        *due_ns = Some(now_ns.saturating_add(SLIDE_INTERVAL_NS));
        Some(index)
    }

    /// Show a prepared slide, if a slideshow is still up and awake to show
    /// it.
    pub fn show_slide(&mut self, frame: Surface, compositor: &mut Compositor) {
        if self.sleep != Sleep::Awake {
            return;
        }
        if let Some(shown) = self
            .shown
            .as_ref()
            .filter(|shown| matches!(shown.scene, Scene::Slideshow { .. }))
        {
            let _ = compositor.set_surface(shown.wm, frame);
        }
    }

    /// `park_ns` shortened to the screensaver's next frame, picture, or
    /// minute, or left as it is: nothing is due while the display sleeps.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        if self.sleep != Sleep::Awake {
            return park_ns;
        }
        let due = self.shown.as_ref().and_then(|shown| match &shown.scene {
            Scene::Still => None,
            Scene::Slideshow { due_ns, .. } => *due_ns,
            Scene::Clock(face) => Some(face.due_ns()),
            Scene::Starfield(field) => Some(field.due_ns()),
            Scene::Life(life) => Some(life.due_ns()),
        });
        park_within(park_ns, due.map(|due| due.saturating_sub(now_ns)))
    }
}

/// A black surface of `size`, or `None` when the heap will not give one.
fn black(size: (u32, u32)) -> Option<Surface> {
    let mut surface = Surface::new(size.0, size.1)?;
    surface.fill(Color::rgb(0, 0, 0));
    Some(surface)
}

/// A seed for a screensaver's scatter: the start instant, which differs every
/// time one starts. Visual only, so nothing is owed to its unpredictability.
fn seed_from(now_ns: u64) -> u64 {
    now_ns ^ 0x5CEE_7A11_D15C_0DE5
}

/// `whole` nanoseconds as seconds.
#[allow(clippy::cast_precision_loss)] // A monotonic span; microsecond precision is ample.
fn seconds(whole: u64) -> f64 {
    whole as f64 / 1e9
}

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;

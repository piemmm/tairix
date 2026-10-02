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
//!
//! A screensaver can also be shown on request, as a preview. Then the first
//! moment of pointer motion does not take it down — the hand that asked for
//! it is still on the mouse — while a key, a press or a scroll always does.

mod clock;
mod life;
pub mod raytrace;
mod retro_games;
mod ribbon;
mod slides;
mod starfield;
mod system_monitor;
mod telling;

#[cfg(test)]
pub(crate) use system_monitor::fixtures as monitor_fixtures;

use tairix_abi::driver::display::Display;
use tairix_abi::switchboard_ipc::MachineReport;
use tairix_abi::time::WallClockReading;
use tairix_abi::DriverError;
use tairix_display::{DisplaySleep, SwitchedOff};
use tairix_raytrace::Detail;
use tairix_theme::Theme;
use tairix_wallpaper::{ScreensaverKind, ScreensaverOptions};
use tairix_window::WallpaperName;
use tairix_wm::{Color, Compositor, Surface, WindowId};

use crate::switchuser::park_within;

pub use clock::SaverIdentity;

use clock::ClockFace;
use life::Life;
use raytrace::{Memory, Raytrace, TraceHost};
use retro_games::RetroGames;
use ribbon::Ribbon;
use slides::Slides;
use starfield::Starfield;
use system_monitor::SystemMonitor;

/// How long a preview keeps the screen through pointer motion alone: long
/// enough for the hand that pressed its button to come to rest.
pub const PREVIEW_STEADY_NS: u64 = 1_500_000_000;

/// How dark a dimmed screensaver lays black over the backdrop, out of 255:
/// enough that nothing reads as an invitation to click, not so much that the
/// picture is lost.
const DIM_ALPHA: u8 = 176;

/// What a screensaver may draw from, gathered by the embedder as one starts.
pub struct SaverSetup<'a> {
    /// The desktop's own backdrop at the screen's size, which the dimmed
    /// screensaver darkens; without one it is black.
    pub ground: Option<Surface>,
    /// The shipped pictures a slideshow draws from; with none it stays black.
    pub catalog: &'a [WallpaperName],
    /// The wall clock as the screensaver starts, which a kind that
    /// [tells the time](tells_time) tells.
    pub wall: Option<WallClockReading>,
    /// Who is signed in, and where, as the clock names them.
    pub identity: &'a SaverIdentity,
    /// The look in force: the clock's type, and whether motion is reduced.
    pub theme: &'a Theme,
    /// How each scene draws.
    pub options: &'a ScreensaverOptions,
    /// The threads the ray tracer may trace on; with none, it traces on the
    /// serve loop.
    pub tracers: Option<&'a dyn TraceHost>,
    /// What the machine's memory can spare a ray-traced scene.
    pub memory: Memory,
    /// Told the detail the ray tracer's scenes are set out at, whenever the
    /// memory changes it from the last scene's.
    pub tell_detail: fn(Detail),
}

/// What a screensaver draws, and what it needs to keep drawing.
#[allow(
    clippy::large_enum_variant,
    reason = "one scene is held at a time, so the room its largest variant sets is \
              paid once; boxing it would trade that for an allocation that cannot fail \
              gracefully"
)]
enum Scene {
    /// Drawn once: black, or the dimmed backdrop.
    Still,
    /// The shipped pictures in turn.
    Slideshow(Slides),
    Clock(ClockFace),
    Ribbon(Ribbon),
    Starfield(Starfield),
    Life(Life),
    Raytrace(Raytrace),
    RetroGames(RetroGames),
    SystemMonitor(SystemMonitor),
}

impl Scene {
    /// Take down whatever the scene shows beside the screensaver's own
    /// window, before it goes.
    fn take_down(&mut self, compositor: &mut Compositor) {
        if let Self::Raytrace(tracer) = self {
            tracer.take_down(compositor);
        }
    }
}

/// What woke the screen behind a screensaver.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Waking {
    /// The pointer moved, and nothing else happened.
    Moved,
    /// A key, a press or a scroll: a gesture nobody makes by accident.
    Acted,
}

/// One screensaver on the screen.
struct Shown {
    wm: WindowId,
    kind: ScreensaverKind,
    size: (u32, u32),
    scene: Scene,
}

/// The session's screensaver.
pub struct Screensaver {
    shown: Option<Shown>,
    /// How the display behind it sleeps, if it does.
    sleep: DisplaySleep,
    /// Until when pointer motion alone leaves a preview up.
    steady_until_ns: Option<u64>,
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
            sleep: DisplaySleep::new(),
            steady_until_ns: None,
        }
    }

    /// [`start`](Self::start) `kind` as a preview: pointer motion in its first
    /// [`PREVIEW_STEADY_NS`] leaves it up.
    ///
    /// A screensaver already up is left as it is, and so is its waking rule.
    pub fn start_preview(
        &mut self,
        kind: ScreensaverKind,
        setup: SaverSetup<'_>,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> bool {
        if self.shown.is_some() {
            return true;
        }
        let started = self.start(kind, setup, compositor, now_ns);
        if started {
            self.steady_until_ns = Some(now_ns.saturating_add(PREVIEW_STEADY_NS));
        }
        started
    }

    /// Whether `waking` at `now_ns` takes the screensaver down: always, but for
    /// pointer motion in a preview's first moment.
    #[must_use]
    pub fn woken_by(&self, waking: Waking, now_ns: u64) -> bool {
        waking == Waking::Acted || self.steady_until_ns.is_none_or(|until| now_ns >= until)
    }

    /// Whether the screensaver has the screen: its surface is up, or the
    /// display behind it is asleep.
    ///
    /// While it does, the embedder drains the seat's next input into nothing
    /// and takes the screensaver down: the gesture that wakes the screen acts
    /// on nothing behind it.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown.is_some() || !self.sleep.is_awake()
    }

    /// Whether the display is switched off, so the embedder presents
    /// nothing: no frame it sends can be seen.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.sleep.is_off()
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
        let options = setup.options;
        // A scene the heap will not give is a black screen instead: the
        // screen is still covered, which is what a screensaver owes.
        let scene = match kind {
            ScreensaverKind::Blank | ScreensaverKind::Dim => Scene::Still,
            ScreensaverKind::Slideshow => Slides::new(setup.catalog, &options.slideshow, now_ns)
                .map_or(Scene::Still, Scene::Slideshow),
            ScreensaverKind::Clock => {
                let face = ClockFace::new(
                    setup.identity,
                    setup.theme,
                    (scale, size),
                    (setup.wall, now_ns),
                    options.clock,
                );
                face.paint(&mut frame);
                Scene::Clock(face)
            }
            ScreensaverKind::Ribbon => Ribbon::new(
                setup.theme,
                (scale, size),
                (setup.wall, now_ns),
                (calm, options.ribbon),
            )
            .map_or(Scene::Still, |mut face| {
                face.paint(&mut frame);
                Scene::Ribbon(face)
            }),
            ScreensaverKind::Starfield => {
                Starfield::new(size, scale, (calm, options.starfield), now_ns)
                    .map_or(Scene::Still, Scene::Starfield)
            }
            ScreensaverKind::Life => Life::new(size, scale, (calm, options.life), now_ns)
                .map_or(Scene::Still, Scene::Life),
            ScreensaverKind::Raytrace => Raytrace::new(
                size,
                (calm, now_ns),
                (options.raytrace, setup.memory, setup.tell_detail),
                setup.tracers,
                (setup.theme, scale),
            )
            .map_or(Scene::Still, Scene::Raytrace),
            ScreensaverKind::RetroGames => {
                RetroGames::new(size, scale, (calm, options.retro_games), now_ns).map_or(
                    Scene::Still,
                    |mut scene| {
                        scene.paint(&mut frame, compositor.job_runner());
                        Scene::RetroGames(scene)
                    },
                )
            }
            ScreensaverKind::SystemMonitor => SystemMonitor::new(
                setup.identity,
                setup.theme,
                size,
                (setup.wall, now_ns),
                options.system_monitor,
            )
            .map_or(Scene::Still, |board| {
                board.paint(&mut frame);
                Scene::SystemMonitor(board)
            }),
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
        let was_up = self.is_shown();
        self.sleep.wake(display)?;
        self.steady_until_ns = None;
        if let Some(mut shown) = self.shown.take() {
            shown.scene.take_down(compositor);
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
        let answer = self.sleep.switch_off(display, self.shown.is_some())?;
        match answer {
            // Nothing moves on a dark display and only input ends it, so the
            // scene goes now, and all it holds with it.
            SwitchedOff::Off => {
                if let Some(shown) = self.shown.as_mut() {
                    shown.scene.take_down(compositor);
                    shown.scene = Scene::Still;
                }
            }
            SwitchedOff::Blanked | SwitchedOff::Refused(_) => self.blank(compositor),
        }
        Some(answer)
    }

    /// Lay black over whatever the screensaver shows and stop it moving.
    fn blank(&mut self, compositor: &mut Compositor) {
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        shown.scene.take_down(compositor);
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
    /// when the clock's minute has turned; `clock` reads the monotonic clock,
    /// for a scene that measures how much of its frame its work took.
    pub fn advance(
        &mut self,
        now_ns: u64,
        compositor: &mut Compositor,
        wall: &mut dyn FnMut() -> Option<WallClockReading>,
        clock: &mut dyn FnMut() -> u64,
    ) {
        if !self.sleep.is_awake() {
            return;
        }
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        match &mut shown.scene {
            Scene::Clock(face) => face.advance(now_ns, shown.wm, compositor, wall),
            Scene::Ribbon(face) => face.advance(now_ns, shown.wm, compositor, wall),
            Scene::Starfield(field) => field.advance(now_ns, shown.wm, compositor),
            Scene::Life(life) => life.advance(now_ns, shown.wm, compositor),
            Scene::Raytrace(tracer) => tracer.advance(now_ns, shown.wm, compositor, clock),
            Scene::RetroGames(games) => games.advance(now_ns, shown.wm, compositor),
            Scene::SystemMonitor(board) => board.advance(now_ns, shown.wm, compositor, wall),
            Scene::Still | Scene::Slideshow(_) => {}
        }
    }

    /// Whether a System Monitor is up on an awake display, and so wants the
    /// machine reports it draws.
    #[must_use]
    pub fn wants_machine_reports(&self) -> bool {
        self.sleep.is_awake()
            && self
                .shown
                .as_ref()
                .is_some_and(|shown| matches!(shown.scene, Scene::SystemMonitor(_)))
    }

    /// Hand the System Monitor `report`, which came at `now_ns`, answering
    /// whether one was up to draw it.
    pub fn adopt_machine_report(
        &mut self,
        report: MachineReport,
        now_ns: u64,
        compositor: &mut Compositor,
    ) -> bool {
        if !self.wants_machine_reports() {
            return false;
        }
        let Some(Shown {
            wm,
            scene: Scene::SystemMonitor(board),
            ..
        }) = self.shown.as_mut()
        else {
            return false;
        };
        board.adopt(report, now_ns, *wm, compositor);
        true
    }

    /// The monitor that reads the machine is not running: a System Monitor
    /// up says so rather than passing its last readings off as live.
    pub fn machine_unmonitored(&mut self, compositor: &mut Compositor) {
        if let Some(Shown {
            wm,
            scene: Scene::SystemMonitor(board),
            ..
        }) = self.shown.as_mut()
        {
            board.unmonitored(*wm, compositor);
        }
    }

    /// The catalog position a slideshow wants shown at `now_ns`, if its next
    /// picture is due; the one after it is due an interval later. None is due
    /// while the display sleeps.
    pub fn due_slide(&mut self, now_ns: u64) -> Option<usize> {
        if !self.sleep.is_awake() {
            return None;
        }
        let Scene::Slideshow(slides) = &mut self.shown.as_mut()?.scene else {
            return None;
        };
        slides.take_due(now_ns)
    }

    /// The session's workers woke the loop at `now_ns`, as a ray tracer's
    /// thread does on readying a scene: a ray tracer up and revealing comes
    /// back at once, to show the scene's first passes as they come.
    pub fn trace_landed(&mut self, now_ns: u64) {
        if let Some(Shown {
            scene: Scene::Raytrace(tracer),
            ..
        }) = self.shown.as_mut()
        {
            tracer.landed(now_ns);
        }
    }

    /// Show a prepared slide, if a slideshow is still up and awake to show
    /// it.
    pub fn show_slide(&mut self, frame: Surface, compositor: &mut Compositor) {
        if !self.sleep.is_awake() {
            return;
        }
        if let Some(shown) = self
            .shown
            .as_ref()
            .filter(|shown| matches!(shown.scene, Scene::Slideshow(_)))
        {
            let _ = compositor.set_surface(shown.wm, frame);
        }
    }

    /// `park_ns` shortened to the screensaver's next frame, picture, or
    /// minute, or left as it is: nothing is due while the display sleeps.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        if !self.sleep.is_awake() {
            return park_ns;
        }
        let due = self.shown.as_ref().and_then(|shown| match &shown.scene {
            Scene::Still => None,
            Scene::Slideshow(slides) => slides.due_ns(),
            Scene::Clock(face) => Some(face.due_ns()),
            Scene::Ribbon(face) => Some(face.due_ns()),
            Scene::Starfield(field) => Some(field.due_ns()),
            Scene::Life(life) => Some(life.due_ns()),
            Scene::Raytrace(tracer) => Some(tracer.due_ns()),
            Scene::RetroGames(games) => Some(games.due_ns()),
            Scene::SystemMonitor(board) => Some(board.due_ns()),
        });
        park_within(park_ns, due.map(|due| due.saturating_sub(now_ns)))
    }
}

/// Whether the screensaver `kind` tells the time, and so is started with a
/// reading of the wall clock.
#[must_use]
pub const fn tells_time(kind: ScreensaverKind) -> bool {
    matches!(
        kind,
        ScreensaverKind::Clock | ScreensaverKind::Ribbon | ScreensaverKind::SystemMonitor
    )
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

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;

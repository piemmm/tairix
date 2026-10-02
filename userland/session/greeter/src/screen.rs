//! The login screen: the surface, the ribbon of light behind it, the frame
//! they go into, the lockout it presents, the authority behind it, and the
//! sleep its display is put into when nobody is there.
//!
//! Everything about *what the screen does* lives here, so the whole flow —
//! a keystroke reaching a verdict, a refusal becoming a countdown, a screen
//! left alone going dark and waking again — is exercised on the host. What
//! the `Run` binary adds is only where the events and the pixels come from.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::Display;
use tairix_abi::input::{PointerButtonCode, PointerInput};
use tairix_abi::time::{Duration64, Time64};
use tairix_abi::touch::TouchFrame;
use tairix_abi::window_ipc::PointerAction;
use tairix_abi::{DriverError, WAITSET_TIMEOUT_NONE};
use tairix_cursor::{CursorImage, PlacedCursor};
use tairix_display::{DisplaySleep, SwitchedOff};
use tairix_geometry::{Point, Rect, Scale};
use tairix_greeter::{AccountTile, AuthSurface, Backdrop, EventContext, Outcome};
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::Surface;
use tairix_ribbon::SKY;
use tairix_theme::{MotionInteraction, Theme};
use tairix_touch::{Gesture, Recogniser, SurfacePoint, TouchPress, TouchSettings};
use tairix_window::pointer_input_events;

use crate::accounts::SessionTransport;
use crate::chrome::Teller;
use crate::cursor::Cursor;
use crate::frame::{Present, Scanout};
use crate::scene::Scene;
use crate::verify::{Answer, SessionVerifier};
use crate::wait::{frame_budget, park_timeout, Cooldown, Idle};

/// What one round of the screen did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Step {
    /// What to hand the display, if anything.
    pub present: Present,
    /// Whether a secret was verified. The screen is finished: the authority
    /// is watching for the exit and starts the session itself.
    pub verified: bool,
    /// The authority's answer, when one came back this round.
    pub answer: Option<Answer>,
}

impl Step {
    /// Nothing happened.
    const fn quiet() -> Self {
        Self {
            present: Present::Nothing,
            verified: false,
            answer: None,
        }
    }
}

/// The graphical login screen.
///
/// It draws and types; it never decides. Every question about a secret goes
/// out over `session-v1` and comes back as one of three answers, and only a
/// verified one finishes the screen. A refusal, an unreachable authority, an
/// empty account list, and a lockout running out are all "still asking" —
/// none of them exits, so a transient fault cannot spend the authority's
/// restart budget.
pub struct LoginScreen<T: SessionTransport> {
    surface: AuthSurface,
    /// The accounts the screen offers, which it comes back to rest on.
    accounts: Vec<AccountTile>,
    scanout: Scanout,
    cooldown: Cooldown,
    verifier: SessionVerifier<T>,
    theme: Theme,
    scale: Scale,
    teller: Teller,
    /// The ribbon of light behind the column, once one has been raised.
    scene: Option<Scene>,
    cursor: Cursor,
    /// The pointer artwork, once one has been installed. A screen whose
    /// cursor would not rasterise keeps hit-testing and typing with nothing
    /// drawn, which is a missing pointer rather than a broken login.
    pointer: Option<PlacedCursor>,
    /// The surface as last rendered, with no cursor drawn into it and nothing
    /// of the ribbon beneath it.
    ///
    /// Everything the render reads — the account tiles, the field, the
    /// chrome, the lockout, the backdrop — changes only through the surface
    /// reporting it or a ribbon being raised, and both drop this. A pointer
    /// sliding across an unchanged screen, or the ribbon moving behind it,
    /// therefore re-composes pixels that already exist instead of building a
    /// whole screen for every report the seat delivers or every frame the
    /// ribbon draws.
    ///
    /// The buffer itself is kept across frames and repainted in place: every
    /// pixel is written on each paint, so an animated frame reuses it rather
    /// than mapping, zeroing and unmapping a screenful to draw the picture one
    /// step on.
    painted: Option<Surface>,
    /// Whether [`painted`](Self::painted) must be painted again before the next
    /// blit. Set by everything the paint reads changing, cleared by the paint.
    paint_owed: bool,
    /// When the screen last saw input, which is when its display is put to
    /// sleep from.
    idle: Idle,
    sleep: DisplaySleep,
    /// Whether input has reached the screen while its display slept, so the
    /// display is owed a wake.
    wake_owed: bool,
    /// What the seat's touch frames mean. No user is signed in to have chosen
    /// otherwise, so a touch means what it means by default.
    touch: Recogniser,
}

/// One pointing action, from a mouse report or a touch gesture.
#[derive(Copy, Clone, Debug)]
enum Pointing {
    By(i32, i32),
    To(SurfacePoint),
    Pressed(PointerButtonCode),
    Released(PointerButtonCode),
    Nothing,
}

/// A repaint request: what changed, and which pixels it changed.
///
/// Two updates can land in one round — a keystroke and the lockout that
/// answered it, or a move and the tile it moved onto — and each is reported
/// separately, so they are combined here. A whole-screen surface change from
/// either side stays whole: one part of a paint changing everything is not
/// narrowed by another changing a rectangle.
///
/// A pointer that only moved is kept apart from a surface that changed: the
/// pixels it moves over are already rendered, so only the frame composed
/// from them is redone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Repaint {
    /// Nothing changed, so nothing is painted.
    Nothing,
    /// The painted surface stands and only the bytes blitted from it changed,
    /// over these pixels — `None` for all of them. The pointer moved over
    /// them, or the veil closed further over them.
    Scanout(Option<Rect>),
    /// The surface's own content changed, within this rectangle — `None` for
    /// all of it.
    Painted(Option<Rect>),
}

impl Repaint {
    fn of(outcome: Outcome) -> Self {
        if !outcome.redraw() {
            return Self::Nothing;
        }
        if outcome.paints() {
            return Self::Painted(outcome.damage());
        }
        Self::Scanout(outcome.damage())
    }

    /// The wider of two reports, and the stronger: a round that must paint
    /// paints, over everything either half of it changed.
    fn merged(self, other: Self) -> Self {
        match (self, other) {
            (Self::Nothing, repaint) | (repaint, Self::Nothing) => repaint,
            (Self::Scanout(mine), Self::Scanout(theirs)) => Self::Scanout(wider(mine, theirs)),
            (Self::Painted(mine), Self::Painted(theirs) | Self::Scanout(theirs))
            | (Self::Scanout(mine), Self::Painted(theirs)) => Self::Painted(wider(mine, theirs)),
        }
    }
}

/// The rectangle covering both, `None` — the whole screen — swallowing whatever
/// it is merged with.
fn wider(one: Option<Rect>, other: Option<Rect>) -> Option<Rect> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.union(&other)),
        _ => None,
    }
}

impl<T: SessionTransport> LoginScreen<T> {
    /// A screen offering `accounts`, painted into `scanout`, naming the
    /// machine by `identity`.
    ///
    /// An empty list is not an error: the chooser always carries its
    /// typed-name tile, so a machine whose account directory could not be
    /// read is still one a user can log into.
    pub fn new(
        scanout: Scanout,
        theme: Theme,
        scale: Scale,
        identity: String,
        accounts: Vec<AccountTile>,
        transport: T,
    ) -> Self {
        let cursor = Cursor::centred(scanout.mode());
        let mut touch = Recogniser::new(TouchSettings::DEFAULT);
        let mode = scanout.mode();
        touch.set_screen(mode.width_px, mode.height_px, scale.dpi());
        Self {
            surface: AuthSurface::with_accounts(accounts.clone()),
            accounts,
            scanout,
            cooldown: Cooldown::default(),
            verifier: SessionVerifier::new(transport),
            theme,
            scale,
            teller: Teller::new(identity),
            scene: None,
            cursor,
            pointer: None,
            painted: None,
            paint_owed: true,
            idle: Idle::new(0),
            sleep: DisplaySleep::new(),
            wake_owed: false,
            touch,
        }
    }

    /// Draw the ribbon of light behind the column from `now_ns`, kept clear of
    /// the column itself, answering whether the heap would give it.
    ///
    /// A screen with no ribbon keeps the theme's flat desktop colour behind
    /// the column. Under reduced motion the ribbon holds still.
    pub fn raise_ribbon(&mut self, now_ns: u64) -> bool {
        let screen = self.scanout.screen();
        let clear = self.surface.column_rect(screen, self.scale);
        let still = self.theme.motion().reduced_motion();
        self.scene = Scene::new(screen, clear, now_ns, still);
        self.paint_owed = true;
        self.scene.is_some()
    }

    /// Draw `image` as the pointer, from where the pointer already is.
    ///
    /// Called once at start-up with the arrow rasterised for the active
    /// scale. Until it is, the pointer moves and hit-tests but nothing is
    /// drawn for it.
    pub fn set_pointer(&mut self, image: CursorImage) {
        self.pointer = Some(PlacedCursor::new(image, self.cursor.at()));
    }

    /// The rectangle the screen covers.
    #[must_use]
    pub const fn screen(&self) -> Rect {
        self.scanout.screen()
    }

    /// The line currently shown under the field.
    #[must_use]
    pub fn notice(&self) -> &str {
        self.surface.notice()
    }

    /// The frame's bytes, for the present call.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        self.scanout.frame()
    }

    /// Compose the whole screen and present all of it.
    ///
    /// Used for the first frame and after anything that changes more than one
    /// part of the surface.
    pub fn repaint(&mut self) -> Present {
        self.compose(None)
    }

    /// Put the chrome up for `wall` and begin the fade the screen arrives out
    /// of, answering the frame to open on: full black, or — for a theme that
    /// fades instantly — the screen itself.
    ///
    /// The screen has seen no input yet, so its display is put to sleep a
    /// full wait from `now_ns`.
    pub fn open(&mut self, now_ns: u64, wall: Option<Time64>) -> Present {
        self.idle = Idle::new(now_ns);
        if let Some(chrome) = self.teller.tell(wall) {
            let _ = self.surface.set_chrome(chrome);
        }
        match self.begin_entry_fade(now_ns) {
            Present::Nothing => self.repaint(),
            veiled => veiled,
        }
    }

    /// Apply one input event.
    ///
    /// The verdict a submitted secret produced is applied before this
    /// returns: a refusal's lockout starts counting from `now_ns` and is
    /// already on screen in the frame this round presents.
    ///
    /// While the display sleeps the event reaches nothing: it only owes the
    /// display its wake.
    pub fn on_input(&mut self, event: &InputEvent, now_ns: u64) -> Step {
        self.idle.input(now_ns);
        if !self.sleep.is_awake() {
            self.wake_owed = true;
            return Step::quiet();
        }
        let round = self.apply(event, now_ns);
        Step {
            present: self.present_for(round.repaint),
            verified: round.verified,
            answer: round.answer,
        }
    }

    /// Apply one pointer report from the seat.
    ///
    /// The report is relative motion or a button, so the running position is
    /// kept here and the surface is given the absolute events it hit-tests.
    /// One report expands to as many as two of them, and they present
    /// together: a press is a move and a press, not two frames.
    ///
    /// A move also repaints the pointer itself — the union of where it was
    /// and where it now is — so no cursor is left painted behind. Motion
    /// that lands on the same pixel moves nothing and paints nothing.
    ///
    /// While the display sleeps a report reaches nothing and owes the display
    /// its wake, though motion still carries the pointer: it comes back where
    /// the hand put it.
    pub fn on_pointer(&mut self, input: &PointerInput, now_ns: u64) -> Step {
        let action = match *input {
            PointerInput::MovedBy { dx, dy } => Pointing::By(dx, dy),
            PointerInput::Pressed(button) => Pointing::Pressed(button),
            PointerInput::Released(button) => Pointing::Released(button),
            // The authentication surface has nothing scrollable.
            PointerInput::Scrolled { .. } => Pointing::Nothing,
        };
        self.point(action, now_ns)
    }

    /// Read one touch frame, adding what it meant to `gestures` for
    /// [`on_gesture`](Self::on_gesture) to answer.
    pub fn feed_touch(&mut self, frame: &TouchFrame, gestures: &mut Vec<Gesture>) {
        self.touch
            .feed(frame, &mut |gesture| gestures.push(gesture));
    }

    /// Add what a touch meant by waiting until `now_ns` to `gestures`; run
    /// once every queued frame has been fed.
    pub fn expire_touch(&mut self, now_ns: u64, gestures: &mut Vec<Gesture>) {
        self.touch
            .expire(now_ns, &mut |gesture| gestures.push(gesture));
    }

    /// Answer one touch gesture, as [`on_pointer`](Self::on_pointer) answers
    /// a pointer report: a click the fingers made is the button it names.
    pub fn on_gesture(&mut self, gesture: Gesture, now_ns: u64) -> Step {
        let button = |press: TouchPress| match press {
            TouchPress::Device(code) => code,
            TouchPress::Fingers(PointerButton::Primary) => PointerButtonCode::Primary,
            TouchPress::Fingers(PointerButton::Secondary) => PointerButtonCode::Secondary,
            TouchPress::Fingers(PointerButton::Middle) => PointerButtonCode::Middle,
        };
        let action = match gesture {
            Gesture::MovedBy { dx, dy } => Pointing::By(dx, dy),
            Gesture::MovedTo(place) => Pointing::To(place),
            Gesture::Pressed(press) => Pointing::Pressed(button(press)),
            Gesture::Released(press) => Pointing::Released(button(press)),
            // Nothing on the authentication surface scrolls or zooms.
            Gesture::Scrolled { .. } | Gesture::Pinch(_) => Pointing::Nothing,
        };
        self.point(action, now_ns)
    }

    /// One pointing action through the surface. While the display sleeps it
    /// reaches nothing and owes the display its wake, though motion still
    /// carries the pointer.
    fn point(&mut self, action: Pointing, now_ns: u64) -> Step {
        self.idle.input(now_ns);
        let moved = match action {
            Pointing::By(dx, dy) => self.move_pointer(dx, dy),
            Pointing::To(place) => self.place_pointer(place),
            Pointing::Pressed(_) | Pointing::Released(_) | Pointing::Nothing => None,
        };
        if !self.sleep.is_awake() {
            self.wake_owed = true;
            return Step::quiet();
        }
        let action = match action {
            Pointing::By(..) | Pointing::To(_) => PointerAction::Moved,
            Pointing::Pressed(button) => PointerAction::Pressed(button),
            Pointing::Released(button) => PointerAction::Released(button),
            Pointing::Nothing => return Step::quiet(),
        };
        let mut repaint = moved.map_or(Repaint::Nothing, |damage| Repaint::Scanout(Some(damage)));
        let mut verified = false;
        let mut answer = None;
        for event in pointer_input_events(action, self.cursor.at()) {
            let round = self.apply(&event, now_ns);
            repaint = repaint.merged(round.repaint);
            answer = round.answer.or(answer);
            if round.verified {
                verified = true;
                break;
            }
        }
        Step {
            present: self.present_for(repaint),
            verified,
            answer,
        }
    }

    /// Bring the clock, the lockout, the surface's animations and the ribbon
    /// up to date.
    ///
    /// Called on every round the loop wakes for. Nothing repaints unless one
    /// of them actually changed, so a wake that finds nothing to do presents
    /// nothing — and nothing at all is presented while the display sleeps.
    pub fn refresh(&mut self, now_ns: u64, wall: Option<Time64>) -> Step {
        if !self.sleep.is_awake() {
            return Step::quiet();
        }
        let clock = self.teller.tell(wall).map_or(Repaint::Nothing, |chrome| {
            Repaint::of(self.surface.set_chrome(chrome))
        });
        let remaining = self
            .cooldown
            .remaining(now_ns, self.surface.selected_account());
        let cooldown = Repaint::of(self.surface.set_cooldown(remaining));
        let motion = Repaint::of(self.surface.advance(now_ns));
        // The ribbon moves first, so a round that repaints the surface too
        // composes it over the ribbon as it now stands.
        let moved = self
            .scene
            .as_mut()
            .is_some_and(|scene| scene.advance(now_ns));
        let surface = self.present_for(clock.merged(cooldown).merged(motion));
        let ribbon = if moved && surface != Present::Whole {
            self.compose_ribbon()
        } else {
            Present::Nothing
        };
        Step {
            present: surface.merged(ribbon, self.scanout.mode()),
            ..Step::quiet()
        }
    }

    /// The relative nanosecond timeout for the next park.
    ///
    /// The nearest of the clock's next minute, a lockout's next tick, the
    /// next frame of whatever the surface or the ribbon is animating, and the
    /// moment the display is owed its sleep. While the display sleeps there is
    /// none: nothing it shows can change, and only input wakes it.
    #[must_use]
    pub fn park_timeout(&self, now_ns: u64, wall: Option<Time64>) -> u64 {
        if !self.sleep.is_awake() {
            return WAITSET_TIMEOUT_NONE;
        }
        let remaining = self
            .cooldown
            .remaining(now_ns, self.surface.selected_account());
        [
            self.surface.motion_due(now_ns),
            self.scene.as_ref().and_then(|scene| scene.due_in(now_ns)),
            Some(self.idle.timeout(now_ns)),
            self.touch
                .deadline_ns()
                .map(|due| due.saturating_sub(now_ns)),
        ]
        .into_iter()
        .flatten()
        .fold(park_timeout(wall, remaining), u64::min)
    }

    /// Put `display` to sleep once the screen has been left alone for the
    /// energy-saving wait, answering what came of it; `None` while that wait
    /// has not passed, or once the display already sleeps.
    ///
    /// The screen goes back to rest first — the chooser as it first came up,
    /// with whatever was typed erased — and then goes black, and the display
    /// is asked to switch off. One that cannot is left showing that black.
    /// Either way nothing more is presented, and no timer is armed, until
    /// input wakes it.
    pub fn sleep_if_idle(&mut self, display: &mut dyn Display, now_ns: u64) -> Option<SwitchedOff> {
        if !self.sleep.is_awake() || !self.idle.is_due(now_ns) {
            return None;
        }
        self.rest();
        let _ = self.scanout.blacken();
        let _ = display.present(self.scanout.frame());
        self.sleep.switch_off(Some(display), true)
    }

    /// Whether the display is asleep.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn is_asleep(&self) -> bool {
        !self.sleep.is_awake()
    }

    /// Wake `display` for the input that reached the screen while it slept,
    /// answering the frame to present: the screen arriving out of black, as it
    /// first did. A screen that has seen no such input presents nothing.
    ///
    /// # Errors
    ///
    /// The display's refusal to switch back on. It stays asleep, and the next
    /// input asks again.
    pub fn wake(
        &mut self,
        display: &mut dyn Display,
        now_ns: u64,
        wall: Option<Time64>,
    ) -> Result<Present, DriverError> {
        if !core::mem::take(&mut self.wake_owed) || !self.sleep.wake(Some(display))? {
            return Ok(Present::Nothing);
        }
        Ok(self.open(now_ns, wall))
    }

    /// Begin the fade the screen arrives out of, and present its first frame.
    ///
    /// Called before the opening present, so the first frame the display is
    /// handed is full black and the login screen appears out of it. That
    /// black is what the seat was handed over cleared to — and at first boot
    /// it covers the text console's pixels in one step instead of replacing
    /// them with a chooser. The screen answers input and draws its pointer
    /// throughout: it is arriving, not leaving.
    ///
    /// [`Present::Nothing`] when the theme fades instantly: there is nothing
    /// to cover, so the opening frame is the screen itself.
    pub fn begin_entry_fade(&mut self, now_ns: u64) -> Present {
        let outcome = self.surface.begin_entry_fade(now_ns, &self.theme);
        self.present_for(Repaint::of(outcome))
    }

    /// Begin the fade to black the screen leaves through, and present its
    /// first frame.
    ///
    /// Called once a secret has been accepted. The desktop cannot appear
    /// until this process exits, so the screen goes black *before* it does
    /// and the desktop comes up out of the same black — that is what makes
    /// the handover read as one movement rather than two screens swapping.
    /// The surface stops answering input from here on.
    ///
    /// [`Present::Nothing`] when the theme fades instantly: there is no frame
    /// worth showing, so the caller leaves at once.
    pub fn begin_session_fade(&mut self, now_ns: u64) -> Present {
        let outcome = self.surface.begin_session_fade(now_ns, &self.theme);
        if self.surface.session_fade_finished() {
            return Present::Nothing;
        }
        self.present_for(Repaint::of(outcome))
    }

    /// Whether the screen has finished going black, so its owner may leave.
    #[must_use]
    pub fn session_fade_finished(&self) -> bool {
        self.surface.session_fade_finished()
    }

    /// Nanoseconds until the fade's next frame, or `None` once it is over.
    #[must_use]
    pub fn session_fade_due(&self, now_ns: u64) -> Option<u64> {
        if self.surface.session_fade_finished() {
            return None;
        }
        self.surface.motion_due(now_ns)
    }

    /// Darken the fade to `now_ns` and present what changed.
    pub fn session_fade_step(&mut self, now_ns: u64) -> Present {
        let darkened = Repaint::of(self.surface.advance(now_ns));
        self.present_for(darkened)
    }

    /// The most frames the fade can ever ask for.
    ///
    /// What bounds the loop that presents it: a stopped clock or a seat that
    /// reads ready forever must not be able to strand a successful login on
    /// a screen that never finishes leaving.
    #[must_use]
    pub fn session_fade_budget(&self) -> u32 {
        frame_budget(self.theme.motion().duration(MotionInteraction::SessionFade))
    }

    /// Return the screen to rest as it first came up: the chooser with nothing
    /// typed into it and no lockout shown. The surface it replaces erases its
    /// secret as it goes.
    fn rest(&mut self) {
        self.surface = AuthSurface::with_accounts(self.accounts.clone());
        self.cooldown = Cooldown::default();
        self.teller.forget();
        self.paint_owed = true;
    }

    /// Move the pointer by `(dx, dy)` and report the pixels that owe a
    /// repaint: where the cursor was, unioned with where it now is, clipped
    /// to the screen. `None` when the pointer did not move, when there is no
    /// cursor drawn to move, or once the screen is leaving and nothing is
    /// drawn for it — the position is still tracked either way.
    fn move_pointer(&mut self, dx: i32, dy: i32) -> Option<Rect> {
        let was = self.cursor.at();
        let at = self.cursor.moved_by(dx, dy);
        self.repoint(was, at)
    }

    fn place_pointer(&mut self, place: SurfacePoint) -> Option<Rect> {
        let was = self.cursor.at();
        let at = self.cursor.placed(place);
        self.repoint(was, at)
    }

    /// Draw the pointer at `at`, moved from `was`, answering the pixels that
    /// changed.
    fn repoint(&mut self, was: Point, at: Point) -> Option<Rect> {
        let screen = self.scanout.screen();
        let drawn = self.draws_pointer();
        if at == was {
            return None;
        }
        let pointer = self.pointer.as_mut()?;
        let vacated = pointer.bounds();
        pointer.set_pointer(at);
        if !drawn {
            return None;
        }
        let damage = vacated.union(&pointer.bounds()).intersection(&screen);
        (!damage.is_empty()).then_some(damage)
    }

    /// Whether the pointer is drawn over the frame at all.
    ///
    /// It stops the moment the screen begins leaving: a pointer is something
    /// to point *with*, and the verdict is given, input is no longer
    /// answered, and there is nothing left under the black to point at. It
    /// goes with the screen it belonged to rather than staying bright over
    /// it.
    fn draws_pointer(&self) -> bool {
        !self.surface.session_fade_begun()
    }

    /// One event through the surface, with any verdict it produced applied.
    fn apply(&mut self, event: &InputEvent, now_ns: u64) -> Round {
        let outcome = {
            let mut ctx = EventContext {
                screen: self.scanout.screen(),
                scale: self.scale,
                theme: &self.theme,
                verifier: &mut self.verifier,
                now_ns,
            };
            self.surface.on_event(event, &mut ctx)
        };
        let answer = self.verifier.take_answer();
        let mut repaint = Repaint::of(outcome);
        if let Some(answer) = answer {
            // The surface still asks about the account it offered the secret
            // for, so that is whose lockout this answer reports.
            if let Some(account) = self.surface.selected_account() {
                self.cooldown.start(now_ns, answer.retry_after, account);
            }
            if answer.retry_after > Duration64::ZERO {
                repaint =
                    repaint.merged(Repaint::of(self.surface.set_cooldown(answer.retry_after)));
            }
        }
        Round {
            repaint,
            verified: outcome.verified(),
            answer,
        }
    }

    /// Present what `repaint` changed, or nothing when it changed nothing.
    fn present_for(&mut self, repaint: Repaint) -> Present {
        match repaint {
            Repaint::Nothing => Present::Nothing,
            Repaint::Scanout(damage) => self.compose(damage),
            Repaint::Painted(damage) => {
                self.paint_owed = true;
                self.compose(damage)
            }
        }
    }

    /// Copy `damage` of the painted surface into the frame over the ribbon,
    /// with the pointer over both, rendering the surface first when nothing
    /// holds it.
    ///
    /// A screen that is leaving hands the composer no pointer, so the first
    /// veiled frame — which covers the whole screen — is also the one that
    /// paints the arrow out.
    fn compose(&mut self, damage: Option<Rect>) -> Present {
        if self.paint_owed {
            self.paint();
        }
        let drawn = self.draws_pointer();
        // The veil is applied as the surface is blitted, so it is read here
        // rather than painted in: a fade step re-blits what is already painted.
        let reveal = self.surface.reveal();
        let Self {
            scanout,
            pointer,
            painted,
            scene,
            ..
        } = self;
        let Some(painted) = painted.as_ref() else {
            return Present::Nothing;
        };
        let cursor = if drawn { pointer.as_ref() } else { None };
        let ground = scene.as_ref().map(Scene::layer);
        scanout.compose(painted, ground, cursor, damage, reveal)
    }

    /// Compose the pixels the ribbon's last frame moved, with the surface
    /// over them as it stands and the pointer over both.
    fn compose_ribbon(&mut self) -> Present {
        if self.paint_owed {
            self.paint();
        }
        let drawn = self.draws_pointer();
        let reveal = self.surface.reveal();
        let Self {
            scanout,
            pointer,
            painted,
            scene,
            ..
        } = self;
        let (Some(painted), Some(scene)) = (painted.as_ref(), scene.as_ref()) else {
            return Present::Nothing;
        };
        let cursor = if drawn { pointer.as_ref() } else { None };
        let mode = *scanout.mode();
        scene
            .damage()
            .rects()
            .iter()
            .fold(Present::Nothing, |present, strip| {
                let strip =
                    scanout.compose(painted, Some(scene.layer()), cursor, Some(*strip), reveal);
                present.merged(strip, &mode)
            })
    }

    /// Paint the surface, with no cursor drawn into it, into the retained
    /// buffer — allocating that buffer on the first frame, and again whenever
    /// the screen's extent has changed under it.
    ///
    /// Over the ribbon the surface is left transparent behind the column and
    /// every line carries a shadow in the ribbon's sky; with none the theme's
    /// flat desktop colour stands behind it.
    ///
    /// A refused allocation or a refused paint leaves the frame already on
    /// screen rather than blanking it, and leaves the paint owed so the next
    /// round tries again.
    fn paint(&mut self) {
        let screen = self.scanout.screen();
        let fits = self
            .painted
            .as_ref()
            .is_some_and(|held| held.width() == screen.width && held.height() == screen.height);
        if !fits {
            self.painted = Surface::new(screen.width, screen.height);
        }
        let Self {
            surface,
            painted: Some(into),
            scene,
            scale,
            theme,
            ..
        } = self
        else {
            return;
        };
        let backdrop = match scene {
            Some(_) => Backdrop::Scene { ground: SKY },
            None => Backdrop::Desktop,
        };
        let painted = surface.paint_into(into, screen, *scale, theme, backdrop);
        self.paint_owed = !painted;
    }
}

/// What one event through the surface did.
struct Round {
    repaint: Repaint,
    verified: bool,
    answer: Option<Answer>,
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;

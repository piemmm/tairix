//! The desktop session's **screen lock**: the shared authentication surface
//! (`lib/greeter`) held over the whole display, taking every input event
//! until the signed-in user proves they are still the one at the keyboard.
//!
//! Locking is the one way out of a session that keeps the session. Everything
//! carries on running behind the lock — a build finishes, a download
//! completes, an editor keeps its unsaved buffer — but nothing on screen is
//! legible and no keystroke or click reaches any of it.
//!
//! # What makes it a lock rather than a window
//!
//! The surface asks for a password and concludes only on a verified one;
//! that much is `tairix_greeter`'s. What this module adds is everything that
//! makes asking *unavoidable*, and none of it can be done from inside a
//! surface:
//!
//! * **It covers the screen.** The window is the compositor's full extent
//!   and fully opaque, so no pixel of the session shows through. Someone
//!   walking past a locked machine learns nothing about what is on it. The
//!   screen can change size under it — a resume onto another account's mode,
//!   an output arriving or leaving — so the lock redraws itself over the
//!   screen now drawn before every composite, and one it cannot redraw is
//!   [`uncovered`](ScreenLock::uncovered): the embedder then presents
//!   nothing at all.
//! * **It takes every event.** While locked, the embedder drains the pointer
//!   and keyboard into [`handle`](ScreenLock::handle) and routes *nothing*
//!   onward — not to the window manager, not to the taskbar, not to a served
//!   application. That is the embedder's contract, stated on
//!   [`is_locked`](ScreenLock::is_locked); a window cannot enforce it from
//!   the inside.
//! * **It stays on top.** [`keep_topmost`](ScreenLock::keep_topmost) raises
//!   it before every composite, beneath only the screensaver, so an
//!   application that opens or raises a window behind the lock cannot
//!   surface above it.
//!
//! # Who decides it may open again
//!
//! The lock holds no authority to authenticate anybody. The desktop's
//! [`Verifier`] is the per-console elevation broker — the login supervisor
//! that started this session — which attests the caller's identity from the
//! kernel, checks the password against *that* uid, and audits the decision.
//! It therefore ignores the account name the surface offers it: the only
//! identity it will ever check is this process's own. A refusal, a transport
//! failure, a broker that is not there, and a reply it cannot parse are all
//! the same answer: still locked.
//!
//! The broker is asked and never awaited: it derives a key from every
//! password on purpose, and the serve loop it would stall composites every
//! window on the screen. Its answer arrives later through
//! [`answer`](ScreenLock::answer), and until then the lock says it is
//! checking and takes no keystroke.

use alloc::string::{String, ToString};

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::Duration64;
use tairix_controls::damage::{self, Repaint};
use tairix_greeter::{
    AuthSurface, Backdrop, Cooldown, EventContext, Outcome, Ticket, Verdict, Verifier,
};
use tairix_util::secret::WipedBuf;
use tairix_wm::{Compositor, InputEvent, Point, Rect, Scale, Surface, WindowId};

use crate::shell::DesktopShell;
use crate::switchuser::park_within;

/// One password the lock asks the broker about, on its way off the serve
/// loop: held in a buffer sized to it so it never moves, and erased once the
/// broker has answered and again when it is dropped — carried, displaced by a
/// newer offer, or never sent at all.
pub struct UnlockAsk {
    ticket: Ticket,
    password: WipedBuf,
}

impl UnlockAsk {
    /// The question `ticket` names, about `password`, or `None` when no
    /// memory holds it.
    #[must_use]
    pub fn new(ticket: Ticket, password: &str) -> Option<Self> {
        let mut held = WipedBuf::with_capacity(password.len()).ok()?;
        held.extend_zeroed(password.len())?
            .copy_from_slice(password.as_bytes());
        Some(Self {
            ticket,
            password: held,
        })
    }

    /// The offer this question answers for.
    #[must_use]
    pub const fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The password, as the text it was offered as.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        core::str::from_utf8(&self.password).ok()
    }

    /// Erase the password now, rather than when the question is dropped.
    pub fn erase(&mut self) {
        self.password.wipe();
    }
}

/// The broker's answer to one offer at the lock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UnlockAnswer {
    /// The offer it is about.
    pub ticket: Ticket,
    /// What the broker said of it.
    pub verdict: Verdict,
    /// How long the account must wait before another password is checked;
    /// zero for none.
    pub retry_after: Duration64,
}

impl UnlockAnswer {
    /// `verdict` on the offer `ticket` names, with no wait.
    #[must_use]
    pub const fn of(ticket: Ticket, verdict: Verdict) -> Self {
        Self {
            ticket,
            verdict,
            retry_after: Duration64::ZERO,
        }
    }
}

/// What one input event did to the lock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LockOutcome {
    /// Nothing for the embedder to act on. The screen is still locked.
    Pending,
    /// The user proved their identity. The lock is already down — its window
    /// closed and the password erased — and the desktop is reachable again.
    Unlocked,
}

/// One engaged lock: its full-screen window, the surface drawn into it, the
/// screen and look it was drawn in, what of its kept frame is stale, and
/// whether the compositor refused the last attempt to draw it.
struct Engaged {
    wm: WindowId,
    surface: AuthSurface,
    /// The account the lock asks about, which its lockout is metered under.
    login: String,
    /// The broker's lockout on that account, counted down for the surface.
    cooldown: Cooldown,
    screen: Rect,
    style: u64,
    owed: Repaint,
    refused: bool,
}

impl Engaged {
    /// What is left of the account's lockout at `now_ns`.
    fn lockout(&self, now_ns: u64) -> Duration64 {
        self.cooldown.remaining(now_ns, Some(&self.login))
    }

    /// Show what is left of the lockout at `now_ns`, owing what that changed.
    fn show_lockout(&mut self, now_ns: u64) {
        let shown = self.surface.set_cooldown(self.lockout(now_ns));
        self.owe(shown);
    }

    /// Fold what `outcome` changed into what the frame owes. A step that
    /// moves only the strength a surface is blitted at owes nothing here:
    /// the lock is always shown at full strength.
    fn owe(&mut self, outcome: Outcome) {
        if !outcome.paints() {
            return;
        }
        match outcome.damage() {
            Some(rect) => self.owed.add(rect),
            None => self.owed = Repaint::Whole,
        }
    }
}

/// The session's screen lock.
///
/// Idle until [`engage`](ScreenLock::engage) puts it up, then holding the
/// screen until the user is verified or the session is torn down.
#[derive(Default)]
pub struct ScreenLock {
    engaged: Option<Engaged>,
}

impl ScreenLock {
    /// An idle lock.
    #[must_use]
    pub const fn new() -> Self {
        Self { engaged: None }
    }

    /// Whether the screen is locked right now.
    ///
    /// While this is `true` the embedder MUST drain every pointer and
    /// keyboard event into [`handle`](Self::handle) and route none of them to
    /// the window manager, the taskbar, or any served window. That routing is
    /// what makes this a lock; the surface only hides the session.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.engaged.is_some()
    }

    /// Lock the screen, naming the account as `shown` so the user knows whose
    /// password is wanted, and offering `login` to the broker. An empty
    /// `shown` name heads the prompt with the surface's placeholder.
    ///
    /// The prompt reads and marks the account exactly as the login screen
    /// did, because it is the same surface asking the same person; the broker
    /// ignores the offered name and checks this process's own attested
    /// identity regardless.
    ///
    /// Returns whether the screen is now locked. A lock already up answers
    /// `true` and changes nothing. A surface the compositor cannot give
    /// answers `false` and locks nothing: a lock that cannot cover the screen
    /// is worse than no lock, because the user would walk away believing they
    /// were protected.
    pub fn engage(
        &mut self,
        (login, shown): (&str, &str),
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if self.engaged.is_some() {
            return true;
        }
        let screen = compositor.screen_rect();
        let surface = AuthSurface::new(login, shown);
        let Some(frame) = render_frame(&surface, screen, compositor.scale(), shell) else {
            return false;
        };
        let wm = compositor.add_window(screen.origin, frame);
        compositor.raise(wm);
        self.engaged = Some(Engaged {
            wm,
            surface,
            login: login.to_string(),
            cooldown: Cooldown::default(),
            screen,
            style: shell.style_generation(),
            owed: Repaint::clean(),
            refused: false,
        });
        true
    }

    /// Raise the lock above everything but `above` — the screensaver, which
    /// hides the lock until the user comes back — so a window opened or
    /// raised behind it cannot surface over it, redrawing it first over a
    /// screen that is not the one it covers.
    ///
    /// The embedder calls this immediately before each composite while
    /// locked, after the screensaver has been kept on top. A lock already
    /// where it belongs restacks and repaints nothing, and it does nothing at
    /// all when the screen is not locked. An `above` the compositor does not
    /// hold leaves the lock on top.
    pub fn keep_topmost(
        &mut self,
        shell: &DesktopShell,
        compositor: &mut Compositor,
        above: Option<WindowId>,
    ) {
        if self.uncovered(compositor) {
            self.repaint(shell, compositor);
        }
        if let Some(engaged) = self.engaged.as_ref() {
            let beneath = above.is_some_and(|anchor| compositor.stack_below(engaged.wm, anchor));
            if !beneath {
                let _ = compositor.raise(engaged.wm);
            }
        }
    }

    /// Apply one input event — pointer or key — to the lock, at monotonic
    /// `now_ns`.
    ///
    /// Keys edit the password; `Enter` asks `verifier` about it, and the
    /// broker's answer comes back through [`answer`](Self::answer). The
    /// pointer places the caret and selects within the field, and reaches
    /// nothing else on screen; following the device with the cursor is the
    /// drain's. There is no key and no click that dismisses the lock.
    ///
    /// What the event changed is only owed to the frame: the next
    /// [`advance`](Self::advance) draws it, once however many events owed it.
    pub fn handle(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        verifier: &mut dyn Verifier,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) {
        let screen = compositor.screen_rect();
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let Some(engaged) = self.engaged.as_mut() else {
            return;
        };
        let outcome = engaged.surface.on_event(
            &to_window_space(event, screen),
            &mut EventContext {
                screen: window_bounds(screen),
                scale,
                theme,
                verifier,
                now_ns,
            },
        );
        engaged.owe(outcome);
    }

    /// Adopt the broker's `answer`, at monotonic `now_ns`.
    ///
    /// Returns [`LockOutcome::Unlocked`] only for a verified answer to the
    /// offer the lock is waiting on, by which time the lock is already down.
    /// An answer to an offer a lock since abandoned asked, or to none at all,
    /// changes nothing on the field. A lockout it reports counts down from
    /// now whichever offer it answered, since it is the account's: the field
    /// says how long is left and submits nothing until it has run out.
    pub fn answer(
        &mut self,
        answer: UnlockAnswer,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> LockOutcome {
        let theme = shell.session().active_theme();
        let Some(engaged) = self.engaged.as_mut() else {
            return LockOutcome::Pending;
        };
        let outcome = engaged
            .surface
            .answer(answer.ticket, answer.verdict, now_ns, theme);
        if outcome.verified() {
            self.release(compositor);
            return LockOutcome::Unlocked;
        }
        engaged.owe(outcome);
        engaged
            .cooldown
            .start(now_ns, answer.retry_after, &engaged.login);
        engaged.show_lockout(now_ns);
        LockOutcome::Pending
    }

    /// Step the locked surface's motion to `now_ns`, then draw what the frame
    /// owes — that step's change and every event's since the last frame —
    /// into the lock's kept frame, marking only the pixels that changed.
    ///
    /// The embedder calls this once a frame while locked, and parks until
    /// [`park_deadline_ns`](Self::park_deadline_ns). The surface is the same
    /// engine the login screen animates, so the lock animates with it rather
    /// than freezing whatever it was mid-way through. An unlocked screen, and
    /// a surface with nothing in flight and nothing owed, do nothing here.
    pub fn advance(&mut self, now_ns: u64, shell: &DesktopShell, compositor: &mut Compositor) {
        let Some(engaged) = self.engaged.as_mut() else {
            return;
        };
        engaged.show_lockout(now_ns);
        let stepped = engaged.surface.advance(now_ns);
        engaged.owe(stepped);
        self.draw(shell, compositor);
    }

    /// `park_ns` shortened to the locked surface's next animation frame on a
    /// display refreshing at `refresh` or the next second of a lockout's
    /// countdown, or to now while an event's change is still owed — the frame
    /// pacer then holds the wake to the next frame — or left exactly as it is
    /// when the screen is unlocked and nothing is in flight: an idle lock
    /// screen arms no timer.
    ///
    /// A frame the compositor refused to give back is owed too, but arms
    /// nothing: nothing is presented while it stands, so a wake of its own
    /// would spin the loop against an exhausted heap. The loop's next wake
    /// retries it.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64, refresh: RefreshRate) -> u64 {
        park_within(
            park_ns,
            self.engaged.as_ref().and_then(|engaged| {
                if engaged.owed.is_clean() || engaged.refused {
                    let tick = Cooldown::tick_of(engaged.lockout(now_ns));
                    engaged
                        .surface
                        .motion_due(now_ns, refresh)
                        .into_iter()
                        .chain(tick)
                        .min()
                } else {
                    Some(0)
                }
            }),
        )
    }

    /// Take the lock down without verifying anybody.
    ///
    /// Used when the session is tearing the desktop down. It unlocks nothing:
    /// the session is ending, and the login prompt that replaces it asks for
    /// a password of its own. The password goes with the surface — the masked
    /// field zeroises its buffer as it is dropped — so an abandoned prompt
    /// leaves no plaintext behind either.
    pub fn abandon(&mut self, compositor: &mut Compositor) {
        if let Some(engaged) = self.engaged.take() {
            let _ = compositor.remove(engaged.wm);
        }
    }

    /// Whether the screen is locked behind a frame drawn for a screen other
    /// than the one the compositor draws now: one the lock could not redraw.
    ///
    /// While this is `true` the embedder MUST present nothing, because part
    /// of the session lies outside what the lock covers.
    #[must_use]
    pub fn uncovered(&self, compositor: &Compositor) -> bool {
        self.engaged
            .as_ref()
            .is_some_and(|engaged| engaged.screen != compositor.screen_rect())
    }

    /// Repaint the whole lock now, so a resolution change behind it redraws it
    /// at the right size.
    ///
    /// A surface that cannot be built leaves the previous frame up rather
    /// than uncovering the session.
    pub fn repaint(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        let Some(engaged) = self.engaged.as_mut() else {
            return;
        };
        engaged.owed = Repaint::Whole;
        self.draw(shell, compositor);
        if let Some(engaged) = self.engaged.as_ref() {
            let _ = compositor.raise(engaged.wm);
        }
    }

    /// Draw what the kept frame owes, in the look the session wears now: the
    /// whole frame when the screen or the look moved since it was painted,
    /// else only the owed rectangles, each re-derived under its own clip.
    ///
    /// A frame the compositor cannot give back stays owed and leaves the
    /// previous one up.
    fn draw(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        let screen = compositor.screen_rect();
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let style = shell.style_generation();
        let Some(engaged) = self.engaged.as_mut() else {
            return;
        };
        if engaged.screen != screen || engaged.style != style {
            engaged.owed = Repaint::Whole;
        }
        if engaged.owed.is_clean() {
            return;
        }
        let bounds = window_bounds(screen);
        let area = engaged.owed.area(screen.width, screen.height);
        let surface = &engaged.surface;
        let painted = compositor.repaint_window(
            engaged.wm,
            (screen.width, screen.height),
            &area,
            |frame, rects| {
                damage::paint_parts(frame, rects, |frame| {
                    let _ = surface.paint_into(frame, bounds, scale, theme, Backdrop::Desktop);
                });
            },
        );
        engaged.refused = !painted;
        if !painted {
            return;
        }
        if engaged.screen != screen {
            let _ = compositor.move_window(engaged.wm, screen.origin);
            engaged.screen = screen;
        }
        engaged.style = style;
        engaged.owed = Repaint::clean();
    }

    /// Take the lock down after a successful verification.
    fn release(&mut self, compositor: &mut Compositor) {
        if let Some(engaged) = self.engaged.take() {
            let _ = compositor.remove(engaged.wm);
        }
    }
}

/// The lock window's own rectangle: the screen's extent at the window's
/// origin, which is where its pixels start.
fn window_bounds(screen: Rect) -> Rect {
    Rect::new(0, 0, screen.width, screen.height)
}

/// Rebase a pointer event from screen space into the lock window's space.
///
/// The lock window covers the screen from its top-left, so the two spaces
/// differ only by the screen rectangle's own origin — which is zero on every
/// single-output desktop, and correct rather than assumed on any other.
fn to_window_space(event: &InputEvent, screen: Rect) -> InputEvent {
    match event {
        InputEvent::PointerMoved { to } => InputEvent::PointerMoved {
            to: Point::new(to.x - screen.origin.x, to.y - screen.origin.y),
        },
        other => *other,
    }
}

/// Paint one frame of the lock over the whole screen, in the appearance the
/// session is currently wearing.
fn render_frame(
    surface: &AuthSurface,
    screen: Rect,
    scale: Scale,
    shell: &DesktopShell,
) -> Option<Surface> {
    surface.render(
        window_bounds(screen),
        scale,
        shell.session().active_theme(),
        Backdrop::Desktop,
    )
}

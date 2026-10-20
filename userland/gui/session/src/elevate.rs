//! The desktop session's **system authorisation prompt**: the one place the
//! desktop asks whether a program may run with authority the signed-in
//! account may not hold.
//!
//! The session holds no such authority and never will. It asks the console's
//! elevation broker — the login supervisor that started it — which accounts
//! may allow the program, shows the system's own sheet
//! ([`AuthoriseSheet`]), and hands the password typed there to the broker,
//! which re-authenticates the account, checks its authority over the program,
//! audits the decision, and runs the program as that account. The session
//! learns only what happened.
//!
//! # A question only the system can ask
//!
//! The sheet stands on a scrim over everything the session shows — every
//! window and the icon bar — so the live desktop is seen dimmed behind it,
//! which no application can reproduce: none may read another's pixels. While
//! it is up the seat is the prompt's alone (the embedder drains every event
//! into [`handle`](AuthorisePrompt::handle)), and it is kept above every
//! window but the screensaver. The secure attention key, Ctrl+Alt+Delete,
//! never reaches an application: pressed at the prompt it answers on the
//! sheet's band, and pressed anywhere else it brings up the system's own
//! notice that nothing is waiting for a password, so a lookalike drawn by an
//! application is exposed by the one key the user is told always reaches
//! the system.
//!
//! # Never waiting on the broker
//!
//! The broker derives a key from every password on purpose, and the serve
//! loop that would wait for it composites every window on the screen. So both
//! of the prompt's questions — who may allow this, and is this their password
//! — are asked through an [`Elevator`] and answered later through
//! [`AuthorisePrompt::answer`].
//!
//! # Asked for an application
//!
//! The desktop's own Settings asks through its window for one of a closed set
//! of tasks ([`crate::systask`]); such a request names the window, and its
//! end is owed to that window exactly once ([`AuthorisePrompt::take_concluded`]).
//! A window that closes takes its question down unanswered
//! ([`AuthorisePrompt::abort_for`]).

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::AuthorisedOutcome;
use tairix_abi::Errno;
use tairix_colour::Rgba;
use tairix_controls::damage::{self, Repaint};
use tairix_controls::Keystroke;
use tairix_greeter::{AccountTile, AuthoriseAction, AuthoriseSheet, Cooldown, Ticket};
use tairix_raster::Color;
use tairix_util::secret::WipedBuf;
use tairix_wm::{Compositor, InputEvent, Point, Rect, Scale, Surface, WindowId};

use crate::shell::{prompt_origin, DesktopShell};
use crate::switchuser::park_within;
use crate::windows::bundle_artwork;

pub use tairix_greeter::{AUTHORISE_WIDTH as WIN_WIDTH, REQUESTER_SIDE};

/// One-shot: the authorisation prompt is on screen and holding the seat.
///
/// Emitted when the sheet first shows a question, so a host that must type
/// into it can wait for a real surface rather than racing the request.
pub const ELEVATE_PROMPT_SHOWN: tairix_log::EventId = tairix_log::EventId(20_004);

/// The exact message [`ELEVATE_PROMPT_SHOWN`] is emitted with.
pub const ELEVATE_PROMPT_SHOWN_MESSAGE: &str = "authorisation prompt on screen";

/// How strongly the scrim dims what it covers.
const SCRIM_ALPHA: u8 = 140;

/// What runs once a request is allowed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Elevation {
    /// Started and left running: an interactive program collects its own
    /// input.
    Launch,
    /// Run with these arguments, waited for.
    Run(Vec<String>),
    /// Run with these arguments, waited for, its output handed back.
    Capture(Vec<String>),
}

/// One request for authority: what runs, how, on whose behalf, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    /// Absolute path of the program to run.
    pub program: String,
    /// How it runs once allowed.
    pub elevation: Elevation,
    /// Who is asking, as the kernel attested it, by the name shown.
    pub requester: String,
    /// The bundle directory whose picture stands for the requester.
    pub bundle: Option<String>,
    /// What the request does, in words, after the requester's name.
    pub purpose: String,
    /// The application window the request was asked for, which is told how
    /// it ended; `None` for the desktop's own.
    pub window: Option<u64>,
}

/// What the broker did with an allowed request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Done {
    /// The program was started and is running.
    Launched {
        /// Its process id.
        pid: i64,
    },
    /// The program ran and exited with this code.
    Ran {
        /// Its exit status.
        exit_code: i32,
    },
    /// The program ran, exited with this code, and printed this.
    Captured {
        /// Its exit status.
        exit_code: i32,
        /// Its whole standard output.
        output: Vec<u8>,
    },
    /// The program ran but printed more than the seam carries; none of it
    /// is offered.
    Overran {
        /// Its exit status.
        exit_code: i32,
    },
}

/// Why an offered password did nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The authority refused it.
    Refused,
    /// The account is cooling down for this long; nothing was checked.
    Cooling(Duration64),
    /// The account was accepted but the program would not run.
    NotDone(Errno),
    /// No answer could be had.
    Unreachable,
}

/// The accounts that may allow a request: the signed-in one when it may,
/// and the others that may.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Listing {
    /// The signed-in account, when it may.
    pub own: Option<AccountTile>,
    /// Every other account that may.
    pub others: Vec<AccountTile>,
}

/// One answer the broker gave the prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// Who may allow the request, or `None` when the broker could not say.
    Listing(Ticket, Option<Listing>),
    /// What became of an offered password.
    Offer(Ticket, Result<Done, Refusal>),
}

impl Answer {
    /// The question this answers.
    #[must_use]
    pub const fn ticket(&self) -> Ticket {
        match self {
            Self::Listing(ticket, _) | Self::Offer(ticket, _) => *ticket,
        }
    }
}

/// How the prompt reaches the broker.
///
/// Injected so the whole prompt — its sheet, its wording, its handling of
/// every answer, its erasure of the secret — is exercised on the host without
/// a kernel. The implementation asks off the serve loop; the prompt itself
/// holds no authority.
pub trait Elevator {
    /// Ask which accounts may allow `program`, answering the ticket the
    /// answer will carry.
    fn authorisers(&mut self, program: &str) -> Ticket;

    /// Ask the broker to perform `request` as `username` once `password`
    /// authenticates it, answering the ticket the answer will carry.
    ///
    /// `password` is the prompt's to erase once this returns, so an
    /// implementor that carries it later copies it into an [`ElevateAsk`].
    fn offer(&mut self, username: &str, password: &str, request: &Request) -> Ticket;
}

/// One question on its way to the broker, its password held in a buffer
/// sized to it so it never moves, and erased once answered and again when
/// dropped.
pub enum ElevateAsk {
    /// Who may allow `program`, for the signed-in account `own`.
    Authorisers {
        /// The question's ticket.
        ticket: Ticket,
        /// The program asked about.
        program: String,
        /// The signed-in account's login name, which the broker lists first
        /// when it may: the one an answer's own account must be.
        own: String,
    },
    /// Perform `request` as `username`, once `password` authenticates it.
    Offer {
        /// The question's ticket.
        ticket: Ticket,
        /// The account to run as.
        username: String,
        /// Its offered password.
        password: WipedBuf,
        /// What to run.
        request: Request,
    },
}

impl ElevateAsk {
    /// The offer `ticket` names, or `None` when no memory holds the password.
    #[must_use]
    pub fn offer(
        ticket: Ticket,
        username: &str,
        password: &str,
        request: &Request,
    ) -> Option<Self> {
        let mut held = WipedBuf::with_capacity(password.len()).ok()?;
        held.extend_zeroed(password.len())?
            .copy_from_slice(password.as_bytes());
        Some(Self::Offer {
            ticket,
            username: String::from(username),
            password: held,
            request: request.clone(),
        })
    }

    /// The question's ticket.
    #[must_use]
    pub const fn ticket(&self) -> Ticket {
        match self {
            Self::Authorisers { ticket, .. } | Self::Offer { ticket, .. } => *ticket,
        }
    }

    /// The offered password, as the text it was offered as.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        match self {
            Self::Offer { password, .. } => core::str::from_utf8(password).ok(),
            Self::Authorisers { .. } => None,
        }
    }

    /// Erase the password now, rather than when the question is dropped.
    pub fn erase(&mut self) {
        if let Self::Offer { password, .. } = self {
            password.wipe();
        }
    }
}

/// How the prompt ended, or that it has not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromptOutcome {
    /// Still up.
    Pending,
    /// The request was allowed and performed. The prompt is already down
    /// and its secret erased.
    Done {
        /// What was asked.
        request: Request,
        /// What became of it.
        done: Done,
    },
    /// Nothing was performed: the user cancelled, or the prompt was
    /// abandoned. `request` is `None` for the secure attention notice.
    Cancelled {
        /// What was asked, when anything was.
        request: Option<Request>,
    },
}

/// What a request asked for an application window owes that window once the
/// prompt ends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Concluded {
    /// The window that asked.
    pub window_id: u64,
    /// What became of its task.
    pub outcome: AuthorisedOutcome,
    /// What a captured task printed, and nothing for any other outcome.
    pub output: Vec<u8>,
}

/// The prompt and the broker it asks, as a window bridge that serves requests
/// reaches them.
pub struct AuthoriseDesk<'a> {
    /// The session's one prompt.
    pub prompt: &'a mut AuthorisePrompt,
    /// How it reaches the broker.
    pub elevator: &'a mut dyn Elevator,
}

/// One showing prompt.
struct Showing {
    request: Option<Request>,
    scrim: WindowId,
    wm: WindowId,
    sheet: AuthoriseSheet,
    /// The screen and look it was last drawn for, and its own size there.
    screen: Rect,
    style: u64,
    extent: (u32, u32),
    owed: Repaint,
    /// The listing and the offer the broker is answering, if any.
    listing: Option<Ticket>,
    offered: Option<(Ticket, String)>,
    cooldown: Cooldown,
    /// Whether the sheet still shows the generic glyph for its requester.
    pictured: bool,
    /// Whether the sheet has just begun asking its question and
    /// [`ELEVATE_PROMPT_SHOWN`] is yet to be announced.
    announce: bool,
}

/// The session's authorisation-prompt slot: idle, or holding exactly one
/// question.
#[derive(Default)]
pub struct AuthorisePrompt {
    showing: Option<Showing>,
    /// The ends owed to the windows whose requests ended, oldest first.
    concluded: Vec<Concluded>,
}

impl AuthorisePrompt {
    /// An idle prompt.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            showing: None,
            concluded: Vec::new(),
        }
    }

    /// The ends owed to application windows since the last take, each to be
    /// delivered once.
    pub fn take_concluded(&mut self) -> Vec<Concluded> {
        core::mem::take(&mut self.concluded)
    }

    /// Window `window_id` has closed: a question it asked comes down
    /// unanswered, and nothing is owed to a window no longer there to tell.
    pub fn abort_for(&mut self, window_id: u64, compositor: &mut Compositor) {
        self.concluded.retain(|owed| owed.window_id != window_id);
        if self.pending().and_then(|request| request.window) != Some(window_id) {
            return;
        }
        if let Some(showing) = self.showing.take() {
            let _ = compositor.remove(showing.wm);
            let _ = compositor.remove(showing.scrim);
        }
    }

    /// Whether the prompt is up and holding the seat.
    ///
    /// While this is `true` the embedder MUST drain every pointer and
    /// keyboard event into [`handle`](Self::handle) and route none onward.
    #[must_use]
    pub fn is_up(&self) -> bool {
        self.showing.is_some()
    }

    /// The program the showing question is about, if one is.
    #[must_use]
    pub fn pending(&self) -> Option<&Request> {
        self.showing
            .as_ref()
            .and_then(|showing| showing.request.as_ref())
    }

    /// Ask whether `request` may be allowed: the scrim comes up, the sheet
    /// says it is finding who may, and the broker is asked.
    ///
    /// Returns whether the prompt came up. One already up, or windows the
    /// compositor could not give, answer `false` and ask nothing, so a
    /// prompt that cannot be shown is never taken for an answer.
    pub fn ask(
        &mut self,
        request: Request,
        elevator: &mut dyn Elevator,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if self.showing.is_some() {
            return false;
        }
        let sheet = AuthoriseSheet::new(&request.requester, &request.purpose);
        if !self.open(sheet, Some(request), shell, compositor) {
            return false;
        }
        if let Some(showing) = self.showing.as_mut() {
            let program = showing
                .request
                .as_ref()
                .map_or("", |request| request.program.as_str());
            showing.listing = Some(elevator.authorisers(program));
        }
        true
    }

    /// The secure attention key was pressed at `now_ns`: the showing sheet
    /// answers on its band, or, with nothing showing, the system's notice
    /// that nothing is waiting comes up. Answers whether anything is up.
    pub fn attend(
        &mut self,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if let Some(showing) = self.showing.as_mut() {
            showing.sheet.attend(now_ns);
            showing.owed = Repaint::Whole;
            return true;
        }
        self.open(AuthoriseSheet::nothing_waiting(), None, shell, compositor)
    }

    /// Adopt one of the broker's answers at `now_ns`.
    ///
    /// Only the answer to a question this prompt is waiting on is adopted.
    /// Returns [`PromptOutcome::Done`] for an allowed request, by which time
    /// the prompt is down; every other answer leaves the question up, saying
    /// what happened.
    pub fn answer(
        &mut self,
        answer: Answer,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> PromptOutcome {
        let theme = shell.session().active_theme();
        let Some(showing) = self.showing.as_mut() else {
            return PromptOutcome::Pending;
        };
        match answer {
            Answer::Listing(ticket, listing) => {
                if showing.listing != Some(ticket) {
                    return PromptOutcome::Pending;
                }
                showing.listing = None;
                match listing {
                    Some(listing) => showing.sheet.set_authorisers(listing.own, listing.others),
                    None => showing.sheet.set_unavailable(),
                }
                showing.announce = true;
                showing.owed = Repaint::Whole;
                PromptOutcome::Pending
            }
            Answer::Offer(ticket, outcome) => {
                let Some((asked, account)) = showing.offered.take() else {
                    return PromptOutcome::Pending;
                };
                if asked != ticket {
                    showing.offered = Some((asked, account));
                    return PromptOutcome::Pending;
                }
                match outcome {
                    Ok(done) => return self.conclude(compositor, Some(done)),
                    Err(Refusal::Refused) => showing.sheet.refuse(now_ns, theme),
                    Err(Refusal::Cooling(retry_after)) => {
                        showing.cooldown.start(now_ns, retry_after, &account);
                        let _ = showing
                            .sheet
                            .set_cooldown(showing.cooldown.remaining(now_ns, Some(&account)));
                    }
                    Err(Refusal::NotDone(_)) => showing.sheet.not_done(),
                    Err(Refusal::Unreachable) => showing.sheet.unreachable(),
                }
                showing.owed = Repaint::Whole;
                PromptOutcome::Pending
            }
        }
    }

    /// Apply one input event taken at `now_ns`, in screen coordinates.
    ///
    /// The pointer reaches the sheet where it is over it and nothing at all
    /// elsewhere: the scrim takes every press around the sheet. On anything
    /// other than [`PromptOutcome::Pending`] the prompt is already down.
    pub fn handle(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        elevator: &mut dyn Elevator,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> PromptOutcome {
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let Some(showing) = self.showing.as_mut() else {
            return PromptOutcome::Pending;
        };
        let origin = sheet_origin(showing.screen, showing.extent);
        let bounds = Rect::new(0, 0, showing.extent.0, showing.extent.1);
        let mut owed = damage::sink();
        let acted = match *event {
            InputEvent::KeyPressed { key, modifiers } => showing.sheet.on_key(
                Keystroke {
                    key,
                    modifiers,
                    at_ns: now_ns,
                },
                bounds,
                scale,
                theme,
                &mut owed,
            ),
            InputEvent::PointerMoved { to } => showing.sheet.on_pointer(
                &InputEvent::PointerMoved {
                    to: Point::new(to.x - origin.x, to.y - origin.y),
                },
                bounds,
                scale,
                theme,
                &mut owed,
            ),
            InputEvent::PointerPressed { .. }
            | InputEvent::PointerReleased { .. }
            | InputEvent::PointerScrolled { .. } => showing
                .sheet
                .on_pointer(event, bounds, scale, theme, &mut owed),
            _ => None,
        };
        for rect in owed.rects() {
            showing.owed.add(*rect);
        }
        match acted {
            Some(AuthoriseAction::Cancelled) => self.conclude(compositor, None),
            Some(AuthoriseAction::Offered) => {
                self.offer(elevator);
                PromptOutcome::Pending
            }
            None => PromptOutcome::Pending,
        }
    }

    /// Resolve the requester's picture through the session's artwork cache.
    ///
    /// Until it lands the sheet shows the generic glyph; one that cannot be
    /// had leaves the glyph and is asked for no more. The decode runs where
    /// the cache runs it, so a paint never waits on it.
    pub fn resolve_picture(&mut self, shell: &mut DesktopShell, scale: Scale) {
        let (picture, pending) = {
            let Some(bundle) = self
                .showing
                .as_ref()
                .filter(|showing| !showing.pictured)
                .and_then(|showing| showing.request.as_ref())
                .and_then(|request| request.bundle.as_deref())
            else {
                return;
            };
            bundle_artwork(shell, bundle, scale.scale_length(REQUESTER_SIDE))
        };
        let Some(showing) = self.showing.as_mut() else {
            return;
        };
        if let Some(picture) = picture {
            showing.sheet.set_picture(picture);
            showing.owed = Repaint::Whole;
        }
        showing.pictured = !pending;
    }

    /// Step the sheet's motion and the lockout's countdown to `now_ns`, then
    /// draw what is owed. Answers whether the sheet began asking its question
    /// in what was just drawn, which the embedder announces.
    pub fn advance(
        &mut self,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let Some(showing) = self.showing.as_mut() else {
            return false;
        };
        // A lockout counts down only while nothing is being checked: an
        // answer pending is the broker's to give.
        if !showing.sheet.is_checking() {
            if let Some(account) = showing.sheet.account().map(AccountTile::login_name) {
                let remaining = showing.cooldown.remaining(now_ns, Some(account));
                if showing.sheet.set_cooldown(remaining) {
                    showing.owed = Repaint::Whole;
                }
            }
        }
        let bounds = Rect::new(0, 0, showing.extent.0, showing.extent.1);
        let mut moved = damage::sink();
        showing
            .sheet
            .advance(now_ns, bounds, scale, theme, &mut moved);
        for rect in moved.rects() {
            showing.owed.add(*rect);
        }
        self.draw(shell, compositor);
        self.showing.as_mut().is_some_and(|showing| {
            showing.owed.is_clean() && core::mem::take(&mut showing.announce)
        })
    }

    /// `park_ns` shortened to the sheet's next frame on a display refreshing
    /// at `refresh`, the next second of a lockout's countdown, or now while a
    /// change is owed; left as it is with nothing up.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64, refresh: RefreshRate) -> u64 {
        park_within(
            park_ns,
            self.showing.as_ref().and_then(|showing| {
                if !showing.owed.is_clean() {
                    return Some(0);
                }
                let tick = showing
                    .sheet
                    .account()
                    .map(AccountTile::login_name)
                    .and_then(|account| {
                        Cooldown::tick_of(showing.cooldown.remaining(now_ns, Some(account)))
                    });
                showing
                    .sheet
                    .next_frame_in(now_ns, refresh)
                    .into_iter()
                    .chain(tick)
                    .min()
            }),
        )
    }

    /// Raise the scrim and the sheet above everything but `above` — the
    /// screensaver — so nothing opened behind them surfaces over them.
    ///
    /// The embedder calls this before each composite while the prompt is up.
    pub fn keep_topmost(&mut self, compositor: &mut Compositor, above: Option<WindowId>) {
        let Some(showing) = self.showing.as_ref() else {
            return;
        };
        for wm in [showing.scrim, showing.wm] {
            let beneath = above.is_some_and(|anchor| compositor.stack_below(wm, anchor));
            if !beneath {
                let _ = compositor.raise(wm);
            }
        }
    }

    /// Repaint everything the prompt shows, so a change of look or of screen
    /// behind it redraws it as it now must be.
    pub fn repaint(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        if let Some(showing) = self.showing.as_mut() {
            showing.owed = Repaint::Whole;
        }
        self.draw(shell, compositor);
    }

    /// Take the prompt down without performing anything.
    ///
    /// Used when the screen is locked, the account switched away from, or
    /// the session ends: an unanswered question must not stand where the
    /// person it was for may no longer be. The secret goes with the sheet,
    /// whose field zeroises itself as it is dropped. An offer already with
    /// the broker is answered to nobody.
    pub fn abandon(&mut self, compositor: &mut Compositor) -> PromptOutcome {
        self.conclude(compositor, None)
    }

    /// Ask the broker about what was typed, the sheet saying it is checking —
    /// the password erased from it — until the answer comes back.
    fn offer(&mut self, elevator: &mut dyn Elevator) {
        let Some(showing) = self.showing.as_mut() else {
            return;
        };
        let (Some(request), Some(account), Some(secret)) = (
            showing.request.as_ref(),
            showing
                .sheet
                .account()
                .map(|account| String::from(account.login_name())),
            showing.sheet.secret(),
        ) else {
            return;
        };
        let ticket = elevator.offer(&account, secret, request);
        showing.offered = Some((ticket, account));
        showing.sheet.check();
        showing.owed = Repaint::Whole;
    }

    /// Bring up the scrim and `sheet` for `request`.
    fn open(
        &mut self,
        sheet: AuthoriseSheet,
        request: Option<Request>,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let screen = compositor.screen_rect();
        let theme = shell.session().active_theme();
        let extent = sheet.client_extent(compositor.scale(), theme);
        let Some(scrim_surface) = scrim(screen) else {
            return false;
        };
        let Some(sheet_surface) = Surface::new(extent.0, extent.1) else {
            return false;
        };
        let scrim = compositor.add_window(screen.origin, scrim_surface);
        let wm = compositor.add_window(sheet_origin(screen, extent), sheet_surface);
        let _ = compositor.raise(scrim);
        let _ = compositor.raise(wm);
        self.showing = Some(Showing {
            request,
            scrim,
            wm,
            sheet,
            screen,
            style: shell.style_generation(),
            extent,
            owed: Repaint::Whole,
            listing: None,
            offered: None,
            cooldown: Cooldown::default(),
            pictured: false,
            announce: false,
        });
        self.draw(shell, compositor);
        true
    }

    /// Draw what the prompt owes: the scrim whenever the screen changed, the
    /// sheet at its content-sized extent wherever its pixels are stale.
    fn draw(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        let screen = compositor.screen_rect();
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let style = shell.style_generation();
        let Some(showing) = self.showing.as_mut() else {
            return;
        };
        let extent = showing.sheet.client_extent(scale, theme);
        if showing.screen != screen {
            if let Some(cover) = scrim(screen) {
                let _ = compositor.set_surface(showing.scrim, cover);
                let _ = compositor.move_window(showing.scrim, screen.origin);
            }
            showing.owed = Repaint::Whole;
        }
        if showing.style != style || extent != showing.extent {
            showing.owed = Repaint::Whole;
        }
        if showing.owed.is_clean() {
            return;
        }
        let bounds = Rect::new(0, 0, extent.0, extent.1);
        let area = showing.owed.area(extent.0, extent.1);
        let sheet = &showing.sheet;
        let painted = compositor.repaint_window(showing.wm, extent, &area, |frame, rects| {
            damage::paint_parts(frame, rects, |frame| {
                frame.fill(Color::TRANSPARENT);
                sheet.render(frame, bounds, scale, theme);
            });
        });
        if !painted {
            return;
        }
        if showing.screen != screen || showing.extent != extent {
            let _ = compositor.move_window(showing.wm, sheet_origin(screen, extent));
        }
        showing.screen = screen;
        showing.extent = extent;
        showing.style = style;
        showing.owed = Repaint::clean();
    }

    /// Take the prompt down and say how it ended.
    fn conclude(&mut self, compositor: &mut Compositor, done: Option<Done>) -> PromptOutcome {
        let Some(showing) = self.showing.take() else {
            return PromptOutcome::Pending;
        };
        let _ = compositor.remove(showing.wm);
        let _ = compositor.remove(showing.scrim);
        // Dropping the sheet here zeroises its field.
        let outcome = match (done, showing.request) {
            (Some(done), Some(request)) => PromptOutcome::Done { request, done },
            (_, request) => PromptOutcome::Cancelled { request },
        };
        let (window, done) = match &outcome {
            PromptOutcome::Done { request, done } => (request.window, Some(done.clone())),
            PromptOutcome::Cancelled { request } => {
                (request.as_ref().and_then(|request| request.window), None)
            }
            PromptOutcome::Pending => (None, None),
        };
        if let Some(window_id) = window {
            let (owed, output) = crate::systask::outcome_of(done);
            self.concluded.push(Concluded {
                window_id,
                outcome: owed,
                output,
            });
        }
        outcome
    }

    /// The sheet's window, for a host-side observer.
    #[must_use]
    pub fn wm_id(&self) -> Option<WindowId> {
        self.showing.as_ref().map(|showing| showing.wm)
    }

    /// The scrim's window, for a host-side observer.
    #[must_use]
    pub fn scrim_id(&self) -> Option<WindowId> {
        self.showing.as_ref().map(|showing| showing.scrim)
    }

    /// The sheet's bounds on `screen`, for a host-side observer.
    #[must_use]
    pub fn sheet_rect(&self) -> Option<Rect> {
        self.showing.as_ref().map(|showing| {
            let origin = sheet_origin(showing.screen, showing.extent);
            Rect::new(origin.x, origin.y, showing.extent.0, showing.extent.1)
        })
    }

    /// The sheet's own state, for a host-side observer.
    #[cfg(test)]
    pub(crate) fn sheet(&self) -> Option<&AuthoriseSheet> {
        self.showing.as_ref().map(|showing| &showing.sheet)
    }
}

/// Where a sheet of `extent` stands on `screen`.
fn sheet_origin(screen: Rect, extent: (u32, u32)) -> Point {
    prompt_origin(screen, extent)
}

/// The scrim over `screen`: black, part-transparent, so the live desktop is
/// seen dimmed behind it.
fn scrim(screen: Rect) -> Option<Surface> {
    let mut cover = Surface::new(screen.width, screen.height)?;
    cover.fill(Color::from(Rgba::new(0, 0, 0, SCRIM_ALPHA)));
    Some(cover)
}

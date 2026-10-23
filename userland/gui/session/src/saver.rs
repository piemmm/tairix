//! The screensaver: one surface over the whole screen once the desktop has
//! sat idle, gone at the first input — and, a set time after it starts, the
//! display behind it switched off.
//!
//! It covers every window and hides the pointer, so nothing on screen is
//! legible while it is up. It is not a lock: the first input only takes it
//! down, and reaches nothing else. A lock the idle policy engaged beneath it
//! is what the user meets when they come back.
//!
//! The desktop draws its own two itself, black or its backdrop dimmed. Every
//! other screensaver is a bundle spawned into the drawing sandbox and hosted
//! ([`Hosted`]): it draws into a region of its own and the session lays what
//! it presents onto the screensaver's surface, so no saver's code runs in the
//! session (`plans/SCREENSAVERS.md`). A saver that is ended — it misbehaved,
//! was late, or went — leaves the screen black, and the end is reported.
//!
//! Switching the display off is asked of the display itself. One that has no
//! power control of its own is kept black and still instead, so the desktop
//! spends nothing on it either way; only a display that is genuinely off is
//! presented nothing at all.
//!
//! A screensaver can also be shown on request, as a preview. Then the first
//! moment of pointer motion does not take it down — the hand that asked for
//! it is still on the mouse — while a key, a press or a scroll always does.

mod catalog;
#[cfg(test)]
pub(crate) mod fixtures;
mod hosted;
mod keep;
mod talk;

pub use catalog::{load_savers, Saver, SaverCatalog, SAVER_REFUSED, SAVER_REFUSED_MESSAGE};
pub use hosted::{Ended, Hosted, SaverErrands, SaverMemory};
pub use keep::{keep, kept_at, Picture, PictureFiles, Unkept};
pub use talk::Fault;

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::desktop::{Appearance, Contrast, Density, DesktopText};
use tairix_abi::driver::display::RefreshRate;
use tairix_abi::saver_ipc::{OptionValue, SaverRegion, Start, WallReading, SAVER_SEED_LEN};
use tairix_abi::switchboard_ipc::MachineReport;
use tairix_abi::sysinfo::MemoryBand;
use tairix_abi::{DriverError, Errno};
use tairix_display::{DisplaySleep, Presenter, SwitchedOff};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_parallel::JobRunner;
use tairix_reclaim::PressureBand;
use tairix_sandbox::{SessionDescriptors, SessionTransport};
use tairix_wallpaper::{SaverValues, TaskNames};
use tairix_wm::{Color, Compositor, Scale, Surface, WindowId};

use crate::switchuser::park_within;

/// How long a preview keeps the screen through pointer motion alone: long
/// enough for the hand that pressed its button to come to rest.
pub const PREVIEW_STEADY_NS: u64 = 1_500_000_000;

/// Event id of a hosted saver's first frame laid on the screen, in the desktop
/// session's reserved range.
pub const SAVER_SHOWN: EventId = EventId(20_028);

/// The exact message [`SAVER_SHOWN`] is emitted with.
pub const SAVER_SHOWN_MESSAGE: &str = "screensaver presented its first frame";

/// Event id of a hosted saver taken down on purpose: dismissed, or its display
/// switched off.
pub const SAVER_DISMISSED: EventId = EventId(20_029);

/// The exact message [`SAVER_DISMISSED`] is emitted with.
pub const SAVER_DISMISSED_MESSAGE: &str = "screensaver dismissed";

/// Event id of a hosted saver ended because it misbehaved, was late, went, or
/// never started.
pub const SAVER_ENDED: EventId = EventId(20_030);

/// The exact message [`SAVER_ENDED`] is emitted with.
pub const SAVER_ENDED_MESSAGE: &str = "screensaver ended; the screen is kept black";

/// Event id of a chooser showing the screensaver catalogue: the first
/// screensaver picture a window asks for once the list was read to its end,
/// which it can ask for only once it holds the list.
pub const SAVER_CATALOGUE_SHOWN: EventId = EventId(20_032);

/// The exact message [`SAVER_CATALOGUE_SHOWN`] is emitted with.
pub const SAVER_CATALOGUE_SHOWN_MESSAGE: &str = "screensaver catalogue shown in a chooser";

/// How dark a dimmed screensaver lays black over the backdrop, out of 255:
/// enough that nothing reads as an invitation to click, not so much that the
/// picture is lost.
const DIM_ALPHA: u8 = 176;

/// The address space a saver is given beyond its region, which is its picture:
/// room for an ordinary heap. A containment bound on untrusted code, held to a
/// share of the machine below.
const SAVER_MEMORY_BYTES: u64 = 128 << 20;

/// What a saver asking for the large ceiling is given beyond that: a scene of
/// the ray tracer's maximum detail at its peak.
const SAVER_LARGE_MEMORY_BYTES: u64 = 2 << 30;

/// The most of the machine's memory one saver may be given, as a divisor.
const SAVER_MEMORY_SHARE: u64 = 2;

/// What starts a saver bundle: its region made and its program spawned into
/// the drawing sandbox, handed the region.
pub trait SaverLauncher {
    /// The pipes to a saver.
    type Transport: SessionTransport;

    /// Start the program `entry`, handed a region of `len` bytes, held to the
    /// `ceilings` of address space and threads.
    ///
    /// # Errors
    ///
    /// Why it could not be started.
    fn launch(
        &mut self,
        entry: &str,
        len: usize,
        ceilings: (u64, u64),
    ) -> Result<(Self::Transport, Box<dyn SaverMemory>), Errno>;
}

/// What a saver is started from, beyond what it declares: the screen and the
/// desktop it covers.
pub struct SaverContext<'a> {
    /// The screen.
    pub screen: (u32, u32),
    /// The desktop's UI scale.
    pub scale: Scale,
    /// The display's refresh rate.
    pub refresh: RefreshRate,
    /// The desktop's look, its text and its accessibility axes.
    pub look: (Appearance, Option<DesktopText>, Contrast, Density, bool),
    /// The CSPRNG's draw for the saver, its only entropy.
    pub seed: [u8; SAVER_SEED_LEN],
    /// The machine's memory pressure.
    pub pressure: PressureBand,
    /// The machine's online cores, and the memory it holds.
    pub machine: (u16, u64),
    /// Who is signed in, and where.
    pub identity: (&'a str, &'a str),
    /// How many slides the slides feed offers.
    pub slides: u16,
    /// The values the user gave every saver's options.
    pub values: &'a SaverValues,
    /// Which tasks' names the machine feed may carry.
    pub names: TaskNames,
}

/// A saver the screensaver is to host: the bundle and how it is started.
pub struct SaverLaunch<'a, L: SaverLauncher> {
    /// The bundle.
    pub saver: &'a Saver,
    /// What it is started from.
    pub context: SaverContext<'a>,
    /// What spawns it.
    pub launcher: &'a mut L,
}

/// What a screensaver draws from, gathered by the embedder as one starts.
pub struct SaverSetup<'a, L: SaverLauncher> {
    /// The desktop's own backdrop at the screen's size, which the dimmed
    /// screensaver darkens; without one it is black.
    pub ground: Option<Surface>,
    /// The saver bundle to host, for a choice that is one.
    pub launch: Option<SaverLaunch<'a, L>>,
}

/// What woke the screen behind a screensaver.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Waking {
    /// The pointer moved, and nothing else happened.
    Moved,
    /// A key, a press or a scroll: a gesture nobody makes by accident.
    Acted,
}

/// A hosted saver that was ended, for the embedder to say.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaverEnd {
    /// Its identifier, as its manifest spells it.
    pub saver: String,
    /// What the saver is called.
    pub title: String,
    /// Why it was ended.
    pub ended: Ended,
    /// The code it exited with, where one is known.
    pub code: Option<i32>,
}

impl SaverEnd {
    /// Why it was ended, as said after its name.
    fn reason(&self) -> Reason<'_> {
        Reason(self)
    }
}

impl fmt::Display for SaverEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the screensaver {} {}; the screen is kept black",
            self.title,
            self.reason()
        )
    }
}

/// A saver end's reason, alone.
struct Reason<'a>(&'a SaverEnd);

impl fmt::Display for Reason<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.ended {
            Ended::Refused(errno) => write!(f, "could not be started: {errno}"),
            Ended::Declined(errno) => write!(f, "could not start: {errno}"),
            Ended::Faulted(Fault::OutOfTurn) => f.write_str("broke the screensaver protocol"),
            Ended::Faulted(Fault::NeverReady) => f.write_str("did not start in time"),
            Ended::Faulted(Fault::Late) => f.write_str("did not draw a frame in time"),
            Ended::Garbled => f.write_str("sent what the screensaver protocol does not say"),
            Ended::Gone => match self.0.code {
                Some(code) => write!(f, "stopped with exit code {code}"),
                None => f.write_str("stopped"),
            },
            Ended::Unwatched(errno) => write!(f, "could not be watched: {errno}"),
        }
    }
}

/// One screensaver on the screen.
struct Shown<T: SessionTransport, S: Sink> {
    wm: WindowId,
    size: (u32, u32),
    /// The saver drawing on it, while one is.
    hosted: Option<HostedSaver<T, S>>,
}

/// The saver drawing on the screensaver.
struct HostedSaver<T: SessionTransport, S: Sink> {
    hosted: Hosted<T, S>,
    /// Who it is, which its records and its end are reported under.
    name: SaverName,
    /// Which of the session's savers it is: what its errands' answers carry,
    /// so none reaches a saver started after the one that asked.
    instance: u64,
    /// Whether its first frame has been recorded.
    announced: bool,
}

/// Who a hosted saver is.
struct SaverName {
    /// Its identifier, as its manifest spells it.
    saver: String,
    /// What it is called.
    title: String,
}

/// The session's screensaver.
pub struct Screensaver<T: SessionTransport, S: Sink> {
    shown: Option<Shown<T, S>>,
    /// How the display behind it sleeps, if it does.
    sleep: DisplaySleep,
    /// Until when pointer motion alone leaves a preview up.
    steady_until_ns: Option<u64>,
    /// How many savers it has hosted: the next one's instance.
    hosted: u64,
    /// The session's log, which every hosted saver's session and record
    /// goes through.
    log: S,
}

impl<T: SessionTransport, S: Sink + Clone> Screensaver<T, S> {
    /// No screensaver up, recording through `log`.
    #[must_use]
    pub const fn new(log: S) -> Self {
        Self {
            shown: None,
            sleep: DisplaySleep::new(),
            steady_until_ns: None,
            hosted: 0,
            log,
        }
    }

    /// [`start`](Self::start) as a preview: pointer motion in its first
    /// [`PREVIEW_STEADY_NS`] leaves it up.
    ///
    /// A screensaver already up is left as it is, and so is its waking rule.
    pub fn start_preview<L: SaverLauncher<Transport = T>>(
        &mut self,
        setup: SaverSetup<'_, L>,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Result<bool, SaverEnd> {
        if !self.would_start() {
            return Ok(true);
        }
        let started = self.start(setup, compositor, now_ns);
        if matches!(started, Ok(true)) {
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

    /// Whether a start would cover the screen anew: none is up. A start while
    /// one is up leaves it as it is, so an embedder prepares nothing for one,
    /// least of all what the saver up draws on.
    #[must_use]
    pub const fn would_start(&self) -> bool {
        self.shown.is_none()
    }

    /// Whether the display is switched off, so the embedder presents
    /// nothing: no frame it sends can be seen.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.sleep.is_off()
    }

    /// Cover the screen at monotonic `now_ns` and hide the pointer: black, or
    /// the dimmed `ground`, beneath the saver `setup` launches if it names
    /// one.
    ///
    /// Answers whether the screen is covered: a surface the heap would not
    /// give covers nothing, and the desktop stays as it was. A saver that
    /// could not be started leaves the screen covered black, and says why.
    ///
    /// # Errors
    ///
    /// The saver that could not be started, the screen covered black.
    pub fn start<L: SaverLauncher<Transport = T>>(
        &mut self,
        setup: SaverSetup<'_, L>,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Result<bool, SaverEnd> {
        if !self.would_start() {
            return Ok(true);
        }
        let screen = compositor.screen_rect();
        let size = (screen.width, screen.height);
        let frame = match setup.ground {
            Some(mut ground) => {
                let (width, height) = (ground.width(), ground.height());
                // Composited over the ground: a plain fill would replace it.
                ground.fill_round_rect(0, 0, width, height, 0, Color::rgba(0, 0, 0, DIM_ALPHA));
                Some(ground)
            }
            None => black(size),
        };
        let Some(frame) = frame else {
            return Ok(false);
        };
        let wm = compositor.add_window(screen.origin, frame);
        compositor.raise(wm);
        compositor.set_cursor_hidden(true);
        let mut shown = Shown {
            wm,
            size,
            hosted: None,
        };
        let started = setup
            .launch
            .map(|launch| host(launch, self.log.clone(), now_ns));
        let outcome = match started {
            Some(Ok((hosted, name))) => {
                self.hosted = self.hosted.wrapping_add(1);
                shown.hosted = Some(HostedSaver {
                    hosted,
                    name,
                    instance: self.hosted,
                    announced: false,
                });
                Ok(true)
            }
            Some(Err(end)) => {
                self.record_end(&end);
                Err(end)
            }
            None => Ok(true),
        };
        self.shown = Some(shown);
        outcome
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
        display: Option<&mut dyn Presenter>,
    ) -> Result<bool, DriverError> {
        let was_up = self.is_shown();
        self.sleep.wake(display)?;
        self.steady_until_ns = None;
        if let Some(mut shown) = self.shown.take() {
            if let Some(saver) = shown.hosted.take() {
                self.record_dismissed(&saver.name);
                let _ = saver.hosted.end();
            }
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
    /// desktop stays as it is and takes input as it did. Either way the saver
    /// drawing on it is ended: nothing it drew could be seen.
    pub fn switch_display_off(
        &mut self,
        compositor: &mut Compositor,
        display: Option<&mut dyn Presenter>,
    ) -> Option<SwitchedOff> {
        let answer = self.sleep.switch_off(display, self.shown.is_some())?;
        let blank = !matches!(answer, SwitchedOff::Off);
        if let Some((name, _)) = self.stop_saver(compositor, blank) {
            self.record_dismissed(&name);
        }
        Some(answer)
    }

    /// End the saver drawing on the screensaver, laying black over what it
    /// drew when `blank`: who it was, and the code it exited with.
    fn stop_saver(
        &mut self,
        compositor: &mut Compositor,
        blank: bool,
    ) -> Option<(SaverName, Option<i32>)> {
        let shown = self.shown.as_mut()?;
        let saver = shown.hosted.take()?;
        if blank {
            if let Some(frame) = black(shown.size) {
                let _ = compositor.set_surface(shown.wm, frame);
            }
        }
        let code = saver.hosted.end();
        Some((saver.name, code))
    }

    /// End the saver for `ended`, blacking the screen, and say so.
    fn fail(&mut self, compositor: &mut Compositor, ended: Ended) -> Option<SaverEnd> {
        let (name, code) = self.stop_saver(compositor, true)?;
        let end = SaverEnd {
            saver: name.saver,
            title: name.title,
            ended,
            code,
        };
        self.record_end(&end);
        Some(end)
    }

    /// A frame was presented to the display: lay the hosted saver's frame held
    /// behind the one it carried, and record the saver's first frame on the
    /// screen, once.
    pub fn shown(&mut self, compositor: &mut Compositor) {
        self.announce(|hosted, wm| hosted.shown((compositor, wm)));
    }

    /// Record the hosted saver's first frame on the screen, once: `reached`
    /// says whether one of its frames has reached it.
    fn announce(&mut self, reached: impl FnOnce(&mut Hosted<T, S>, WindowId) -> bool) {
        let preview = self.steady_until_ns.is_some();
        let Some(Shown {
            wm,
            hosted: Some(saver),
            ..
        }) = self.shown.as_mut()
        else {
            return;
        };
        if !reached(&mut saver.hosted, *wm) || saver.announced {
            return;
        }
        saver.announced = true;
        log(
            &self.log,
            &Event {
                level: Level::Info,
                id: SAVER_SHOWN,
                message: SAVER_SHOWN_MESSAGE,
                fields: &[
                    Field {
                        key: "saver",
                        value: FieldValue::Str(&saver.name.saver),
                    },
                    Field {
                        key: "preview",
                        value: FieldValue::Bool(preview),
                    },
                ],
            },
        );
    }

    /// Record that the saver `name` was taken down on purpose.
    fn record_dismissed(&self, name: &SaverName) {
        log(
            &self.log,
            &Event {
                level: Level::Info,
                id: SAVER_DISMISSED,
                message: SAVER_DISMISSED_MESSAGE,
                fields: &[Field {
                    key: "saver",
                    value: FieldValue::Str(&name.saver),
                }],
            },
        );
    }

    /// Record `end` where it can be found whoever reads the session's
    /// standard error.
    fn record_end(&self, end: &SaverEnd) {
        let reason = alloc::format!("{}", end.reason());
        log(
            &self.log,
            &Event {
                level: Level::Warn,
                id: SAVER_ENDED,
                message: SAVER_ENDED_MESSAGE,
                fields: &[
                    Field {
                        key: "saver",
                        value: FieldValue::Str(&end.saver),
                    },
                    Field {
                        key: "reason",
                        value: FieldValue::Str(&reason),
                    },
                ],
            },
        );
    }

    /// End the saver drawing, its pipes refused a watch for `errno`: what it
    /// says could never be heard.
    pub fn unwatched(&mut self, errno: Errno, compositor: &mut Compositor) -> Option<SaverEnd> {
        self.fail(compositor, Ended::Unwatched(errno))
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

    /// The hosted saver's two descriptors, for the embedder's wait-set, and
    /// which of them it wants watched.
    #[must_use]
    pub fn saver_descriptors(&self) -> (Option<SessionDescriptors>, bool, bool) {
        match self.hosted() {
            Some(hosted) => (
                hosted.descriptors(),
                hosted.wants_read(),
                hosted.wants_write(),
            ),
            None => (None, false, false),
        }
    }

    fn hosted(&self) -> Option<&Hosted<T, S>> {
        self.shown
            .as_ref()
            .and_then(|shown| shown.hosted.as_ref())
            .map(|saver| &saver.hosted)
    }

    /// Which saver is drawing, for the errands it asks for to carry.
    #[must_use]
    pub fn hosting(&self) -> Option<u64> {
        self.shown
            .as_ref()
            .and_then(|shown| shown.hosted.as_ref())
            .map(|saver| saver.instance)
    }

    /// Run `act` on the hosted saver, ending it on the reason `act` gives.
    fn with_hosted(
        &mut self,
        compositor: &mut Compositor,
        act: impl FnOnce(&mut Hosted<T, S>, WindowId, &mut Compositor) -> Result<(), Ended>,
    ) -> Option<SaverEnd> {
        let shown = self.shown.as_mut()?;
        let saver = shown.hosted.as_mut()?;
        let ended = act(&mut saver.hosted, shown.wm, compositor).err()?;
        self.fail(compositor, ended)
    }

    /// [`with_hosted`](Self::with_hosted), for an answer to an errand saver
    /// `instance` asked for: dropped where another saver draws now.
    fn answering(
        &mut self,
        instance: u64,
        compositor: &mut Compositor,
        act: impl FnOnce(&mut Hosted<T, S>, &dyn JobRunner) -> Result<(), Ended>,
    ) -> Option<SaverEnd> {
        if self.hosting() != Some(instance) {
            return None;
        }
        self.with_hosted(compositor, |hosted, _, compositor| {
            act(hosted, compositor.job_runner())
        })
    }

    /// The saver's pipe is readable at `now_ns`: take in what it said.
    pub fn on_readable(
        &mut self,
        now_ns: u64,
        (compositor, display): (&mut Compositor, Option<&mut dyn Presenter>),
        errands: &mut dyn SaverErrands,
    ) -> Option<SaverEnd> {
        let ended = self.with_hosted(compositor, |hosted, wm, compositor| {
            hosted.on_readable(now_ns, (compositor, wm, display), errands)
        });
        if ended.is_none() {
            self.announce(|hosted, _| hosted.take_reached());
        }
        ended
    }

    /// The display latched a frame: one the hosted saver drew in its memory
    /// may let the next go, and the saver's first frame on the screen is
    /// recorded once.
    pub fn latched(&mut self, compositor: &mut Compositor, display: &mut dyn Presenter) {
        self.announce(|hosted, wm| hosted.latched((compositor, wm), display));
    }

    /// Whether the hosted saver's frames go straight to the display, the
    /// compositor to present nothing meanwhile.
    #[must_use]
    pub fn scans_out(&self) -> bool {
        self.shown
            .as_ref()
            .and_then(|shown| shown.hosted.as_ref())
            .is_some_and(|saver| saver.hosted.scans_out())
    }

    /// The saver's pipe has room, at `now_ns`, for what is queued.
    pub fn on_writable(&mut self, now_ns: u64, compositor: &mut Compositor) -> Option<SaverEnd> {
        self.with_hosted(compositor, |hosted, _, _| hosted.on_writable(now_ns))
    }

    /// Carry the hosted saver on to `now_ns`, the wall clock reading `wall`:
    /// end it past its deadline, and ask for the frame it is owed. Nothing is
    /// asked of it while the display sleeps.
    pub fn advance(
        &mut self,
        now_ns: u64,
        wall: &WallReading,
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        if !self.sleep.is_awake() {
            return None;
        }
        self.with_hosted(compositor, |hosted, _, _| hosted.advance(now_ns, wall))
    }

    /// The font service answered, at `now_ns`, the call relayed for saver
    /// `instance`.
    pub fn font_answered(
        &mut self,
        instance: u64,
        (reply, now_ns): (Result<&[u8], Errno>, u64),
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        self.answering(instance, compositor, |hosted, _| {
            hosted.font_answered(reply, now_ns)
        })
    }

    /// Slide `index` saver `instance` wanted landed, fitted to the screen, or
    /// could not be read.
    pub fn slide_landed(
        &mut self,
        instance: u64,
        (index, slide): (u16, Option<&Surface>),
        now_ns: u64,
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        self.answering(instance, compositor, |hosted, runner| {
            hosted.slide_landed(index, slide, now_ns, runner)
        })
    }

    /// The picture saver `instance` handed over was kept, or was not.
    pub fn kept(
        &mut self,
        instance: u64,
        (kept, now_ns): (bool, u64),
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        self.answering(instance, compositor, |hosted, _| hosted.kept(kept, now_ns))
    }

    /// Whether a saver given the machine feed is up on an awake display, and
    /// so wants the machine reports.
    #[must_use]
    pub fn wants_machine_reports(&self) -> bool {
        self.sleep.is_awake() && self.hosted().is_some_and(Hosted::wants_machine_reports)
    }

    /// Hand the saver `report`, which came at `now_ns`.
    pub fn adopt_machine_report(
        &mut self,
        report: MachineReport,
        now_ns: u64,
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        if !self.wants_machine_reports() {
            return None;
        }
        self.with_hosted(compositor, |hosted, _, _| hosted.machine(report, now_ns))
    }

    /// The monitor that reads the machine is not running: a saver given the
    /// machine feed is told its last reading is no longer live.
    pub fn machine_unmonitored(
        &mut self,
        now_ns: u64,
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        self.with_hosted(compositor, |hosted, _, _| hosted.machine_lost(now_ns))
    }

    /// The machine's memory pressure moved to `band` at `now_ns`.
    pub fn pressure(
        &mut self,
        band: PressureBand,
        now_ns: u64,
        compositor: &mut Compositor,
    ) -> Option<SaverEnd> {
        let band = memory_band(band);
        self.with_hosted(compositor, |hosted, _, _| hosted.pressure(band, now_ns))
    }

    /// `park_ns` shortened to what the hosted saver is next owed, or left as
    /// it is: nothing is owed while the display sleeps.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        if !self.sleep.is_awake() {
            return park_ns;
        }
        let due = self.hosted().and_then(Hosted::due_ns);
        park_within(park_ns, due.map(|due| due.saturating_sub(now_ns)))
    }
}

/// Spawn and begin hosting the saver `launch` names at `now_ns`, its session
/// logging through `sink`: the saver, and who it is.
fn host<L: SaverLauncher, S: Sink>(
    launch: SaverLaunch<'_, L>,
    sink: S,
    now_ns: u64,
) -> Result<(Hosted<L::Transport, S>, SaverName), SaverEnd> {
    let SaverLaunch {
        saver,
        context,
        launcher,
    } = launch;
    let refused = |errno: Errno| SaverEnd {
        saver: saver.choice.clone(),
        title: saver.title.clone(),
        ended: Ended::Refused(errno),
        code: None,
    };
    let names = context.names;
    let start = saver_start(saver, &context, now_ns).map_err(refused)?;
    let ceilings = ceilings(saver, start.region.len(), context.machine);
    let entry = tairix_appstore::entry_path(&saver.bundle);
    let (transport, region) = launcher
        .launch(&entry, start.region.len(), ceilings)
        .map_err(refused)?;
    let hosted = Hosted::begin(
        (transport, region, sink),
        &start,
        (names, &saver.title),
        now_ns,
    )
    .map_err(refused)?;
    let name = SaverName {
        saver: saver.choice.clone(),
        title: saver.title.clone(),
    };
    Ok((hosted, name))
}

/// The `Start` `saver` is begun with at `now_ns`, its every option given the
/// value the user chose or its default.
///
/// # Errors
///
/// The refusal of a screen no region could hold, or an option past the most
/// a start carries.
pub fn saver_start(saver: &Saver, context: &SaverContext<'_>, now_ns: u64) -> Result<Start, Errno> {
    let feeds = saver.feeds;
    let region = SaverRegion::new(context.screen.0, context.screen.1, feeds)?;
    let mut start = Start::new(region, context.scale.percent(), context.refresh);
    let (appearance, text, contrast, density, reduced_motion) = context.look;
    start.now_ns = now_ns;
    start.appearance = appearance;
    start.text = text;
    start.contrast = contrast;
    start.density = density;
    start.reduced_motion = reduced_motion;
    start.seed = context.seed;
    start.pressure = memory_band(context.pressure);
    start.cores = context.machine.0.max(1);
    start.feeds = feeds;
    start.slides = if feeds.slides() { context.slides } else { 0 };
    start.account = BoundedText::new(context.identity.0).unwrap_or(BoundedText::EMPTY);
    start.machine = BoundedText::new(context.identity.1).unwrap_or(BoundedText::EMPTY);
    let (memory, threads) = ceilings(saver, region.len(), context.machine);
    start.memory = memory;
    start.installed_memory = context.machine.1;
    start.threads = u32::try_from(threads).unwrap_or(1);
    if let Some(id) = &saver.id {
        for option in &saver.options {
            start.push_option(OptionValue {
                key: *option.key(),
                value: context.values.value(id, option),
            })?;
        }
    }
    Ok(start)
}

/// The address space and threads `saver` is held to, its region `len` bytes,
/// on a machine of `cores` cores holding `memory` bytes: its own thread, or
/// one per core for a saver that runs in parallel; room for its region, which
/// its scene draws straight into, and its heap, and the large ceiling's more
/// where it asks — never more than a share of the machine, however much it
/// asks for.
#[must_use]
pub fn ceilings(saver: &Saver, len: usize, (cores, memory): (u16, u64)) -> (u64, u64) {
    let region = u64::try_from(len).unwrap_or(u64::MAX);
    let large = if saver.large_memory {
        SAVER_LARGE_MEMORY_BYTES
    } else {
        0
    };
    let asked = region
        .saturating_add(SAVER_MEMORY_BYTES)
        .saturating_add(large);
    let threads = if saver.parallel {
        u64::from(cores.max(1))
    } else {
        1
    };
    (asked.min(memory / SAVER_MEMORY_SHARE), threads)
}

/// `band` as the saver protocol carries it: the two are one five-band
/// ladder, so every depth is one the protocol spells.
fn memory_band(band: PressureBand) -> MemoryBand {
    MemoryBand::new(band.depth()).unwrap_or(MemoryBand::NORMAL)
}

/// A black surface of `size`, or `None` when the heap will not give one.
fn black(size: (u32, u32)) -> Option<Surface> {
    let mut surface = Surface::new(size.0, size.1)?;
    surface.fill(Color::rgb(0, 0, 0));
    Some(surface)
}

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;

//! The desktop's screen capture: the one way anything takes a picture or a
//! recording of the screen (`plans/CAPTURE.md`).
//!
//! No application can capture, because no protocol asks for one: a capture
//! starts only from the user's chord or a click on the desktop's own capture
//! chrome. The controller owns that chrome — the capture bar, the selection
//! overlay, the countdown and the floating thumbnail — and decides what each
//! capture shows: the area asked for, its own chrome left out, the lock and
//! the authorisation sheet blanked. The compositor's capture tap composes it,
//! the capture file worker keeps a still, and `recorder.app`, hosted by
//! [`Recording`], encodes a recording — run as an administrator who allowed
//! it through the system's authorisation sheet, when its sound is one the
//! signed-in account may not record.
//!
//! While the bar, the overlay or the countdown is up it holds the seat as the
//! menu chain does, and Escape discards whatever it holds. A chord pressed
//! while it holds the seat cancels it; one pressed during a recording stops
//! the recording.

mod bar;
mod chord;
mod overlay;
mod plate;
mod recording;
mod still;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::home::{
    HOME_DESKTOP_DIR, HOME_FILES_DIR, HOME_FILES_PICTURES_DIR, HOME_FILES_VIDEOS_DIR,
};
use tairix_abi::input::KeyInput;
use tairix_abi::recorder_ipc::RecordSound;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_abi::window_ipc::{AppMenuItemId, AppMenuSymbol};
use tairix_controls::{ChainModel, ChainRow, MenuItem, MenuMark, PlatePlacement, PlateSide};
use tairix_log::{Event, EventId, Field, FieldValue, Level};
use tairix_wallpaper::{CaptureDuration, CaptureFolder, CaptureSettings, CaptureTimer};
use tairix_wm::{
    shadow_footprint, CaptureError, CaptureTap, Compositor, InputEvent, Rect, Surface, WindowId,
};

use tairix_taskbar::TaskbarResponse;

use crate::keep::{Keeping, Unkept, PNG_ENDING};
use crate::menu::{marked, ChainAnswer, ChainOutcome};
use crate::shell::{DesktopShell, ShellOutcome};

use bar::{BarAction, CaptureBar};
use overlay::{Picked, SelectOverlay};
use plate::{Countdown, Thumbnail};

pub use chord::{is_escape, is_space, CaptureChord, ChordAsk};
pub use recording::{
    administrator_request, claim_recording, settle_recording, take_output, Allowed, Lost,
    RecordEnd, RecordStep, RecorderHost, Recording, RecordingFile, StopReason, NOTHING_LOST,
};
pub use still::{
    compose_still, keep_still, still_density, sweep_captures, CaptureBuffer, ClipboardPng,
    StillKept, StillShot, SCREENSHOT_NAME,
};

/// A still was composed. Logged with what it took and where it goes.
pub const CAPTURE_TAKEN: EventId = EventId(20_037);

/// The exact message [`CAPTURE_TAKEN`] is logged with.
pub const CAPTURE_TAKEN_MESSAGE: &str = "screen captured";

/// A recording began. Logged with what it takes and the sound it carries.
pub const RECORDING_STARTED: EventId = EventId(20_038);

/// The exact message [`RECORDING_STARTED`] is logged with.
pub const RECORDING_STARTED_MESSAGE: &str = "screen recording started";

/// A recording ended. Logged with why, and what of it was lost.
pub const RECORDING_ENDED: EventId = EventId(20_039);

/// The exact message [`RECORDING_ENDED`] is logged with.
pub const RECORDING_ENDED_MESSAGE: &str = "screen recording ended";

/// A capture the user asked for was not taken. Logged with why.
pub const CAPTURE_REFUSED: EventId = EventId(20_040);

/// The exact message [`CAPTURE_REFUSED`] is logged with.
pub const CAPTURE_REFUSED_MESSAGE: &str = "screen capture refused";

/// A capture was given its name in its folder: a still or a recording, or what
/// a session ended mid-capture left, named at the next start. Logged with
/// which.
pub const CAPTURE_KEPT: EventId = EventId(20_042);

/// The exact message [`CAPTURE_KEPT`] is logged with.
pub const CAPTURE_KEPT_MESSAGE: &str = "screen capture kept";

/// What a kept capture was.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kept {
    /// A still.
    Still,
    /// A recording.
    Recording,
    /// What a session ended mid-capture left.
    Leftover,
}

impl Kept {
    /// How [`CAPTURE_KEPT`] names it, in its `how` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Still => "still",
            Self::Recording => "recording",
            Self::Leftover => "leftover",
        }
    }
}

/// Log that a capture was kept as `kept`.
pub fn log_kept(shell: &DesktopShell, kept: Kept) {
    log(
        shell,
        (Level::Info, CAPTURE_KEPT, CAPTURE_KEPT_MESSAGE),
        &[Field {
            key: "how",
            value: FieldValue::Str(kept.as_str()),
        }],
    );
}

/// What a recording is called before the moment it ended.
pub const RECORDING_NAME: &str = "Screen Recording";

/// Who the system's authorisation sheet names as asking for a recording's
/// sound: the desktop's own name for the task.
pub const RECORDING_REQUESTER: &str = "Screen Recording";

/// A recording's file ending.
pub const RECORDING_ENDING: &str = "webm";

/// What a capture takes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CaptureTarget {
    /// The whole screen.
    Screen,
    /// One window the user picks.
    Window,
    /// A rectangle the user drags out.
    Area,
}

impl CaptureTarget {
    /// The target's logged spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Window => "window",
            Self::Area => "area",
        }
    }
}

/// One of the six captures the bar offers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CaptureMode {
    /// Whether it records rather than takes a still.
    pub recording: bool,
    /// What it takes.
    pub target: CaptureTarget,
}

impl CaptureMode {
    /// The bar's modes, in the order it shows them.
    pub const ALL: [Self; 6] = [
        Self::still(CaptureTarget::Screen),
        Self::still(CaptureTarget::Window),
        Self::still(CaptureTarget::Area),
        Self::record(CaptureTarget::Screen),
        Self::record(CaptureTarget::Window),
        Self::record(CaptureTarget::Area),
    ];

    /// A still of `target`.
    #[must_use]
    pub const fn still(target: CaptureTarget) -> Self {
        Self {
            recording: false,
            target,
        }
    }

    /// A recording of `target`.
    #[must_use]
    pub const fn record(target: CaptureTarget) -> Self {
        Self {
            recording: true,
            target,
        }
    }
}

/// The folder under the user's home a capture is kept in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KeptFolder {
    /// `Desktop`.
    Desktop,
    /// `Home/Pictures`, for stills.
    Pictures,
    /// `Home/Videos`, for recordings.
    Videos,
}

impl KeptFolder {
    /// Every folder a capture may be kept in.
    pub const ALL: [Self; 3] = [Self::Desktop, Self::Pictures, Self::Videos];

    /// Where `save_to` keeps a still, or a recording when `recording`. A
    /// recording is never put on the clipboard, so that choice keeps it on the
    /// Desktop, as it does a still too large for the clipboard.
    #[must_use]
    pub const fn of(save_to: CaptureFolder, recording: bool) -> Self {
        match (save_to, recording) {
            (CaptureFolder::Home, false) => Self::Pictures,
            (CaptureFolder::Home, true) => Self::Videos,
            (CaptureFolder::Desktop | CaptureFolder::Clipboard, _) => Self::Desktop,
        }
    }

    /// Its folders under the home, outermost first.
    #[must_use]
    pub const fn folders(self) -> &'static [&'static str] {
        match self {
            Self::Desktop => &[HOME_DESKTOP_DIR],
            Self::Pictures => &[HOME_FILES_DIR, HOME_FILES_PICTURES_DIR],
            Self::Videos => &[HOME_FILES_DIR, HOME_FILES_VIDEOS_DIR],
        }
    }

    /// How a still, or a recording when `recording`, is kept here.
    #[must_use]
    pub const fn keeping(self, recording: bool) -> Keeping<'static> {
        if recording {
            Keeping {
                folders: self.folders(),
                what: RECORDING_NAME,
                ending: RECORDING_ENDING,
            }
        } else {
            Keeping {
                folders: self.folders(),
                what: SCREENSHOT_NAME,
                ending: PNG_ENDING,
            }
        }
    }
}

/// The surfaces a capture shows blanked, because they hold the user's
/// secrets.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Screened {
    /// The screen lock's window, while it is engaged.
    pub lock: Option<WindowId>,
    /// The authorisation sheet's window, while it is up.
    pub sheet: Option<WindowId>,
}

impl Screened {
    /// The windows as a capture tap lists them, or `None` where the heap
    /// refuses even that, so the capture is refused rather than shown
    /// unblanked.
    #[must_use]
    pub fn windows(self) -> Option<Vec<WindowId>> {
        let mut windows = Vec::new();
        windows.try_reserve_exact(2).ok()?;
        windows.extend(self.lock);
        windows.extend(self.sheet);
        Some(windows)
    }
}

/// A recording to start: what it takes and how.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordPlan {
    /// What it takes.
    pub target: CaptureTarget,
    /// The desktop rectangle recorded.
    pub area: Rect,
    /// The one window recorded, every other left out, when one was picked.
    pub window: Option<WindowId>,
    /// The sound recorded with it.
    pub sound: RecordSound,
    /// Whether the pointer is drawn.
    pub pointer: bool,
    /// Whether a press rings the pointer.
    pub clicks: bool,
    /// How long it runs before it stops itself, if it does.
    pub duration_ns: Option<u64>,
    /// Where it is kept.
    pub folder: KeptFolder,
}

/// What the desktop tells the user about a capture that did not go as asked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureNotice {
    /// A still could not be composed, memory being short.
    Uncomposed,
    /// The capture bar or the selection overlay could not be shown, memory
    /// being short.
    NoControls,
    /// What was chosen left the screen before it was taken: the window picked
    /// closed, or the area is no longer on the desktop.
    Gone {
        /// What was chosen.
        target: CaptureTarget,
        /// Whether it was to be recorded rather than taken as a still.
        recording: bool,
    },
    /// A recording was asked for while the last is still being finished.
    Finishing,
    /// A still too large for the clipboard was saved at this path instead.
    SavedInstead(String),
    /// A capture could not be kept.
    Unkept(Unkept),
    /// A recording ended short of what was asked.
    Lost(Lost),
}

impl CaptureNotice {
    /// The notice as the notification area shows it: its title and body.
    #[must_use]
    pub fn text(&self) -> (&'static str, String) {
        match self {
            Self::Uncomposed => (
                "Screenshot not taken",
                String::from("There was not enough memory to capture the screen."),
            ),
            Self::NoControls => (
                "Capture not available",
                String::from("There was not enough memory to show the capture controls."),
            ),
            Self::Gone { target, recording } => gone(*target, *recording),
            Self::Finishing => (
                "Screen recording not started",
                String::from("The last recording is still being saved."),
            ),
            Self::SavedInstead(path) => (
                "Screenshot saved instead",
                format!(
                    "It was too large for the clipboard, so it was saved as {}.",
                    path.rsplit('/').next().unwrap_or(path)
                ),
            ),
            Self::Unkept(why) => ("Capture not saved", format!("It was not saved: {why}.")),
            Self::Lost(lost) => ("Screen recording ended", format!("{lost}.")),
        }
    }
}

/// What is told of a capture whose `target` left the screen before it was
/// taken, a recording when `recording`.
fn gone(target: CaptureTarget, recording: bool) -> (&'static str, String) {
    let title = if recording {
        "Screen recording not started"
    } else {
        "Screenshot not taken"
    };
    let body = match (target, recording) {
        (CaptureTarget::Window, false) => "The window closed before it could be captured.",
        (CaptureTarget::Window, true) => "The window closed before it could be recorded.",
        (CaptureTarget::Screen | CaptureTarget::Area, _) => {
            "The area chosen is no longer on the screen."
        }
    };
    (title, String::from(body))
}

/// What a capture leaves the session program to carry out.
#[derive(Debug)]
pub enum CaptureWork {
    /// Keep a still, off the loop.
    Still(StillShot),
    /// Start a recording.
    Record(RecordPlan),
    /// Tell the user something did not go as asked.
    Notice(CaptureNotice),
}

/// Why a capture the user asked for was not taken.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Refusal {
    /// The bar or the overlay could not be shown.
    Controls,
    /// What was chosen left the screen.
    Gone,
    /// The still could not be composed.
    Uncomposed,
}

impl Refusal {
    /// The refusal as it is logged.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Controls | Self::Uncomposed => "memory",
            Self::Gone => "gone",
        }
    }
}

/// What a committed capture takes, resolved when it is taken.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Subject {
    Screen,
    Window { id: WindowId, shadow: bool },
    Area(Rect),
}

impl Subject {
    const fn target(self) -> CaptureTarget {
        match self {
            Self::Screen => CaptureTarget::Screen,
            Self::Window { .. } => CaptureTarget::Window,
            Self::Area(_) => CaptureTarget::Area,
        }
    }
}

/// A capture committed to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shot {
    subject: Subject,
    recording: bool,
    clipboard: bool,
}

/// A capture whose subject is being chosen in the overlay.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Plan {
    recording: bool,
    clipboard: bool,
    /// Whether the countdown runs once it is chosen: from the bar it does,
    /// from a chord it does not.
    timed: bool,
}

/// What of the capture holds the seat.
enum Holding {
    Idle,
    Bar(Box<CaptureBar>),
    Choosing { overlay: SelectOverlay, plan: Plan },
    Counting { countdown: Countdown, shot: Shot },
}

/// The Options menu's row ids: the group in the high bits and the place,
/// counted from one, in the low four.
const SAVE_GROUP: u16 = 1;
const TIMER_GROUP: u16 = 2;
const DURATION_GROUP: u16 = 3;
const SOUND_GROUP: u16 = 4;
const SHOW_GROUP: u16 = 5;
const SHOW_POINTER: usize = 0;
const SHOW_CLICKS: usize = 1;
const SHOW_THUMBNAIL: usize = 2;

/// Whether a recording runs, and whether it was asked to stop.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Runs {
    /// None runs, or the one that does is already stopped.
    No,
    /// One runs.
    Yes,
    /// One runs and was asked to stop: kept here rather than queued as work,
    /// so no shortage of memory can lose the ask.
    StopAsked,
}

/// The desktop's capture controller.
pub struct CaptureController {
    live: CaptureSettings,
    /// A choice made here that is still to be written.
    owed: bool,
    mode: CaptureMode,
    holding: Holding,
    runs: Runs,
    ringing: bool,
    screened: Screened,
    thumbnail: Option<Thumbnail>,
    menu: Option<(ChainModel, PlatePlacement)>,
    work: Vec<CaptureWork>,
}

impl CaptureController {
    /// A controller holding nothing, choosing as `settings` say.
    #[must_use]
    pub const fn new(settings: CaptureSettings) -> Self {
        Self {
            live: settings,
            owed: false,
            mode: CaptureMode::still(CaptureTarget::Screen),
            holding: Holding::Idle,
            runs: Runs::No,
            ringing: false,
            screened: Screened {
                lock: None,
                sheet: None,
            },
            thumbnail: None,
            menu: None,
            work: Vec::new(),
        }
    }

    /// The options in force.
    #[must_use]
    pub const fn settings(&self) -> &CaptureSettings {
        &self.live
    }

    /// Adopt `stored`, the options the store holds, unless a choice made here
    /// is still to be written or `publishing`: the live choice stands until
    /// its write has landed, and a refused one comes back as the store's.
    pub fn adopt(&mut self, stored: &CaptureSettings, publishing: bool) {
        if !self.owed && !publishing {
            self.live = *stored;
        }
    }

    /// The options chosen since this was last asked, to be written once.
    pub fn take_settled(&mut self) -> Option<CaptureSettings> {
        core::mem::take(&mut self.owed).then_some(self.live)
    }

    /// Whether the bar, the overlay or the countdown holds the seat.
    #[must_use]
    pub const fn holds_seat(&self) -> bool {
        !matches!(self.holding, Holding::Idle)
    }

    /// Note whether a recording runs and has not been asked to stop, so a
    /// chord stops it rather than starting another capture.
    pub fn set_recording(&mut self, recording: bool) {
        self.runs = match (recording, self.runs) {
            (false, _) => Runs::No,
            (true, Runs::StopAsked) => Runs::StopAsked,
            (true, Runs::No | Runs::Yes) => Runs::Yes,
        };
    }

    /// Ring the pointer at each press from now on, or stop: what a recording
    /// showing clicks asks.
    pub fn ring_clicks(&mut self, ring: bool) {
        self.ringing = ring;
    }

    /// Whether a press rings the pointer.
    #[must_use]
    pub const fn rings_clicks(&self) -> bool {
        self.ringing
    }

    /// Whether `outcome` is the capture's own — the recording indicator
    /// pressed — taking it if so: the recording that runs is finished.
    pub fn claims(&mut self, outcome: &ShellOutcome) -> bool {
        if !matches!(
            outcome,
            ShellOutcome::Taskbar(TaskbarResponse::StopScreenRecording)
        ) {
            return false;
        }
        self.ask_stop();
        true
    }

    /// The surfaces every capture shows blanked from now on.
    pub fn screen(&mut self, screened: Screened) {
        self.screened = screened;
    }

    /// What the capture left to carry out, in the order it was asked.
    pub fn take_work(&mut self) -> Vec<CaptureWork> {
        core::mem::take(&mut self.work)
    }

    /// Whether the recording that runs was asked to stop since this was last
    /// asked.
    pub fn take_stop(&mut self) -> bool {
        let asked = self.runs == Runs::StopAsked;
        if asked {
            self.runs = Runs::Yes;
        }
        asked
    }

    /// Ask the recording that runs, if one does, to stop.
    fn ask_stop(&mut self) {
        if self.runs == Runs::Yes {
            self.runs = Runs::StopAsked;
        }
    }

    /// The Options menu the bar asked for and where it opens, for the seat to
    /// open as its one chain.
    pub fn take_menu(&mut self) -> Option<(ChainModel, PlatePlacement)> {
        self.menu.take()
    }

    /// Take `chord`, pressed at `now_ns`.
    pub fn chord(
        &mut self,
        chord: CaptureChord,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if self.runs != Runs::No {
            self.ask_stop();
            return;
        }
        if self.holds_seat() {
            self.discard(shell, compositor);
            return;
        }
        let clipboard = chord.clipboard || self.live.save_to == CaptureFolder::Clipboard;
        match chord.ask {
            ChordAsk::Screen => {
                let shot = Shot {
                    subject: Subject::Screen,
                    recording: false,
                    clipboard,
                };
                self.take(shot, now_ns, shell, compositor);
            }
            ChordAsk::Area => {
                let plan = Plan {
                    recording: false,
                    clipboard,
                    timed: false,
                };
                self.choose(plan, CaptureTarget::Area, shell, compositor);
            }
            ChordAsk::Bar => match CaptureBar::open(self.mode, shell, compositor) {
                Some(bar) => self.holding = Holding::Bar(Box::new(bar)),
                None => self.refuse((CaptureTarget::Screen, false), Refusal::Controls, shell),
            },
        }
    }

    /// Take `event` while the capture holds the seat.
    pub fn pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let alt = shell.modifiers().alt;
        match &mut self.holding {
            Holding::Bar(bar) => {
                if let Some(action) = bar.pointer(event, now_ns, shell, compositor) {
                    self.act(action, now_ns, shell, compositor);
                }
            }
            Holding::Choosing { overlay, .. } => {
                if let Some(picked) = overlay.pointer(event, alt, compositor) {
                    self.picked(picked, now_ns, shell, compositor);
                }
            }
            Holding::Counting { .. } | Holding::Idle => {}
        }
    }

    /// Take `key` while the capture holds the seat.
    pub fn key(
        &mut self,
        key: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if is_escape(key) {
            self.discard(shell, compositor);
            return;
        }
        match &mut self.holding {
            Holding::Bar(bar) => {
                if let Some(action) = bar.key(key) {
                    self.act(action, now_ns, shell, compositor);
                }
            }
            Holding::Choosing { overlay, .. } if is_space(key) => overlay.toggle(compositor),
            Holding::Choosing { .. } | Holding::Counting { .. } | Holding::Idle => {}
        }
    }

    /// Put on screen what the events of a batch changed, once.
    pub fn settle(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        match &mut self.holding {
            Holding::Bar(bar) => bar.settle(shell, compositor),
            Holding::Choosing { overlay, .. } => overlay.settle(compositor),
            Holding::Counting { .. } | Holding::Idle => {}
        }
    }

    /// Resolve what has come due by `now_ns`: a countdown that ran out takes
    /// its capture, and a thumbnail shown long enough goes.
    pub fn tick(&mut self, now_ns: u64, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let due =
            matches!(&self.holding, Holding::Counting { countdown, .. } if countdown.due(now_ns));
        if due {
            if let Holding::Counting { countdown, shot } =
                core::mem::replace(&mut self.holding, Holding::Idle)
            {
                countdown.close(compositor);
                self.take(shot, now_ns, shell, compositor);
            }
        } else if let Holding::Counting { countdown, .. } = &mut self.holding {
            countdown.tick(now_ns, shell, compositor);
        }
        if self
            .thumbnail
            .as_ref()
            .is_some_and(|thumbnail| thumbnail.expired(now_ns))
        {
            if let Some(thumbnail) = self.thumbnail.take() {
                thumbnail.close(compositor);
            }
        }
    }

    /// The park the capture needs: shortened to the countdown's next second
    /// and the thumbnail's end.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        let park_ns = match &self.holding {
            Holding::Counting { countdown, .. } => countdown.park_deadline_ns(now_ns, park_ns),
            _ => park_ns,
        };
        self.thumbnail.as_ref().map_or(park_ns, |thumbnail| {
            thumbnail.park_deadline_ns(now_ns, park_ns)
        })
    }

    /// Discard everything the capture holds and shows: the lock, the
    /// authorisation sheet or the screensaver took the seat.
    pub fn abandon(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        self.discard(shell, compositor);
        self.menu = None;
        if let Some(thumbnail) = self.thumbnail.take() {
            thumbnail.close(compositor);
        }
    }

    /// Show `picture`, which the session already holds, as the floating
    /// thumbnail, if the options ask for one.
    pub fn show_thumbnail(
        &mut self,
        picture: &Surface,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) {
        if !self.live.thumbnail {
            return;
        }
        if let Some(shown) = self.thumbnail.take() {
            shown.close(compositor);
        }
        self.thumbnail = Thumbnail::show(picture, now_ns, shell, compositor);
    }

    /// What the Options menu answered: a row chosen changes that option, to be
    /// written once. One chosen with the menu kept up answers the rows the
    /// menu shows now.
    pub fn menu_answered(&mut self, answer: &ChainAnswer) -> Option<ChainModel> {
        let (ChainAnswer::Repeated(item) | ChainAnswer::Ended(ChainOutcome::Chosen(item))) = answer
        else {
            return None;
        };
        if let Some(next) = chosen(self.live, *item) {
            if next != self.live {
                self.live = next;
                self.owed = true;
            }
        }
        matches!(answer, ChainAnswer::Repeated(_))
            .then(|| options_menu(self.live, self.mode.recording))
    }

    fn act(
        &mut self,
        action: BarAction,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        match action {
            BarAction::Mode(mode) => self.mode = mode,
            BarAction::Options(anchor) => {
                let placement = PlatePlacement {
                    anchor,
                    side: PlateSide::Above,
                    gap: 0,
                };
                self.menu = Some((options_menu(self.live, self.mode.recording), placement));
            }
            BarAction::Commit => {
                let mode = self.mode;
                self.discard(shell, compositor);
                let clipboard = !mode.recording && self.live.save_to == CaptureFolder::Clipboard;
                if mode.target == CaptureTarget::Screen {
                    let shot = Shot {
                        subject: Subject::Screen,
                        recording: mode.recording,
                        clipboard,
                    };
                    self.commit(shot, true, now_ns, shell, compositor);
                } else {
                    let plan = Plan {
                        recording: mode.recording,
                        clipboard,
                        timed: true,
                    };
                    self.choose(plan, mode.target, shell, compositor);
                }
            }
            BarAction::Close => self.discard(shell, compositor),
        }
    }

    fn choose(
        &mut self,
        plan: Plan,
        target: CaptureTarget,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) {
        let at = shell.router().pointer();
        match SelectOverlay::open(target == CaptureTarget::Window, at, compositor) {
            Some(overlay) => self.holding = Holding::Choosing { overlay, plan },
            None => self.refuse((target, plan.recording), Refusal::Controls, shell),
        }
    }

    fn picked(
        &mut self,
        picked: Picked,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        match core::mem::replace(&mut self.holding, Holding::Idle) {
            Holding::Choosing { overlay, plan } => {
                overlay.close(compositor);
                let subject = match picked {
                    Picked::Area(rect) => Subject::Area(rect),
                    Picked::Window { id, shadow } => Subject::Window { id, shadow },
                };
                let shot = Shot {
                    subject,
                    recording: plan.recording,
                    clipboard: plan.clipboard,
                };
                self.commit(shot, plan.timed, now_ns, shell, compositor);
            }
            other => self.holding = other,
        }
    }

    /// Take `shot` at once, or after the countdown when it is `timed` and the
    /// options set one.
    fn commit(
        &mut self,
        shot: Shot,
        timed: bool,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let seconds = u64::from(self.live.timer.seconds());
        if timed && seconds > 0 {
            let due_ns = now_ns.saturating_add(seconds * u64::from(NANOS_PER_SEC));
            let countdown = Countdown::start(due_ns, shot.recording, now_ns, shell, compositor);
            self.holding = Holding::Counting { countdown, shot };
        } else {
            self.take(shot, now_ns, shell, compositor);
        }
    }

    fn take(
        &mut self,
        shot: Shot,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let target = shot.subject.target();
        let Some((area, only)) = subject_area(shot.subject, compositor) else {
            self.refuse((target, shot.recording), Refusal::Gone, shell);
            return;
        };
        if shot.recording {
            if let Some(thumbnail) = self.thumbnail.take() {
                thumbnail.close(compositor);
            }
            self.owe(CaptureWork::Record(RecordPlan {
                target,
                area,
                window: only,
                sound: self.live.sound,
                pointer: self.live.pointer,
                clicks: self.live.clicks,
                duration_ns: self
                    .live
                    .duration
                    .seconds()
                    .map(|seconds| u64::from(seconds) * u64::from(NANOS_PER_SEC)),
                folder: KeptFolder::of(self.live.save_to, true),
            }));
            return;
        }
        let composed = self
            .still_tap(area, only)
            .ok_or(CaptureError::OutOfMemory)
            .and_then(|tap| compose_still(compositor, tap));
        let Ok(pixels) = composed else {
            self.refuse((target, false), Refusal::Uncomposed, shell);
            return;
        };
        let to = if shot.clipboard { "clipboard" } else { "file" };
        log(
            shell,
            (Level::Info, CAPTURE_TAKEN, CAPTURE_TAKEN_MESSAGE),
            &[
                Field {
                    key: "target",
                    value: FieldValue::Str(target.as_str()),
                },
                Field {
                    key: "to",
                    value: FieldValue::Str(to),
                },
            ],
        );
        self.show_thumbnail(pixels.surface(), now_ns, shell, compositor);
        self.owe(CaptureWork::Still(StillShot {
            pixels,
            density: still_density(compositor.scale()),
            clipboard: shot.clipboard,
            folder: KeptFolder::of(self.live.save_to, false),
        }));
    }

    /// The tap a still of `area` is composed through: only its one window where
    /// it has one, every piece of capture chrome left out, the secret surfaces
    /// blanked.
    fn still_tap(&self, area: Rect, only: Option<WindowId>) -> Option<CaptureTap> {
        let mut exclude = Vec::new();
        exclude.try_reserve_exact(2).ok()?;
        exclude.extend(self.thumbnail.as_ref().map(Thumbnail::window));
        match &self.holding {
            Holding::Bar(bar) => exclude.push(bar.window()),
            Holding::Choosing { overlay, .. } => exclude.push(overlay.window()),
            Holding::Counting { countdown, .. } => exclude.extend(countdown.window()),
            Holding::Idle => {}
        }
        Some(CaptureTap {
            area,
            only,
            exclude,
            blank: self.screened.windows()?,
            pointer: self.live.pointer,
        })
    }

    fn discard(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        match core::mem::replace(&mut self.holding, Holding::Idle) {
            Holding::Bar(bar) => bar.close(shell, compositor),
            Holding::Choosing { overlay, .. } => overlay.close(compositor),
            Holding::Counting { countdown, .. } => countdown.close(compositor),
            Holding::Idle => {}
        }
    }

    /// Log and tell why the capture of `target`, a recording where the pair
    /// says so, was not taken.
    fn refuse(
        &mut self,
        (target, recording): (CaptureTarget, bool),
        refusal: Refusal,
        shell: &DesktopShell,
    ) {
        log(
            shell,
            (Level::Warn, CAPTURE_REFUSED, CAPTURE_REFUSED_MESSAGE),
            &[
                Field {
                    key: "target",
                    value: FieldValue::Str(target.as_str()),
                },
                Field {
                    key: "reason",
                    value: FieldValue::Str(refusal.as_str()),
                },
            ],
        );
        let notice = match refusal {
            Refusal::Controls => CaptureNotice::NoControls,
            Refusal::Gone => CaptureNotice::Gone { target, recording },
            Refusal::Uncomposed => CaptureNotice::Uncomposed,
        };
        self.owe(CaptureWork::Notice(notice));
    }

    /// Queue `work`; past what the heap will give it is dropped, and a still
    /// with it is wiped as it goes. A stop is never work, so is never lost.
    fn owe(&mut self, work: CaptureWork) {
        if self.work.try_reserve(1).is_ok() {
            self.work.push(work);
        }
    }

    #[cfg(test)]
    pub(crate) const fn holding_name(&self) -> &'static str {
        match self.holding {
            Holding::Idle => "idle",
            Holding::Bar(_) => "bar",
            Holding::Choosing { .. } => "choosing",
            Holding::Counting { .. } => "counting",
        }
    }

    #[cfg(test)]
    pub(crate) fn bar_part_centre(
        &self,
        part: bar::Part,
        compositor: &Compositor,
    ) -> Option<tairix_wm::Point> {
        match &self.holding {
            Holding::Bar(bar) => bar.part_centre(part, compositor),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn chrome_windows(&self) -> Vec<WindowId> {
        let mut windows: Vec<WindowId> = self
            .thumbnail
            .as_ref()
            .map(Thumbnail::window)
            .into_iter()
            .collect();
        match &self.holding {
            Holding::Bar(bar) => windows.push(bar.window()),
            Holding::Choosing { overlay, .. } => windows.push(overlay.window()),
            Holding::Counting { countdown, .. } => windows.extend(countdown.window()),
            Holding::Idle => {}
        }
        windows
    }
}

#[cfg(test)]
pub(crate) use bar::Part as BarPart;

/// The desktop rectangle `subject` covers now, and the one window it shows
/// alone where it is a window's, or `None` for a window that has gone or an
/// area off the screen.
fn subject_area(subject: Subject, compositor: &Compositor) -> Option<(Rect, Option<WindowId>)> {
    let screen = compositor.screen_rect();
    let (area, only) = match subject {
        Subject::Screen => (screen, None),
        Subject::Area(rect) => (rect.intersection(&screen), None),
        Subject::Window { id, shadow } => {
            let bounds = compositor.window(id)?.bounds();
            let area = if shadow {
                shadow_footprint(bounds, compositor.scale(), compositor.theme())
            } else {
                bounds
            };
            (area.intersection(&screen), Some(id))
        }
    };
    (!area.is_empty()).then_some((area, only))
}

fn log(shell: &DesktopShell, (level, id, message): (Level, EventId, &str), fields: &[Field<'_>]) {
    tairix_log::log(
        shell.audit(),
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

/// The id of the option row at `place` of `group`.
fn option_id(group: u16, place: usize) -> Option<AppMenuItemId> {
    let place = u16::try_from(place).ok().filter(|place| *place < 15)?;
    AppMenuItemId::new((group << 4) | (place + 1)).ok()
}

/// `live` with the option row `item` chosen, or `None` for a row the menu
/// never declared.
fn chosen(mut live: CaptureSettings, item: AppMenuItemId) -> Option<CaptureSettings> {
    let raw = item.get();
    let place = usize::from(raw & 0xF).checked_sub(1)?;
    match raw >> 4 {
        SAVE_GROUP => live.save_to = *CaptureFolder::ALL.get(place)?,
        TIMER_GROUP => live.timer = *CaptureTimer::ALL.get(place)?,
        DURATION_GROUP => live.duration = *CaptureDuration::ALL.get(place)?,
        SOUND_GROUP => live.sound = *RecordSound::ALL.get(place)?,
        SHOW_GROUP => match place {
            SHOW_POINTER => live.pointer = !live.pointer,
            SHOW_CLICKS => live.clicks = !live.clicks,
            SHOW_THUMBNAIL => live.thumbnail = !live.thumbnail,
            _ => return None,
        },
        _ => return None,
    }
    Some(live)
}

/// The Options menu over `live`, for a still or, when `recording`, a
/// recording: a recording's rows add its duration, its sound and its clicks.
fn options_menu(live: CaptureSettings, recording: bool) -> ChainModel {
    let mut model = ChainModel::new("Options");
    push_save_to(&mut model, live, recording);
    let timer = model.push(ChainRow::submenu(
        AppMenuSymbol::Clock,
        MenuItem::new("Timer"),
    ));
    for (place, choice) in CaptureTimer::ALL.into_iter().enumerate() {
        let label = match choice {
            CaptureTimer::Off => "None",
            CaptureTimer::Seconds5 => "5 Seconds",
            CaptureTimer::Seconds10 => "10 Seconds",
        };
        let row = (label, AppMenuSymbol::Clock);
        let ids = (TIMER_GROUP, place);
        push_choice(&mut model, timer, ids, row, live.timer == choice);
    }
    if recording {
        push_recording_choices(&mut model, live);
    }
    push_shows(&mut model, live, recording);
    model
}

/// The *Save to* submenu: a recording goes to Videos where a still goes to
/// Pictures, and never to the clipboard.
fn push_save_to(model: &mut ChainModel, live: CaptureSettings, recording: bool) {
    let save = model.push(ChainRow::submenu(
        AppMenuSymbol::Folder,
        MenuItem::new("Save to"),
    ));
    let kept = KeptFolder::of(live.save_to, recording);
    for (place, folder) in CaptureFolder::ALL.into_iter().enumerate() {
        let (label, symbol, chosen) = match folder {
            CaptureFolder::Desktop => (
                "Desktop",
                AppMenuSymbol::Folder,
                kept == KeptFolder::Desktop,
            ),
            CaptureFolder::Home if recording => {
                ("Videos", AppMenuSymbol::Folder, kept == KeptFolder::Videos)
            }
            CaptureFolder::Home => (
                "Pictures",
                AppMenuSymbol::Folder,
                kept == KeptFolder::Pictures,
            ),
            CaptureFolder::Clipboard if recording => continue,
            CaptureFolder::Clipboard => (
                "Clipboard",
                AppMenuSymbol::Copy,
                live.save_to == CaptureFolder::Clipboard,
            ),
        };
        let ids = (SAVE_GROUP, place);
        push_choice(model, save, ids, (label, symbol), chosen);
    }
}

/// A recording's *Duration* and *Sound* submenus.
fn push_recording_choices(model: &mut ChainModel, live: CaptureSettings) {
    let duration = model.push(ChainRow::submenu(
        AppMenuSymbol::Clock,
        MenuItem::new("Duration"),
    ));
    for (place, choice) in CaptureDuration::ALL.into_iter().enumerate() {
        let label = match choice {
            CaptureDuration::Unlimited => "Unlimited",
            CaptureDuration::Seconds10 => "10 Seconds",
            CaptureDuration::Seconds30 => "30 Seconds",
            CaptureDuration::Minute1 => "1 Minute",
            CaptureDuration::Minutes5 => "5 Minutes",
            CaptureDuration::Minutes10 => "10 Minutes",
        };
        let row = (label, AppMenuSymbol::Clock);
        let ids = (DURATION_GROUP, place);
        push_choice(model, duration, ids, row, live.duration == choice);
    }
    let sound = model.push(ChainRow::submenu(
        AppMenuSymbol::AudioOutput,
        MenuItem::new("Sound"),
    ));
    for (place, choice) in RecordSound::ALL.into_iter().enumerate() {
        let label = match choice {
            RecordSound::None => "None",
            RecordSound::Microphone => "Microphone",
            RecordSound::Desktop => "Desktop Sound",
            RecordSound::Both => "Microphone and Desktop Sound",
        };
        let row = (label, AppMenuSymbol::AudioOutput);
        let ids = (SOUND_GROUP, place);
        push_choice(model, sound, ids, row, live.sound == choice);
    }
}

/// The *Show* boxes, grouped below the submenus; *Show Clicks* only for a
/// recording.
fn push_shows(model: &mut ChainModel, live: CaptureSettings, recording: bool) {
    let shows = [
        (SHOW_POINTER, "Show Pointer", live.pointer, true),
        (SHOW_CLICKS, "Show Clicks", live.clicks, recording),
        (
            SHOW_THUMBNAIL,
            "Show Floating Thumbnail",
            live.thumbnail,
            true,
        ),
    ];
    let mut first = true;
    for (place, label, on, offered) in shows {
        let Some(id) = option_id(SHOW_GROUP, place).filter(|_| offered) else {
            continue;
        };
        let item = MenuItem::new(label);
        let item = if on {
            item.with_mark(MenuMark::Check)
        } else {
            item
        };
        let row = ChainRow::item(id, AppMenuSymbol::Visible, item).repeatable();
        model.push(if core::mem::take(&mut first) {
            row.grouped()
        } else {
            row
        });
    }
}

/// File the radio row `label` at `place` of `group` under `parent`, marked
/// when it is the one `chosen`.
fn push_choice(
    model: &mut ChainModel,
    parent: usize,
    (group, place): (u16, usize),
    (label, symbol): (&str, AppMenuSymbol),
    chosen: bool,
) {
    if let Some(id) = option_id(group, place) {
        let item = marked(MenuItem::new(label), chosen);
        model.push(ChainRow::item(id, symbol, item).under(parent).repeatable());
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;

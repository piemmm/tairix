//! One machine's window: its worker's session and the requests waiting for
//! it, its keyboard, its media, its sound, its picture and its status strip.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::window_ipc::KeyboardMode;
use tairix_controls::{StatusBand, StatusTone};
use tairix_crt::screen::{fit, Drawing, Picture, Scaling, DOT};
use tairix_crt::Tube;
use tairix_geometry::{Rect, Region, Scale};
use tairix_machines::menu::{Effect, Followed};
use tairix_openmicro::front::keys::{self, Keyboard, Mapping, Press};
use tairix_openmicro::front::media::{Disc, FileWork, Handover, Holding, Rack, Saving, Tape};
use tairix_openmicro::front::menu::{Offered, Shown};
use tairix_openmicro::front::pacing::{Next, Pacer};
use tairix_openmicro::front::picture::{expand, palette};
use tairix_openmicro::front::{events, LEAST_PICTURE, OPEN_PICTURE};
use tairix_openmicro::machine::bbc::fdc::PhysicalDrive;
use tairix_openmicro::machine::canvas::{CANVAS_HEIGHT, CANVAS_WIDTH};
use tairix_openmicro::machine::{bbc, electron, Model, CLOCK};
use tairix_openmicro::worker::protocol::{
    session_bounds, Change, Input, Lights, Loaded, Media, ModelKey, Ran, Refusal as Refused, Reply,
    Request, MAX_INPUTS, SHARE_BYTES,
};
use tairix_openmicro::worker::MachineClient;
use tairix_raster::resample::Region as Target;
use tairix_raster::{Color, Surface};
use tairix_rt::LogSink;
use tairix_sandbox::rt::{RtSessionLauncher, SessionMembers};
use tairix_sandbox::session::Share;
use tairix_sandbox::supervise::send_waiting;
use tairix_sandbox::{SupervisedSession, WorkerMemory};
use tairix_theme::Theme;
use tairix_window::app::{self, Fitted, LazyPool, WindowPane};
use tairix_window::{Desktop, Owed, Repaint, WindowSizing};

use crate::files::Speaker;
use crate::program::{record, unwatch_sound, Client, APP_NAME};
use crate::store::Images;

/// The least a key is held, in cycles: long enough for two keyboard scans.
const LEAST_HOLD: u64 = 50_000;
/// Each palette slot's colour, made once.
const PALETTE: [[u8; DOT]; 256] = palette();
/// Cycles from one pasted character's key to the next's.
const TYPE_GAP: u64 = 100_000;
/// The most pasted text taken at once.
const MOST_PASTE: usize = 4_096;

/// The session a machine's worker runs in, replaced when it fails.
pub type Session = SupervisedSession<RtSessionLauncher, LogSink>;

/// Where a machine stands in its window's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Life {
    /// Its images are being read, for the start `ticket` names.
    Starting(u64),
    /// It runs.
    Running,
    /// It was asked for its snapshot, by the deadline given.
    Suspending(u64),
    /// It is shutting down: its media are being taken out, by the deadline
    /// given, and then what they offer to save answered.
    ShuttingDown(Option<u64>),
}

/// What the next paint owes beyond what the picture redraws of itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Owing {
    #[default]
    Nothing,
    /// The status strip.
    Strip,
    /// The whole window, cleared and the picture drawn whole.
    Whole,
}

/// A machine's keyboard, as its model has it.
enum Keys {
    ModelB(Keyboard<bbc::keyboard::Key>),
    Electron(Keyboard<electron::keyboard::Key>),
}

fn changes<K: Copy>(presses: &[Press<K>], key: fn(K) -> ModelKey) -> Vec<Change> {
    presses
        .iter()
        .map(|press| match *press {
            Press::Key(k, down) => Change::Key(key(k), down),
            Press::Break(down) => Change::Break(down),
        })
        .collect()
}

impl Keys {
    const fn new(model: Model, mapping: Mapping) -> Self {
        match model {
            Model::ModelB => Self::ModelB(Keyboard::new(mapping)),
            Model::Electron => Self::Electron(Keyboard::new(mapping)),
        }
    }

    fn edge(&mut self, input: &tairix_abi::input::KeyInput) -> Vec<Change> {
        match self {
            Self::ModelB(keyboard) => {
                let mut out = Vec::new();
                keyboard.edge(input, &mut out);
                changes(&out, ModelKey::ModelB)
            }
            Self::Electron(keyboard) => {
                let mut out = Vec::new();
                keyboard.edge(input, &mut out);
                changes(&out, ModelKey::Electron)
            }
        }
    }

    fn release_all(&mut self) -> Vec<Change> {
        match self {
            Self::ModelB(keyboard) => {
                let mut out = Vec::new();
                keyboard.release_all(&mut out);
                changes(&out, ModelKey::ModelB)
            }
            Self::Electron(keyboard) => {
                let mut out = Vec::new();
                keyboard.release_all(&mut out);
                changes(&out, ModelKey::Electron)
            }
        }
    }

    fn set_mapping(&mut self, mapping: Mapping) -> Vec<Change> {
        match self {
            Self::ModelB(keyboard) => {
                let mut out = Vec::new();
                keyboard.set_mapping(mapping, &mut out);
                changes(&out, ModelKey::ModelB)
            }
            Self::Electron(keyboard) => {
                let mut out = Vec::new();
                keyboard.set_mapping(mapping, &mut out);
                changes(&out, ModelKey::Electron)
            }
        }
    }

    const fn mapping(&self) -> Mapping {
        match self {
            Self::ModelB(keyboard) => keyboard.mapping(),
            Self::Electron(keyboard) => keyboard.mapping(),
        }
    }

    /// The key that types `ch` and SHIFT where it needs it.
    fn typing(&self, ch: char, caps_lock: bool) -> Option<(ModelKey, ModelKey, bool)> {
        match self {
            Self::ModelB(_) => keys::typing::<bbc::keyboard::Key>(ch, caps_lock).map(|(k, s)| {
                (
                    ModelKey::ModelB(k),
                    ModelKey::ModelB(bbc::keyboard::Key::SHIFT),
                    s,
                )
            }),
            Self::Electron(_) => {
                keys::typing::<electron::keyboard::Key>(ch, caps_lock).map(|(k, s)| {
                    (
                        ModelKey::Electron(k),
                        ModelKey::Electron(electron::keyboard::Key::SHIFT),
                        s,
                    )
                })
            }
        }
    }
}

/// The number a key is counted under for its least hold.
const fn key_number(key: ModelKey) -> usize {
    match key {
        ModelKey::ModelB(key) => (key.number() & 0x7F) as usize,
        ModelKey::Electron(key) => key.number() as usize,
    }
}

/// A request waiting for the machine, owning what it carries.
pub enum Pending {
    /// Build the machine from its images.
    Configure,
    /// A disc into a drive, its record taking the drive once the disc it
    /// replaces is dealt with.
    InsertDisc {
        /// The drive.
        drive: PhysicalDrive,
        /// Whether the machine may not write it.
        write_protected: bool,
        /// The disc.
        disc: Disc,
    },
    /// A drive's disc out.
    EjectDisc(PhysicalDrive),
    /// A tape into the deck.
    InsertTape(Tape),
    /// The deck's tape out.
    EjectTape,
    /// The tape wound back.
    RewindTape,
    /// RECORD down or up.
    RecordTape(bool),
    /// The machine off and on.
    PowerOn,
    /// The machine's snapshot taken.
    TakeSnapshot,
    /// The snapshot a suspend keeps taken.
    SuspendSnapshot,
    /// A snapshot restored.
    RestoreSnapshot(Vec<u8>),
    /// The suspended state restored as the machine resumes.
    Resume(Vec<u8>),
    /// The filed discs collected.
    CollectDiscs,
}

/// What became of a reply, for the application to act on.
pub enum Event {
    /// Nothing more.
    Nothing,
    /// A snapshot taken to be saved.
    Snapshot(Vec<u8>),
    /// The snapshot a suspend keeps.
    Suspended(Vec<u8>),
    /// A disc or tape to write back, or a file let go.
    Files(Vec<FileWork>),
    /// The suspended state the machine resumes from is restored, and the
    /// files of what that took out are to be written and let go.
    Resumed(Vec<FileWork>),
    /// The snapshot a suspend asked for was refused, and why.
    Unsaved(&'static str),
    /// The suspended state was refused, and why; the machine runs from
    /// power-on.
    NotResumed {
        /// What was not done.
        what: &'static str,
        /// Why.
        detail: &'static str,
    },
    /// The machine refused a request, and why, and the file of what it
    /// carried is to be let go.
    Refused {
        /// What was not done.
        what: &'static str,
        /// Why.
        detail: &'static str,
        /// The files to let go.
        work: Vec<FileWork>,
    },
}

/// A machine and its window.
pub struct MachineWindow {
    /// Its number.
    pub id: u32,
    /// Where it stands.
    pub life: Life,
    /// Its name, which titles its window.
    pub name: String,
    /// Its model.
    pub model: Model,
    /// Its wait-set place.
    pub slot: usize,
    /// The start it was opened for.
    pub ticket: u64,
    pane: WindowPane,
    surface: Surface,
    owed: Owed,
    /// What the next paint presents, kept to reuse its storage.
    damage: Region,
    owing: Owing,
    picture: Picture,
    session: Option<Session>,
    members: SessionMembers,
    share: Option<Arc<dyn Share>>,
    machine: MachineClient,
    asked: Option<Pending>,
    pending: VecDeque<Pending>,
    inputs: VecDeque<(u64, Change)>,
    timeline: u64,
    pressed_at: [u64; keys::KEY_NUMBERS],
    pacer: Pacer,
    keys: Keys,
    images: Option<Box<Images>>,
    /// Its sound stream.
    pub sound: Option<Speaker>,
    scratch: Vec<u8>,
    slots: Vec<u8>,
    /// Its media and the picker.
    pub rack: Rack,
    /// The file a picker picked, held while its name is asked.
    pub picked: Option<(u64, bool)>,
    lights: Lights,
    drives: [bool; 2],
    loaded: Loaded,
    /// The menu up over its window.
    pub menu: Option<Followed<()>>,
    message: Option<(String, StatusTone)>,
    full_screen: bool,
    /// Its sound's level.
    pub volume: u16,
    /// Whether a tape loads as fast as the machine can run.
    pub fast_tape: bool,
    /// What its folder offers.
    pub offered: Offered,
    /// Whether it was ever run, so a stop keeps its last screen.
    pub ran: bool,
    running_from: Option<u64>,
}

/// The window's opening size and the least it takes, a strip beneath the
/// picture.
fn window_sizing(
    desktop: &Desktop,
    theme: &Theme,
) -> (tairix_abi::driver::display::DisplayMode, WindowSizing) {
    let strip = StatusBand::new(theme, desktop.scale()).height();
    let (width, height) = desktop.window_size(OPEN_PICTURE.0, OPEN_PICTURE.1);
    let least = desktop.window_size(LEAST_PICTURE.0, LEAST_PICTURE.1);
    let mode = app::mode_for(width, height.saturating_add(strip));
    let sizing = WindowSizing::Resizable {
        min_width_px: least.0,
        min_height_px: least.1.saturating_add(strip),
        max_width_px: 0,
        max_height_px: 0,
    };
    (mode, sizing)
}

impl MachineWindow {
    /// The window of machine `id`, called `name`, opened and starting, its
    /// keyboard read raw.
    ///
    /// # Errors
    ///
    /// Why the window could not be opened, as the user is told.
    #[allow(clippy::too_many_arguments)] // Each is one fact of the machine the window shows.
    pub fn open(
        client: &mut Client,
        (endpoint, set): (u64, u64),
        (desktop, theme): (&Desktop, &Theme),
        machine: &tairix_openmicro::front::library::Machine,
        (slot, ticket): (usize, u64),
        tokens: (u64, u64),
    ) -> Result<Self, String> {
        let (mode, sizing) = window_sizing(desktop, theme);
        let surface = Surface::new(mode.width_px, mode.height_px)
            .ok_or_else(|| String::from("there is not enough memory for its window"))?;
        let mut picture = Picture::new(machine.tube);
        picture.screen_mut().set_scaling(machine.scaling);
        let (Ok(across), Ok(down)) = (u32::try_from(CANVAS_WIDTH), u32::try_from(CANVAS_HEIGHT))
        else {
            return Err(String::from("its picture is too large"));
        };
        if !picture.screen_mut().reshape(across, down) {
            return Err(String::from("there is not enough memory for its picture"));
        }
        let pane = WindowPane::open(client, endpoint, &mode, &machine.name, sizing)
            .map_err(|err| format!("the desktop would not open its window ({err})"))?;
        if let Err(err) = client.set_keyboard_mode(pane.id(), KeyboardMode::Raw) {
            app::report(
                APP_NAME,
                format_args!("the keyboard cannot be read raw ({err}); keys type text"),
            );
        }
        let model = machine.model();
        let mut window = Self {
            id: machine.id,
            life: Life::Starting(ticket),
            name: machine.name.clone(),
            model,
            slot,
            ticket,
            pane,
            surface,
            owed: Owed::new(),
            damage: Region::new(),
            owing: Owing::Nothing,
            picture,
            session: None,
            members: SessionMembers::new(set, tokens.0, tokens.1),
            share: None,
            machine: MachineClient::new(),
            asked: None,
            pending: VecDeque::new(),
            inputs: VecDeque::new(),
            timeline: 0,
            pressed_at: [0; keys::KEY_NUMBERS],
            pacer: Pacer::new(),
            keys: Keys::new(model, machine.keyboard),
            images: None,
            sound: None,
            scratch: Vec::new(),
            slots: alloc::vec![0; CANVAS_WIDTH],
            rack: Rack::new(),
            picked: None,
            lights: Lights::default(),
            drives: [false; 2],
            loaded: Loaded::default(),
            menu: None,
            message: Some((String::from("Starting\u{2026}"), StatusTone::Muted)),
            full_screen: false,
            volume: machine.volume,
            fast_tape: machine.fast_tape,
            offered: Offered::default(),
            ran: false,
            running_from: None,
        };
        window.owe_whole();
        Ok(window)
    }

    /// Its window's id.
    #[must_use]
    pub fn window_id(&self) -> u64 {
        self.pane.id()
    }

    /// The pane, given up as the window is held open while it is kept.
    pub fn into_pane(self) -> WindowPane {
        self.pane
    }

    /// The images read: the worker started and the machine built from them,
    /// resuming from `resume` where it was suspended.
    pub fn started(
        &mut self,
        images: Box<Images>,
        resume: Option<(tairix_machines::state::LastScreen, Vec<u8>)>,
        now: u64,
    ) -> Result<(), String> {
        let share = core::num::NonZeroUsize::new(SHARE_BYTES)
            .ok_or_else(|| String::from("its picture has no size"))?;
        let bounds = session_bounds().map_err(|err| format!("{err:?}"))?;
        let launcher = RtSessionLauncher::own_binary(WorkerMemory::Documents).sharing(share);
        let mut session = SupervisedSession::new(launcher, bounds, LogSink);
        let _ = session.start(now);
        self.share = session.share().cloned();
        self.session = Some(session);
        self.images = Some(images);
        self.life = Life::Running;
        self.message = None;
        self.ask(Pending::Configure);
        // Queued with the build, so whatever is put in later goes in after.
        if let Some((screen, kept)) = resume {
            self.show_kept(&screen);
            self.ask(Pending::Resume(kept));
        }
        self.owe_strip();
        Ok(())
    }

    /// The last screen a suspended machine left, shown until it runs.
    fn show_kept(&mut self, screen: &tairix_machines::state::LastScreen) {
        let screen_mut = self.picture.screen_mut();
        if screen_mut.size() == (screen.width, screen.height) {
            for (row, line) in (0u32..).zip(screen.pixels.chunks_exact(screen.width as usize * DOT))
            {
                if let Some(dots) = screen_mut.row_mut(row) {
                    dots.copy_from_slice(line);
                }
            }
        }
        self.owe_whole();
    }

    /// Its last screen, to be kept.
    #[must_use]
    pub fn last_screen(&self) -> Option<tairix_machines::state::LastScreen> {
        tairix_machines::state::LastScreen::of(self.picture.screen())
    }

    fn mode(&self) -> tairix_abi::driver::display::DisplayMode {
        *self.pane.mode()
    }

    /// The window's picture and status strip.
    fn layout(&self, (scale, theme): (Scale, &Theme)) -> Option<(Target, Rect)> {
        let mode = self.mode();
        let band = StatusBand::new(theme, scale);
        let strip = band.height().min(mode.height_px);
        let room = mode.height_px - strip;
        let picture = fit(mode.width_px, room)?;
        Some((
            picture,
            Rect::new(0, i32::try_from(room).ok()?, mode.width_px, strip),
        ))
    }

    /// `text` shown in the status strip, and recorded.
    pub fn say(&mut self, text: impl Into<String>, tone: StatusTone) {
        let text = text.into();
        app::report(APP_NAME, &text);
        self.message = Some((text, tone));
        self.owe_strip();
    }

    fn owe_strip(&mut self) {
        if self.owing == Owing::Nothing {
            self.owing = Owing::Strip;
        }
    }

    /// The whole window owed again.
    pub fn owe_whole(&mut self) {
        self.owing = Owing::Whole;
        self.picture.invalidate();
    }

    /// A request for the machine, behind those waiting.
    pub fn ask(&mut self, pending: Pending) {
        self.pending.push_back(pending);
    }

    /// Whether the machine's worker is live.
    #[must_use]
    pub fn live(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_live)
    }

    /// Ask for the next request waiting, or the slice the pace asks for.
    pub fn pump(&mut self, now: u64) {
        if !self.machine.is_idle() || !self.live() {
            return;
        }
        let collect = self.rack.filed();
        while let Some(pending) = self.pending.pop_front() {
            if let Some(request) = request_of(&pending, self.images.as_deref(), collect) {
                if self.machine.ask(&request).is_ok() {
                    self.asked = Some(pending);
                }
                return;
            }
        }
        if self.images.is_none() || self.life != Life::Running {
            return;
        }
        let hurried = self.fast_tape && self.lights.motor;
        let ring = self.sound.as_mut().and_then(Speaker::ring);
        if let Next::Run { cycles, sound } = self.pacer.next(now, ring, hurried) {
            let end = self.timeline + u64::from(cycles);
            let mut inputs = Vec::new();
            while let Some(&(at, change)) = self.inputs.front() {
                if at >= end || inputs.len() == MAX_INPUTS {
                    break;
                }
                self.inputs.pop_front();
                let at = u32::try_from(at.saturating_sub(self.timeline)).unwrap_or(0);
                inputs.push(Input { at, change });
            }
            let request = Request::Run {
                cycles,
                sound: sound && self.sound.is_some(),
                inputs,
            };
            if self.machine.ask(&request).is_ok() {
                self.timeline = end;
            }
        }
    }

    /// Send what waits, and arm the session's readiness.
    pub fn sync(&mut self, now: u64) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let _ = send_waiting(&mut self.machine, session, now);
        if let Err(err) = self.members.sync(
            session.descriptors(),
            session.wants_read(),
            session.wants_write(),
        ) {
            app::report(
                APP_NAME,
                format_args!("a machine's session cannot be waited on ({err})"),
            );
        }
    }

    /// Serve the session as its pipes are ready: what it can take, what it
    /// answered, and the loss of a worker that died. A read is made only on
    /// readiness, as it blocks.
    pub fn serve(&mut self, now: u64, (readable, writable): (bool, bool)) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let Some(session) = self.session.as_mut() else {
            return frames;
        };
        let mut lost = None;
        if writable && session.wants_write() {
            if let Err(err) = session.on_writable(now) {
                lost = Some(format!("{err:?}"));
            }
        }
        if readable && session.wants_read() {
            if let Err(err) = session.on_readable(now) {
                lost = Some(format!("{err:?}"));
            }
        }
        if let Some(why) = lost {
            self.lost(&why);
        }
        loop {
            let Some(session) = self.session.as_mut() else {
                return frames;
            };
            let machine = &mut self.machine;
            let got = session.recv(now, |frame| match machine.answer(frame) {
                Ok(_) => Ok(frame.to_vec()),
                Err(why) => Err(why),
            });
            match got {
                Ok(Some(Ok(frame))) => {
                    if frames.try_reserve(1).is_ok() {
                        frames.push(frame);
                    }
                }
                Ok(Some(Err(why))) => {
                    session.condemn(now, why);
                    self.lost(why);
                }
                Ok(None) => break,
                Err(err) => {
                    self.lost(&format!("{err:?}"));
                    break;
                }
            }
        }
        frames
    }

    /// The worker is gone; a replacement starts from power-on.
    fn lost(&mut self, why: &str) {
        if self.share.take().is_some() {
            record(events::WORKER_REPLACED, events::WORKER_REPLACED_MESSAGE);
            self.say(
                format!("The machine stopped ({why}); it is starting again."),
                StatusTone::Warning,
            );
        }
        self.machine.restart();
        self.asked = None;
        self.pending.clear();
        self.inputs.clear();
        self.pressed_at = [0; keys::KEY_NUMBERS];
        self.loaded = Loaded::default();
    }

    /// A replacement worker, once its deadline comes: the machine built
    /// again, its media put back.
    pub fn restart_due(&mut self, now: u64) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if self.life != Life::Running || session.restart_deadline().is_none_or(|at| at > now) {
            return;
        }
        if session.start(now).is_none() {
            return;
        }
        self.share = session.share().cloned();
        self.timeline = 0;
        self.ask(Pending::Configure);
        for drive in PhysicalDrive::ALL {
            if let Some(disc) = self.rack.disc(drive).cloned() {
                self.ask(Pending::InsertDisc {
                    drive,
                    write_protected: disc.medium.is_none(),
                    disc,
                });
            }
        }
        if let Some(tape) = self.rack.tape().cloned() {
            self.ask(Pending::InsertTape(tape));
        }
    }

    /// When the window next wants the loop: a replacement worker, the tube's
    /// next step, or the pace's next slice.
    pub fn due(&mut self, now: u64) -> Option<u64> {
        let restart = self.session.as_ref().and_then(Session::restart_deadline);
        let tube = self.picture.due();
        let paced = (self.machine.is_idle()
            && self.pending.is_empty()
            && self.images.is_some()
            && self.life == Life::Running)
            .then(|| {
                let hurried = self.fast_tape && self.lights.motor;
                let ring = self.sound.as_mut().and_then(Speaker::ring);
                let mut pacer = self.pacer;
                match pacer.next(now, ring, hurried) {
                    Next::At(at) => Some(at),
                    Next::Run { .. } | Next::Wait => None,
                }
            })
            .flatten();
        let suspend = match self.life {
            Life::Suspending(by) | Life::ShuttingDown(Some(by)) => Some(by),
            Life::Starting(_) | Life::Running | Life::ShuttingDown(None) => None,
        };
        [restart, tube, paced, suspend].into_iter().flatten().min()
    }

    /// The tube's animation stepped, where it is due; the next paint draws
    /// what that moved.
    pub fn step(&mut self, now: u64) {
        self.picture.step(now);
    }

    /// What a reply from the worker says.
    pub fn answered(&mut self, frame: &[u8]) -> Event {
        let asked = self.asked.take();
        let Ok(reply) = Reply::decode(frame) else {
            return Event::Nothing;
        };
        match reply {
            Reply::Ran(ran) => {
                self.ran(&ran);
                Event::Nothing
            }
            Reply::Media(media) => self.media(&media, asked),
            Reply::Snapshot(snapshot) => match asked {
                Some(Pending::SuspendSnapshot) => Event::Suspended(snapshot.to_vec()),
                _ => Event::Snapshot(snapshot.to_vec()),
            },
            Reply::Refused(refusal) => self.refused(refusal, asked),
            Reply::Done => {
                if matches!(asked, Some(Pending::Configure | Pending::PowerOn)) {
                    self.running_from = Some(self.timeline);
                    self.owe_whole();
                }
                Event::Nothing
            }
        }
    }

    fn refused(&mut self, refusal: Refused, asked: Option<Pending>) -> Event {
        let mut work = Vec::new();
        // A machine never built has nothing in it to take out.
        if matches!(self.life, Life::ShuttingDown(_)) && refusal == Refused::Unconfigured {
            return Event::Files(work);
        }
        let detail = detail_of(refusal);
        let resuming = matches!(asked, Some(Pending::Resume(_)));
        let what = match asked {
            Some(Pending::SuspendSnapshot) => return Event::Unsaved(detail),
            Some(Pending::Configure) => {
                // Its suspended state stays kept for a start that builds.
                self.pending
                    .retain(|pending| !matches!(pending, Pending::Resume(_)));
                "The machine could not be built"
            }
            Some(Pending::InsertDisc { disc, .. }) => {
                work.extend(disc.medium.map(FileWork::Forget));
                "The disc could not be put in"
            }
            Some(Pending::InsertTape(tape)) => {
                work.extend(tape.medium.map(FileWork::Forget));
                "The tape could not be put in"
            }
            Some(Pending::RestoreSnapshot(_)) => "The snapshot could not be restored",
            Some(Pending::Resume(_)) => "Its saved state could not be restored",
            Some(Pending::TakeSnapshot) => "The snapshot could not be taken",
            _ => "The machine refused",
        };
        self.message = Some((String::from(what), StatusTone::Danger));
        self.owe_strip();
        if resuming {
            Event::NotResumed { what, detail }
        } else {
            Event::Refused { what, detail, work }
        }
    }

    fn ran(&mut self, ran: &Ran<'_>) {
        self.ran = true;
        if let Some(share) = &self.share {
            for row in 0..CANVAS_HEIGHT {
                if !ran.row_changed(row) || !share.copy_out(row * CANVAS_WIDTH, &mut self.slots) {
                    continue;
                }
                let dots = u32::try_from(row)
                    .ok()
                    .and_then(|row| self.picture.screen_mut().row_mut(row));
                if let Some(dots) = dots {
                    expand(&PALETTE, &self.slots, dots);
                }
            }
        }
        if !ran.sound.is_empty() {
            let played = self
                .sound
                .as_mut()
                .map(|sound| sound.play(ran.sound, self.volume, &mut self.scratch));
            if let Some(Err(err)) = played {
                if let Some(sound) = self.sound.take() {
                    sound.close();
                }
                self.say(
                    format!("The sound stream failed ({err}); the machine is silent."),
                    StatusTone::Warning,
                );
            }
        }
        let filed = self.rack.filed();
        let settled = PhysicalDrive::ALL.iter().any(|drive| {
            let index = drive.index();
            self.drives[index] && !ran.drives[index] && filed[index]
        });
        if ran.lights != self.lights || ran.drives != self.drives {
            self.lights = ran.lights;
            self.drives = ran.drives;
            self.owe_strip();
        }
        self.loaded = ran.loaded;
        if self
            .running_from
            .is_some_and(|from| self.timeline >= from + CLOCK)
        {
            self.running_from = None;
            record(events::MACHINE_RUNNING, events::MACHINE_RUNNING_MESSAGE);
        }
        if settled {
            self.ask(Pending::CollectDiscs);
        }
    }

    /// The media a request took out or collected, through the rack: the
    /// files it says to write and let go, the saves it offers.
    fn media(&mut self, media: &Media<'_>, asked: Option<Pending>) -> Event {
        for written in &media.discs {
            if let Some((side, track)) = written.lost {
                self.say(
                    format!(
                        "Drive {}'s disc holds side {side} track {track} in a form its image cannot record; it is saved without it.",
                        written.drive.index()
                    ),
                    StatusTone::Warning,
                );
            }
        }
        let resumed = matches!(asked, Some(Pending::Resume(_)));
        let handover = match asked {
            Some(Pending::EjectDisc(drive)) => Handover::EjectDisc(drive),
            Some(Pending::InsertDisc { drive, disc, .. }) => Handover::InsertDisc(drive, disc),
            Some(Pending::EjectTape) => Handover::EjectTape,
            Some(Pending::InsertTape(tape)) => Handover::InsertTape(tape),
            Some(Pending::RestoreSnapshot(_) | Pending::Resume(_)) => Handover::Restore,
            _ => Handover::Collect,
        };
        if handover == Handover::Restore {
            self.owe_whole();
        }
        let mut work = Vec::new();
        self.rack.handed(media, handover, &mut work);
        if resumed {
            Event::Resumed(work)
        } else {
            Event::Files(work)
        }
    }

    /// The machine's changes for a host key edge, each at a cycle the
    /// keyboard scan sees it.
    pub fn key(&mut self, input: &tairix_abi::input::KeyInput) {
        let changes = self.keys.edge(input);
        self.press(&changes);
    }

    /// Every key let go: the window lost the focus.
    pub fn release_keys(&mut self) {
        let changes = self.keys.release_all();
        self.press(&changes);
    }

    /// Map the keyboard by `mapping` from now on.
    pub fn set_mapping(&mut self, mapping: Mapping) {
        let changes = self.keys.set_mapping(mapping);
        self.press(&changes);
    }

    fn press(&mut self, changes: &[Change]) {
        let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
        let mut at = self.timeline.max(last);
        for &change in changes {
            if let Change::Key(key, down) = change {
                let held = &mut self.pressed_at[key_number(key) % keys::KEY_NUMBERS];
                if down {
                    *held = at;
                } else {
                    at = at.max(*held + LEAST_HOLD);
                }
            }
            self.inputs.push_back((at, change));
        }
    }

    /// BREAK pressed and let go.
    pub fn press_break(&mut self) {
        self.press(&[Change::Break(true), Change::Break(false)]);
    }

    /// `text` typed key by key, SHIFT as each character needs.
    pub fn type_text(&mut self, text: &str) {
        let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
        let mut at = self.timeline.max(last);
        for ch in text.chars().take(MOST_PASTE) {
            let Some((key, shift, shifted)) = self.keys.typing(ch, self.lights.caps_lock) else {
                continue;
            };
            let up = at + LEAST_HOLD;
            if shifted {
                self.inputs.push_back((at, Change::Key(shift, true)));
            }
            self.inputs.push_back((at, Change::Key(key, true)));
            self.inputs.push_back((up, Change::Key(key, false)));
            if shifted {
                self.inputs.push_back((up, Change::Key(shift, false)));
            }
            at += TYPE_GAP;
        }
    }

    /// Pause, or run on, every key let go as it pauses.
    pub fn toggle_pause(&mut self) {
        let paused = !self.pacer.paused();
        self.pacer.set_paused(paused);
        if paused {
            self.release_keys();
        }
        self.owe_strip();
    }

    /// Run at `speed`.
    pub fn set_speed(&mut self, speed: tairix_openmicro::front::pacing::Speed) {
        self.pacer.set_speed(speed);
        self.owe_strip();
    }

    /// Show its picture through `tube` and as `scaling` enlarges it.
    pub fn set_look(&mut self, tube: Tube, scaling: Scaling) {
        if self.picture.tube() != tube {
            self.picture.set_tube(tube);
            self.owe_whole();
        }
        if self.picture.screen().scaling() != scaling {
            self.picture.screen_mut().set_scaling(scaling);
            self.owe_whole();
        }
    }

    /// Its window retitled `name`.
    pub fn rename(&mut self, client: &mut Client, name: &str) {
        self.name = String::from(name);
        if let Err(err) = client.set_title(self.pane.id(), name) {
            app::report(
                APP_NAME,
                format_args!("the window could not be retitled ({err})"),
            );
        }
    }

    /// Fill the screen, or leave it.
    pub fn toggle_full_screen(&mut self, client: &mut Client) {
        let state = if self.full_screen {
            tairix_abi::window_ipc::WindowSizeState::Restored
        } else {
            tairix_abi::window_ipc::WindowSizeState::Fullscreen
        };
        if let Err(err) = client.set_size_state(self.pane.id(), state) {
            self.say(
                format!("The window could not fill the screen ({err})."),
                StatusTone::Warning,
            );
        }
    }

    /// What the menu shows of the machine.
    #[must_use]
    pub fn shown(&self) -> Shown {
        Shown {
            model: self.model,
            discs: self.loaded.discs,
            images: PhysicalDrive::ALL.map(|drive| self.rack.disc(drive).is_some()),
            tape: self.loaded.tape,
            recording: self.loaded.recording,
            paused: self.pacer.paused(),
            speed: self.pacer.speed(),
            full_screen: self.full_screen,
            scaling: self.picture.screen().scaling(),
            scan_lines: Effect::ScanLines.is_on(self.picture.tube()),
            keyboard: self.keys.mapping(),
        }
    }

    /// Whether RECORD is down.
    #[must_use]
    pub const fn recording(&self) -> bool {
        self.loaded.recording
    }

    /// The window was resized, or left or took the whole screen.
    pub fn resized(&mut self, client: &mut Client, (width, height): (u32, u32), full_screen: bool) {
        self.full_screen = full_screen;
        let mode = app::mode_for(width, height);
        if self.pane.resize_with(client, &mode, &mut self.surface) == Fitted::Kept {
            app::report(APP_NAME, app::SIZE_KEPT);
        }
        self.owe_whole();
    }

    /// The desktop let go of the window's frames.
    pub fn released(&mut self) {
        self.pane.release_frames();
        self.owe_whole();
    }

    /// Ask for the machine's snapshot to suspend it, by `by`: its discs
    /// collected first, what was waiting given up.
    pub fn suspend(&mut self, by: u64) {
        self.pending.clear();
        self.inputs.clear();
        self.ask(Pending::CollectDiscs);
        self.ask(Pending::SuspendSnapshot);
        self.life = Life::Suspending(by);
        self.say("Saving\u{2026}", StatusTone::Muted);
    }

    /// Take every medium out to shut it down, by `by`, what was waiting
    /// given up.
    pub fn shut_down(&mut self, by: u64) {
        self.pending.clear();
        self.inputs.clear();
        if self.model.has_drives() {
            for drive in PhysicalDrive::ALL {
                self.ask(Pending::EjectDisc(drive));
            }
        }
        self.ask(Pending::EjectTape);
        self.life = Life::ShuttingDown(Some(by));
        self.say("Shutting down\u{2026}", StatusTone::Muted);
    }

    /// Whether nothing is asked of the worker or waiting to be.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.machine.is_idle() && self.pending.is_empty()
    }

    /// The worker and the sound let go, the window kept.
    pub fn retire(&mut self, set: u64, sound_token: u64) {
        self.session = None;
        self.share = None;
        if let Some(sound) = self.sound.take() {
            unwatch_sound(set, &sound, sound_token);
            sound.close();
        }
    }

    /// Draw what changed into the surface, then present only that.
    pub fn paint(
        &mut self,
        client: &mut Client,
        (desktop, theme): (&Desktop, &Theme),
        (now, pool): (u64, &LazyPool),
    ) {
        let looks = (desktop.scale(), theme);
        let Some((target, strip)) = self.layout(looks) else {
            return;
        };
        let mode = self.mode();
        let drawing = Drawing {
            percent: desktop.scale().percent(),
            runner: pool.runner(),
            refresh: desktop.refresh(),
            now_ns: now,
        };
        let damage = &mut self.damage;
        damage.clear();
        match core::mem::take(&mut self.owing) {
            // Only a whole repaint clears, as only then is the picture drawn
            // whole.
            Owing::Whole => {
                self.surface.fill(Color::rgb(0, 0, 0));
                damage.add(Rect::new(0, 0, mode.width_px, mode.height_px));
            }
            Owing::Strip => damage.add(strip),
            Owing::Nothing => {}
        }
        match self.picture.draw(&mut self.surface, target, drawing) {
            Ok(drawn) => damage.add_region(&drawn),
            Err(why) => app::report(
                APP_NAME,
                format_args!("the picture could not be drawn ({why:?})"),
            ),
        }
        if damage.intersects(strip) {
            let message = message_of(self.message.as_ref(), &self.pacer);
            let (lights, drives) = (self.lights, self.drives);
            match self.model {
                Model::ModelB => {
                    let lamps = [
                        ("CAPS LOCK", lights.caps_lock),
                        ("SHIFT LOCK", lights.shift_lock),
                        ("MOTOR", lights.motor),
                        ("DRIVE 0", drives[0]),
                        ("DRIVE 1", drives[1]),
                    ];
                    paint_strip(&mut self.surface, strip, looks, (lamps, &message));
                }
                Model::Electron => {
                    let lamps = [("CAPS LOCK", lights.caps_lock), ("MOTOR", lights.motor)];
                    paint_strip(&mut self.surface, strip, looks, (lamps, &message));
                }
            }
        }
        self.owed.owe(&mode, Repaint::Reported, &self.damage);
        let Some(parts) = self.owed.take(&mode) else {
            return;
        };
        if let Err(err) = self.pane.present_list(client, &self.surface, &parts) {
            app::report(
                APP_NAME,
                format_args!("the window could not be presented ({err})"),
            );
        }
    }

    /// Present what the window drew while both its frames were the desktop's.
    pub fn present_deferred(&mut self, client: &mut Client) {
        let _ = self.pane.present_deferred(client, &self.surface);
    }

    /// Offer `saving` to be saved, once no picker is open.
    pub fn offer(&mut self, saving: Saving) {
        self.rack.offer(saving);
    }
}

/// `pending` as the request it is, borrowing what it carries, a collect
/// reaching the drives `collect` marks; `None` for one that asks nothing: a
/// build whose images are not read, a collect marking no drive.
fn request_of<'a>(
    pending: &'a Pending,
    images: Option<&'a Images>,
    collect: [bool; 2],
) -> Option<Request<'a>> {
    Some(match pending {
        Pending::Configure => Request::Configure(Box::new(images?.configuration())),
        Pending::InsertDisc {
            drive,
            write_protected,
            disc,
        } => Request::InsertDisc {
            drive: *drive,
            format: disc.format,
            write_protected: *write_protected,
            image: &disc.image,
        },
        Pending::EjectDisc(drive) => Request::EjectDisc(*drive),
        Pending::InsertTape(tape) => Request::InsertTape(&tape.image),
        Pending::EjectTape => Request::EjectTape,
        Pending::RewindTape => Request::RewindTape,
        Pending::RecordTape(on) => Request::RecordTape(*on),
        Pending::PowerOn => Request::PowerOn,
        Pending::TakeSnapshot | Pending::SuspendSnapshot => Request::TakeSnapshot,
        Pending::RestoreSnapshot(snapshot) | Pending::Resume(snapshot) => {
            Request::RestoreSnapshot(snapshot)
        }
        Pending::CollectDiscs if collect.contains(&true) => Request::CollectDiscs(collect),
        Pending::CollectDiscs => return None,
    })
}

/// What the user is told of `refusal`.
const fn detail_of(refusal: Refused) -> &'static str {
    match refusal {
        Refused::Unconfigured => "There is no machine yet.",
        Refused::Rom => "A ROM image does not fit the machine.",
        Refused::Disc => "It is not a disc image the machine reads.",
        Refused::Tape => "It is not a tape image the machine reads.",
        Refused::Snapshot => "It is not a snapshot of this machine.",
        Refused::TooLarge => "It is too large.",
        Refused::Memory => "The machine ran out of memory.",
        Refused::Malformed => "The request was malformed.",
        Refused::Model => "The machine has nothing to take it.",
    }
}

/// What a save holds for the picker to offer.
#[must_use]
pub fn save_formats(holding: Holding) -> tairix_abi::window_ipc::SaveFormats {
    let mut formats = tairix_abi::window_ipc::SaveFormats::ANY;
    let own = match holding {
        Holding::Disc => tairix_browse::MediaType::AcornDfsDisc,
        Holding::Tape => tairix_browse::MediaType::TapeUef,
        Holding::Snapshot => return formats,
    };
    let _ = formats.push(tairix_browse::media::save_format(own), true);
    formats
}

/// The strip's message: paused, or the latest said.
fn message_of(message: Option<&(String, StatusTone)>, pacer: &Pacer) -> (String, StatusTone) {
    match (pacer.paused(), message) {
        (true, _) => (String::from("Paused"), StatusTone::Muted),
        (false, Some((text, tone))) => (text.clone(), *tone),
        (false, None) => (String::new(), StatusTone::Ink),
    }
}

/// The status strip: each of the model's lamps, lit or not, and the message.
fn paint_strip<const N: usize>(
    surface: &mut Surface,
    strip: Rect,
    (scale, theme): (Scale, &Theme),
    (lamps, message): ([(&'static str, bool); N], &(String, StatusTone)),
) {
    let band = StatusBand::new(theme, scale);
    let lamp = band.face().line_height() * 2 / 3;
    let widths = lamps.map(|(label, _)| band.slot_width(label) + lamp);
    let slots = band.slots::<N, 0>(strip, widths, []);
    let mut parts: Vec<(Rect, &str, StatusTone)> = slots
        .leading
        .iter()
        .zip(lamps.iter())
        .map(|(slot, (label, _))| {
            let text = Rect::new(
                slot.left() + i32::try_from(lamp).unwrap_or(0),
                slot.top(),
                slot.width.saturating_sub(lamp),
                slot.height,
            );
            (text, *label, StatusTone::Muted)
        })
        .collect();
    parts.push((slots.message, message.0.as_str(), message.1));
    band.paint(surface, strip, theme, &parts);
    let palette = theme.palette();
    let side = lamp.max(2);
    for (slot, (_, lit)) in slots.leading.iter().zip(lamps.iter()) {
        let colour = if *lit {
            palette.danger
        } else {
            palette.on_surface_muted
        };
        let (Ok(x), Ok(y)) = (
            u32::try_from(slot.left()),
            u32::try_from(
                slot.top() + i32::try_from(slot.height.saturating_sub(side) / 2).unwrap_or(0),
            ),
        ) else {
            continue;
        };
        surface.fill_round_rect(x, y, side, side, side / 2, Color::from(colour));
    }
}

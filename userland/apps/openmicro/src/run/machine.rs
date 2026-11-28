//! One machine's window: its worker's session and the requests waiting for
//! it, its keyboard, its media, its sound, its picture and its status strip.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayMode};
use tairix_abi::window_ipc::{KeyboardMode, PointerAction, ViewFilter, WindowView};
use tairix_abi::Errno;
use tairix_controls::{Button, ButtonAction, StatusBand, StatusTone};
use tairix_crt::screen::{fit, Drawing, Picture, Scaling, DOT};
use tairix_crt::Tube;
use tairix_geometry::{Rect, Region, Scale};
use tairix_machines::menu::{Effect, Followed};
use tairix_openmicro::front::keys::{self, Edges, Keyboard, MachineKey, Mapping, Press, Stick};
use tairix_openmicro::front::library::Joystick;
use tairix_openmicro::front::media::{Disc, FileWork, Handover, Holding, Rack, Saving, Tape};
use tairix_openmicro::front::menu::{Offered, Shown, SHIFT_BREAK};
use tairix_openmicro::front::pacing::{Next, Pacer, Quiet};
use tairix_openmicro::front::picture::{palette_of, Expansion};
use tairix_openmicro::front::view::ViewRefusal;
use tairix_openmicro::front::{events, LEAST_PICTURE, OPEN_PICTURE};
use tairix_openmicro::machine::bbc::fdc::PhysicalDrive;
use tairix_openmicro::machine::c64::keyboard::Keycap;
use tairix_openmicro::machine::canvas::{CANVAS_HEIGHT, CANVAS_WIDTH};
use tairix_openmicro::machine::{
    bbc, electron, LetterCase, Model, BREAK_HELD_MICROS, BREAK_KEYS_HELD_CENTISECONDS,
};
use tairix_openmicro::worker::protocol::{
    session_bounds, Change, DeckKey, DiscFormat, Input, Lights, Loaded, Media, ModelKey, Ran,
    Refusal as Refused, Reply, Request, MAX_INPUTS, SHARE_BYTES,
};
use tairix_openmicro::worker::{Link, WORKER_THREADS};
use tairix_raster::resample::Region as Target;
use tairix_raster::{Color, Pixel, Surface};
use tairix_rt::LogSink;
use tairix_sandbox::rt::{KernelWaitSet, RtSessionLauncher, SessionMembers};
use tairix_sandbox::session::Share;
use tairix_sandbox::{SupervisedSession, WorkerMemory};
use tairix_theme::Theme;
use tairix_window::app::{self, Fitted, LazyPool, WindowPane};
use tairix_window::{
    pointer_input_events, pointer_point, Desktop, Owed, PresentState, Repaint, WindowSizing,
};

use crate::files::Speaker;
use crate::program::{record, unwatch_sound, Client, APP_NAME};
use tairix_openmicro::front::images::Images;

/// The least a key is held, in the OS's keyboard scans, halves counted: two
/// and a half, so two see it.
const LEAST_HOLD_HALF_SCANS: u64 = 5;
/// The most pasted text taken at once.
const MOST_PASTE: usize = 4_096;

/// The link a machine's worker is driven over, the worker replaced when it
/// fails.
type WorkerLink = Link<RtSessionLauncher, LogSink, KernelWaitSet>;

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
    // Boxed: its keycaps make its table of what each host key holds the
    // largest by far.
    C64(Box<Keyboard<Keycap>>),
}

/// `$body` with `$keyboard` bound to the model's keyboard and `$key` to
/// the protocol's naming of its keys.
macro_rules! each_keyboard {
    ($keys:expr, $keyboard:ident, $key:ident => $body:expr) => {
        match $keys {
            Keys::ModelB($keyboard) => {
                let $key = ModelKey::ModelB;
                $body
            }
            Keys::Electron($keyboard) => {
                let $key = ModelKey::Electron;
                $body
            }
            Keys::C64($keyboard) => {
                let $key = ModelKey::C64;
                $body
            }
        }
    };
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
    fn new(model: Model, mapping: Mapping) -> Self {
        match model {
            Model::ModelB => Self::ModelB(Keyboard::new(mapping)),
            Model::Electron => Self::Electron(Keyboard::new(mapping)),
            Model::C64 => Self::C64(Box::new(Keyboard::new(mapping))),
        }
    }

    fn edge(&mut self, input: &tairix_abi::input::KeyInput) -> Vec<Change> {
        each_keyboard!(self, keyboard, key => {
            let mut out = Vec::new();
            keyboard.edge(input, &mut out);
            changes(&out, key)
        })
    }

    fn release_all(&mut self) -> Vec<Change> {
        each_keyboard!(self, keyboard, key => {
            let mut out = Vec::new();
            keyboard.release_all(&mut out);
            changes(&out, key)
        })
    }

    fn set_mapping(&mut self, mapping: Mapping) -> Vec<Change> {
        each_keyboard!(self, keyboard, key => {
            let mut out = Vec::new();
            keyboard.set_mapping(mapping, &mut out);
            changes(&out, key)
        })
    }

    fn mapping(&self) -> Mapping {
        each_keyboard!(self, keyboard, _key => keyboard.mapping())
    }

    /// The key that types `ch` and SHIFT where it needs it.
    fn typing(&self, ch: char, case: LetterCase) -> Option<(ModelKey, ModelKey, bool)> {
        each_keyboard!(self, keyboard, key => typed_on(keyboard, (ch, case), key))
    }
}

/// The key `keyboard`'s machine types `ch` with, its letter keys typing as
/// given, its SHIFT, and whether SHIFT goes with it.
fn typed_on<K: MachineKey>(
    _keyboard: &Keyboard<K>,
    (ch, case): (char, LetterCase),
    key: fn(K) -> ModelKey,
) -> Option<(ModelKey, ModelKey, bool)> {
    keys::typing::<K>(ch, case).map(|(typed, shifted)| (key(typed), key(K::SHIFT), shifted))
}

/// The number a key is counted under for its least hold.
const fn key_number(key: ModelKey) -> usize {
    match key {
        ModelKey::ModelB(key) => (key.number() & 0x7F) as usize,
        ModelKey::Electron(key) => key.number() as usize,
        ModelKey::C64(keycap) => keycap.number() as usize,
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
    /// A datasette key pressed.
    Deck(DeckKey),
    /// A cartridge plugged in from its image.
    InsertCartridge(Vec<u8>),
    /// The cartridge unplugged.
    RemoveCartridge,
    /// A program loaded into memory from its file.
    LoadProgram(Vec<u8>),
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
    /// A drive's disc collected, filed or not, then offered to be saved as
    /// the machine has it.
    SaveDisc(PhysicalDrive),
}

impl Pending {
    /// Whether it puts a medium in.
    const fn puts_in(&self) -> bool {
        matches!(
            self,
            Self::InsertDisc { .. } | Self::InsertTape(_) | Self::InsertCartridge(_)
        )
    }

    /// The file of the medium it would have put in, let go with it.
    fn forgotten(self) -> Option<FileWork> {
        match self {
            Self::InsertDisc { disc, .. } => disc.medium.map(FileWork::Forget),
            Self::InsertTape(tape) => tape.medium.map(FileWork::Forget),
            _ => None,
        }
    }
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
    /// Its wait-set and the tokens its worker's two pipes are reported under.
    wait: (u64, u64, u64),
    link: Option<WorkerLink>,
    share: Option<Arc<dyn Share>>,
    asked: Option<Pending>,
    pending: VecDeque<Pending>,
    inputs: VecDeque<(u64, Change)>,
    timeline: u64,
    /// When each key and stick line last moved, so every press and release
    /// lasts long enough for the scan to see.
    edges: Edges,
    pacer: Pacer,
    keys: Keys,
    /// The keypad as a C64's joystick, and the port it plays in.
    stick: Stick,
    joystick: Joystick,
    /// Each slot's colour, as the model's canvas holds them.
    palette: Box<[[u8; DOT]; 256]>,
    /// The drives the machine has.
    fitted: [bool; 2],
    /// The cartridge image last plugged in, put back in a replacement
    /// worker.
    cartridge: Option<Vec<u8>>,
    images: Option<Box<Images>>,
    /// Its sound stream.
    pub sound: Option<Speaker>,
    slots: Vec<u8>,
    expansion: Expansion,
    /// The canvas rows changed since the window's view was last presented.
    view_rows: Option<Range<u32>>,
    /// Where the desktop refused a view, the window drawing its picture
    /// itself there.
    view_refusal: ViewRefusal,
    /// Its media and the picker.
    pub rack: Rack,
    /// The file a picker picked, held while its name is asked.
    pub picked: Option<(u64, bool)>,
    lights: Lights,
    /// How its letter keys typed at the last slice's end.
    case: LetterCase,
    drives: [bool; 2],
    /// Whether each drive's disc turned as the last slice ended.
    turning: [bool; 2],
    /// When its sound last moved.
    quiet: Quiet,
    loaded: Loaded,
    /// The menu up over its window.
    pub menu: Option<Followed<()>>,
    message: Option<(String, StatusTone)>,
    full_screen: bool,
    /// Its sound's level.
    pub volume: u16,
    /// Whether a tape or a disc loads as fast as the machine can run.
    pub fast_loading: bool,
    /// What its folder offers.
    pub offered: Offered,
    /// Whether it was ever run, so a stop keeps its last screen.
    pub ran: bool,
    running_from: Option<u64>,
    /// The strip's SHIFT-BREAK button, on a model it boots.
    boot: Option<Button>,
}

/// The status strip's height on a `model`'s window: a band's, or one holding
/// the SHIFT-BREAK button on a model it boots.
fn strip_height(model: Model, (scale, theme): (Scale, &Theme)) -> u32 {
    let band = StatusBand::new(theme, scale);
    if model.boots_with_shift_break() {
        band.height_holding_button(scale, theme)
    } else {
        band.height()
    }
}

/// The window's opening size and the least it takes, a strip beneath the
/// picture.
fn window_sizing(
    desktop: &Desktop,
    theme: &Theme,
    model: Model,
) -> (tairix_abi::driver::display::DisplayMode, WindowSizing) {
    let strip = strip_height(model, (desktop.scale(), theme));
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
        let model = machine.model();
        let (mode, sizing) = window_sizing(desktop, theme, model);
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
            wait: (set, tokens.0, tokens.1),
            link: None,
            share: None,
            asked: None,
            pending: VecDeque::new(),
            inputs: VecDeque::new(),
            timeline: 0,
            edges: Edges::new(),
            pacer: Pacer::new(machine.fitting.timing()),
            keys: Keys::new(model, machine.keyboard),
            stick: Stick::new(),
            joystick: machine.joystick,
            palette: palette_of(model),
            fitted: machine.fitting.drives(),
            cartridge: None,
            images: None,
            sound: None,
            slots: alloc::vec![0; CANVAS_WIDTH],
            expansion: Expansion::default(),
            view_rows: None,
            view_refusal: ViewRefusal::default(),
            rack: Rack::new(model),
            picked: None,
            lights: Lights::default(),
            case: LetterCase::default(),
            drives: [false; 2],
            turning: [false; 2],
            quiet: Quiet::default(),
            loaded: Loaded::default(),
            menu: None,
            message: Some((String::from("Starting\u{2026}"), StatusTone::Muted)),
            full_screen: false,
            volume: machine.volume,
            fast_loading: machine.fast_loading,
            offered: Offered::default(),
            ran: false,
            running_from: None,
            boot: model
                .boots_with_shift_break()
                .then(|| Button::labelled(SHIFT_BREAK)),
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
        let launcher = RtSessionLauncher::own_binary(WorkerMemory::Documents)
            .sharing(share)
            .threads(WORKER_THREADS);
        let (set, read, write) = self.wait;
        let mut link = Link::new(
            SupervisedSession::new(launcher, bounds, LogSink),
            SessionMembers::new(set, read, write),
        );
        let _ = link.start(now);
        self.share = link.share().cloned();
        self.link = Some(link);
        self.images = Some(images);
        self.life = Life::Running;
        self.message = None;
        let resume = resume.map(|(screen, kept)| {
            self.show_kept(&screen);
            Pending::Resume(kept)
        });
        self.ask_first(core::iter::once(Pending::Configure).chain(resume));
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
    fn layout(&self, looks: (Scale, &Theme)) -> Option<(Target, Rect)> {
        self.layout_of(&self.mode(), looks)
    }

    /// The picture and status strip of the window shaped as `mode`.
    fn layout_of(&self, mode: &DisplayMode, looks: (Scale, &Theme)) -> Option<(Target, Rect)> {
        let strip = strip_height(self.model, looks).min(mode.height_px);
        let room = mode.height_px - strip;
        let picture = fit(mode.width_px, room)?;
        Some((
            picture,
            Rect::new(0, i32::try_from(room).ok()?, mode.width_px, strip),
        ))
    }

    /// The view the frames of the window shaped as `mode` carry for a
    /// picture at `target`: the canvas itself, for the desktop or its display
    /// engine to enlarge, as long as the tube draws nothing at the window's
    /// own size the enlarging would lose and no view was refused at its size.
    fn wanted_view(&self, mode: &DisplayMode, target: Target, percent: u32) -> Option<WindowView> {
        let allowed = self.view_refusal.allows((mode.width_px, mode.height_px));
        if !allowed || self.picture.tube().passes(percent).1 > 0 {
            return None;
        }
        let (width_px, height_px) = self.picture.screen().size();
        let filter = match self.picture.screen().scaling() {
            Scaling::Sharp => ViewFilter::Sharp,
            Scaling::Smooth => ViewFilter::Smooth,
        };
        Some(WindowView {
            width_px,
            height_px,
            placement: DamageRect {
                x: target.x,
                y: target.y,
                width_px: target.width,
                height_px: target.height,
            },
            filter,
        })
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
        self.view_rows = Some(0..self.picture.screen().size().1);
    }

    /// A request for the machine, behind those waiting.
    pub fn ask(&mut self, pending: Pending) {
        self.pending.push_back(pending);
    }

    /// `requests`, in their order, ahead of every request waiting: a build
    /// goes in before what was put in while it was due.
    fn ask_first(&mut self, requests: impl IntoIterator<Item = Pending>) {
        let waiting = core::mem::take(&mut self.pending);
        self.pending.extend(requests);
        self.pending.extend(waiting);
    }

    /// What waits given up: the files of what it would have put in, to be let
    /// go.
    fn give_up_waiting(&mut self) -> Vec<FileWork> {
        self.pending
            .drain(..)
            .filter_map(Pending::forgotten)
            .collect()
    }

    /// Whether the machine's worker is live.
    #[must_use]
    pub fn live(&self) -> bool {
        self.link.as_ref().is_some_and(Link::is_live)
    }

    /// Whether it is being suspended or shut down, and takes nothing more.
    #[must_use]
    pub const fn put_away(&self) -> bool {
        matches!(self.life, Life::Suspending(_) | Life::ShuttingDown(_))
    }

    /// Ask for the next request waiting, or the slice the pace asks for.
    pub fn pump(&mut self, now: u64) {
        let hurried = self.hurried();
        let Some(link) = self
            .link
            .as_mut()
            .filter(|link| link.is_live() && link.is_idle())
        else {
            return;
        };
        let collect = self.rack.filed();
        while let Some(pending) = self.pending.pop_front() {
            if let Some(request) = request_of(&pending, self.images.as_deref(), collect) {
                if link.ask(&request).is_ok() {
                    self.asked = Some(pending);
                }
                return;
            }
        }
        if self.images.is_none() || self.life != Life::Running {
            return;
        }
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
            if link.ask(&request).is_ok() {
                self.timeline = end;
            }
        }
    }

    /// Send what waits, and watch the worker's pipes as its session wants
    /// them.
    ///
    /// # Errors
    ///
    /// The wait-set's refusal to watch a pipe: the machine cannot be heard.
    pub fn sync(&mut self, now: u64) -> Result<(), Errno> {
        let Some(link) = self.link.as_mut() else {
            return Ok(());
        };
        if let Some(why) = link.sync(now)? {
            self.lost(why);
        }
        Ok(())
    }

    /// Serve the worker's pipes as `ready` says they are: the replies it sent,
    /// in order, and why it was lost, if it was.
    pub fn serve(&mut self, now: u64, ready: (bool, bool)) -> (Vec<Vec<u8>>, Option<&'static str>) {
        let mut frames = Vec::new();
        let lost = self
            .link
            .as_mut()
            .and_then(|link| link.serve(now, ready, &mut frames));
        (frames, lost)
    }

    /// The worker was lost `why`, nothing left asked of it: a replacement
    /// starts from power-on, and what was being put in goes in after it is
    /// built.
    pub fn lost(&mut self, why: &str) {
        if self.share.take().is_some() {
            record(events::WORKER_REPLACED, events::WORKER_REPLACED_MESSAGE);
            self.say(
                format!("The machine stopped ({why}); it is starting again."),
                StatusTone::Warning,
            );
        }
        self.pending.retain(Pending::puts_in);
        if let Some(asked) = self.asked.take().filter(Pending::puts_in) {
            self.pending.push_front(asked);
        }
        self.inputs.clear();
        self.edges = Edges::new();
        self.loaded = Loaded::default();
    }

    /// A replacement worker, once its deadline comes: the machine built
    /// again, its media put back.
    pub fn restart_due(&mut self, now: u64) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        if self.life != Life::Running || link.restart_deadline().is_none_or(|at| at > now) {
            return;
        }
        if link.start(now).is_none() {
            return;
        }
        self.share = link.share().cloned();
        self.timeline = 0;
        self.quiet = Quiet::default();
        let discs = PhysicalDrive::ALL.into_iter().filter_map(|drive| {
            let disc = self.rack.disc(drive)?.clone();
            Some(Pending::InsertDisc {
                drive,
                write_protected: disc.medium.is_none(),
                disc,
            })
        });
        let tape = self.rack.tape().cloned().map(Pending::InsertTape);
        let cartridge = self.cartridge.clone().map(Pending::InsertCartridge);
        let build: Vec<Pending> = core::iter::once(Pending::Configure)
            .chain(discs)
            .chain(tape)
            .chain(cartridge)
            .collect();
        self.ask_first(build);
    }

    /// When the window next wants the loop: a replacement worker, the tube's
    /// next step, or the pace's next slice.
    pub fn due(&mut self, now: u64) -> Option<u64> {
        let restart = self.link.as_ref().and_then(Link::restart_deadline);
        let tube = self.picture.due();
        let paced = (self.link.as_ref().is_some_and(Link::is_idle)
            && self.pending.is_empty()
            && self.images.is_some()
            && self.life == Life::Running)
            .then(|| {
                let hurried = self.hurried();
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
                match asked {
                    Some(Pending::Configure | Pending::PowerOn) => {
                        self.running_from = Some(self.timeline);
                        self.owe_whole();
                    }
                    // Kept only once plugged in, as the worker then holds it.
                    Some(Pending::InsertCartridge(image)) => self.cartridge = Some(image),
                    Some(Pending::RemoveCartridge) => self.cartridge = None,
                    _ => {}
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
            // The worker refused before unplugging the one it holds.
            Some(Pending::InsertCartridge(_)) => "The cartridge could not be plugged in",
            Some(Pending::LoadProgram(_)) => "The program could not be loaded",
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
            self.expansion.restart();
            for row in 0..CANVAS_HEIGHT {
                if !ran.row_changed(row) || !share.copy_out(row * CANVAS_WIDTH, &mut self.slots) {
                    continue;
                }
                if let Ok(row) = u32::try_from(row) {
                    let screen = self.picture.screen_mut();
                    self.expansion.row(&self.palette, screen, row, &self.slots);
                    let rows = self.view_rows.get_or_insert(row..row + 1);
                    *rows = rows.start.min(row)..rows.end.max(row + 1);
                }
            }
        }
        if ran.sound.is_empty() {
            if let Some(sound) = self.sound.as_mut() {
                sound.forget();
            }
        } else {
            let played = self
                .sound
                .as_mut()
                .map(|sound| sound.play(ran.sound, self.volume));
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
        self.turning = ran.turning;
        self.quiet.slice(self.timeline, ran.audible);
        self.loaded = ran.loaded;
        self.case = ran.case;
        if self
            .running_from
            .is_some_and(|from| self.timeline >= from + self.pacer.timing().clock())
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
        let saving = match asked {
            Some(Pending::SaveDisc(drive)) => Some(drive),
            _ => None,
        };
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
            // A restored machine holds its snapshot's cartridge, if any, and
            // not the one last plugged in, which a replacement must not get.
            self.cartridge = None;
        }
        let mut work = Vec::new();
        self.rack.handed(media, handover, &mut work);
        if let Some(saving) = saving.and_then(|drive| self.rack.disc_saving(drive)) {
            self.rack.offer(saving);
        }
        if resumed {
            Event::Resumed(work)
        } else {
            Event::Files(work)
        }
    }

    /// The machine's changes for a host key edge, each at a cycle the
    /// keyboard scan sees it: on a C64 playing the keypad as a joystick, a
    /// keypad key moves the stick.
    pub fn key(&mut self, input: &tairix_abi::input::KeyInput) {
        if let Some(port) = self.stick_port() {
            if let Some(lines) = self.stick.edge(input) {
                self.press(&[Change::Joystick(port, lines)]);
                return;
            }
        }
        let changes = self.keys.edge(input);
        self.press(&changes);
    }

    /// Every key let go: the window lost the focus.
    pub fn release_keys(&mut self) {
        let mut changes = self.keys.release_all();
        if let Some(port) = self.stick_port() {
            changes.extend(
                self.stick
                    .release_all()
                    .map(|lines| Change::Joystick(port, lines)),
            );
        }
        self.press(&changes);
    }

    /// The port a C64's keypad plays its joystick in, if it does.
    fn stick_port(&self) -> Option<u8> {
        (self.model == Model::C64)
            .then(|| self.joystick.port())
            .flatten()
    }

    /// Play the keypad as a joystick as `joystick` says from now on, the
    /// stick let go first.
    pub fn set_joystick(&mut self, joystick: Joystick) {
        if joystick != self.joystick {
            if let Some(port) = self.stick_port() {
                if let Some(lines) = self.stick.release_all() {
                    self.press(&[Change::Joystick(port, lines)]);
                }
            }
            self.joystick = joystick;
        }
    }

    /// The least a key is held, in the machine's cycles.
    fn least_hold(&self) -> u64 {
        self.pacer.timing().clock() * LEAST_HOLD_HALF_SCANS
            / (2 * self.model.key_scans_per_second())
    }

    /// Map the keyboard by `mapping` from now on.
    pub fn set_mapping(&mut self, mapping: Mapping) {
        let changes = self.keys.set_mapping(mapping);
        self.press(&changes);
    }

    fn press(&mut self, changes: &[Change]) {
        let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
        let mut at = self.timeline.max(last);
        let hold = self.least_hold();
        for &change in changes {
            at = match change {
                Change::Key(key, down) => self.edges.key(key_number(key), down, at, hold),
                Change::Joystick(port, lines) => {
                    self.edges.stick(usize::from(port), lines, at, hold)
                }
                _ => at,
            };
            self.inputs.push_back((at, change));
        }
    }

    /// SHIFT held as BREAK is pressed and let go, and on until the reset has
    /// read it: the boot of the medium in the machine, on a model it boots.
    pub fn press_shift_break(&mut self) {
        let shift = match self.model {
            Model::ModelB => ModelKey::ModelB(bbc::keyboard::Key::SHIFT),
            Model::Electron => ModelKey::Electron(electron::keyboard::Key::SHIFT),
            Model::C64 => return,
        };
        let clock = self.pacer.timing().clock();
        let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
        let hold = self.least_hold();
        let down = self
            .edges
            .key(key_number(shift), true, self.timeline.max(last), hold);
        let up = down + clock * BREAK_HELD_MICROS / 1_000_000;
        let let_go = self.edges.key(
            key_number(shift),
            false,
            up + clock * BREAK_KEYS_HELD_CENTISECONDS / 100,
            hold,
        );
        self.inputs.extend([
            (down, Change::Key(shift, true)),
            (down, Change::Break(true)),
            (up, Change::Break(false)),
            (let_go, Change::Key(shift, false)),
        ]);
    }

    /// Where the strip's SHIFT-BREAK button lies, on a model it boots.
    fn boot_at(&self, looks: (Scale, &Theme)) -> Option<Rect> {
        let (scale, theme) = looks;
        let width = self.boot.as_ref()?.measured_width(scale, theme);
        let (_, strip) = self.layout(looks)?;
        let slot = StatusBand::new(theme, scale)
            .slots::<0, 1>(strip, [], [width])
            .trailing[0];
        Some(StatusBand::button_in(slot, scale, theme))
    }

    /// Take a pointer event at `(x, y)` doing `action` over the window: the
    /// strip's SHIFT-BREAK button's, pressing SHIFT-BREAK once clicked.
    pub fn pointer(&mut self, (x, y, action): (u32, u32, PointerAction), looks: (Scale, &Theme)) {
        let Some(at) = self.boot_at(looks) else {
            return;
        };
        let Some(button) = self.boot.as_mut() else {
            return;
        };
        let mut redrawn = Region::new();
        let mut clicked = false;
        for input in pointer_input_events(action, pointer_point(x, y)) {
            clicked |= button.on_pointer(&input, at, &mut redrawn) == Some(ButtonAction::Activated);
        }
        if !redrawn.is_empty() {
            self.owe_strip();
        }
        if clicked {
            self.press_shift_break();
        }
    }

    /// BREAK pressed and let go; on a C64, RESTORE.
    pub fn press_break(&mut self) {
        if self.model == Model::C64 {
            let restore = ModelKey::C64(Keycap::Restore);
            self.press(&[Change::Key(restore, true), Change::Key(restore, false)]);
        } else {
            self.press(&[Change::Break(true), Change::Break(false)]);
        }
    }

    /// A cartridge plugged in from `image`, kept to put back in a
    /// replacement worker.
    pub fn insert_cartridge(&mut self, image: Vec<u8>) {
        self.ask(Pending::InsertCartridge(image));
    }

    /// The cartridge unplugged.
    pub fn remove_cartridge(&mut self) {
        self.ask(Pending::RemoveCartridge);
    }

    /// `text` typed key by key, SHIFT as each character needs.
    pub fn type_text(&mut self, text: &str) {
        let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
        let mut at = self.timeline.max(last);
        let hold = self.least_hold();
        for ch in text.chars().take(MOST_PASTE) {
            let Some((key, shift, shifted)) = self.keys.typing(ch, self.case) else {
                continue;
            };
            if shifted {
                at = self.edges.key(key_number(shift), true, at, hold);
                self.inputs.push_back((at, Change::Key(shift, true)));
            }
            at = self.edges.key(key_number(key), true, at, hold);
            self.inputs.push_back((at, Change::Key(key, true)));
            at = self.edges.key(key_number(key), false, at, hold);
            self.inputs.push_back((at, Change::Key(key, false)));
            if shifted {
                at = self.edges.key(key_number(shift), false, at, hold);
                self.inputs.push_back((at, Change::Key(shift, false)));
            }
            // As long up as down, so the scan sees each letter of a pair apart.
            at += hold;
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

    /// Whether the pace is hurried: fast loading is on, the cassette motor
    /// runs or a drive's disc turns, and the machine has been silent a while.
    fn hurried(&self) -> bool {
        self.fast_loading
            && (self.lights.motor || self.turning[0] || self.turning[1])
            && self.quiet.settled(self.timeline, self.pacer.timing())
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
            drives: self.fitted,
            discs: self.loaded.discs,
            images: PhysicalDrive::ALL.map(|drive| self.rack.disc(drive).is_some()),
            tape: self.loaded.tape,
            recording: self.loaded.recording,
            playing: self.loaded.playing,
            cartridge: self.loaded.cartridge,
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

    /// The drives the machine has.
    #[must_use]
    pub const fn fitted(&self) -> [bool; 2] {
        self.fitted
    }

    /// The window was resized, or left or took the whole screen.
    pub fn resized(
        &mut self,
        client: &mut Client,
        (desktop, theme): (&Desktop, &Theme),
        (width, height): (u32, u32),
        full_screen: bool,
    ) {
        self.full_screen = full_screen;
        self.view_refusal.resized((width, height));
        let mode = app::mode_for(width, height);
        let looks = (desktop.scale(), theme);
        let view = self
            .layout_of(&mode, looks)
            .and_then(|(target, _)| self.wanted_view(&mode, target, desktop.scale().percent()));
        if self.reshape(client, &mode, view) == Fitted::Kept {
            app::report(APP_NAME, app::SIZE_KEPT);
        }
        self.owe_whole();
    }

    /// The window's frames shaped as `mode`, carrying `view`, the surface
    /// it is drawn into fresh where its size changes.
    fn reshape(
        &mut self,
        client: &mut Client,
        mode: &DisplayMode,
        view: Option<WindowView>,
    ) -> Fitted {
        let resized =
            (mode.width_px, mode.height_px) != (self.surface.width(), self.surface.height());
        let fresh = if resized {
            match Surface::new(mode.width_px, mode.height_px) {
                Some(fresh) => Some(fresh),
                None => return Fitted::Kept,
            }
        } else {
            None
        };
        let fitted = self.pane.reshape(client, mode, view);
        if let (Fitted::Adopted, Some(fresh)) = (fitted, fresh) {
            self.surface = fresh;
        }
        fitted
    }

    /// The desktop refused the window's last resize. A view it carried is not
    /// asked for again at that size, so the size the desktop kept is taken
    /// again without one.
    pub fn resize_refused(&mut self) {
        if self.pane.view().is_some() {
            let mode = self.mode();
            self.view_refusal.refused((mode.width_px, mode.height_px));
        }
    }

    /// The desktop let go of the window's frames.
    pub fn released(&mut self) {
        self.pane.release_frames();
        self.owe_whole();
    }

    /// Ask for the machine's snapshot to suspend it, by `by`: its discs
    /// collected first, what was waiting given up and the files of what it
    /// would have put in answered, to be let go.
    pub fn suspend(&mut self, by: u64) -> Vec<FileWork> {
        let work = self.give_up_waiting();
        self.inputs.clear();
        self.ask(Pending::CollectDiscs);
        self.ask(Pending::SuspendSnapshot);
        self.life = Life::Suspending(by);
        self.say("Saving\u{2026}", StatusTone::Muted);
        work
    }

    /// Take every medium out to shut it down, by `by`, what was waiting
    /// given up and the files of what it would have put in answered, to be
    /// let go.
    pub fn shut_down(&mut self, by: u64) -> Vec<FileWork> {
        let work = self.give_up_waiting();
        self.inputs.clear();
        for drive in PhysicalDrive::ALL {
            if self.fitted[drive.index()] {
                self.ask(Pending::EjectDisc(drive));
            }
        }
        self.ask(Pending::EjectTape);
        self.life = Life::ShuttingDown(Some(by));
        self.say("Shutting down\u{2026}", StatusTone::Muted);
        work
    }

    /// Whether nothing is asked of the worker or waiting to be.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.link.as_ref().is_none_or(Link::is_idle) && self.pending.is_empty()
    }

    /// The worker and the sound let go, the window kept: the files of every
    /// medium it held or was to put in answered, to be let go.
    pub fn retire(&mut self, set: u64, sound_token: u64) -> Vec<FileWork> {
        self.link = None;
        self.share = None;
        if let Some(sound) = self.sound.take() {
            unwatch_sound(set, &sound, sound_token);
            sound.close();
        }
        let mut work = self.give_up_waiting();
        work.extend(self.asked.take().and_then(Pending::forgotten));
        self.rack.let_go(&mut work);
        work
    }

    /// Draw what changed into the surface, then present only that: nothing
    /// while the window is hidden, and nothing while both its frames are the
    /// desktop's, what changed meanwhile drawn once one comes back.
    pub fn paint(
        &mut self,
        client: &mut Client,
        (desktop, theme): (&Desktop, &Theme),
        (now, pool): (u64, &LazyPool),
    ) {
        let window = self.pane.id();
        if client.is_hidden(window) {
            return;
        }
        if client.present_state(window) == PresentState::InFlight {
            client.owe_frame(window);
            return;
        }
        let looks = (desktop.scale(), theme);
        let Some((target, strip)) = self.layout(looks) else {
            return;
        };
        let mode = self.mode();
        let view = self.wanted_view(&mode, target, desktop.scale().percent());
        if view.as_ref() != self.pane.view() {
            match self.reshape(client, &mode, view) {
                Fitted::Adopted => self.owe_whole(),
                // Painted once the resize in flight is answered.
                Fitted::Deferred => return,
                Fitted::Kept => {}
            }
        }
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
        let viewed = self.pane.view().is_some();
        if !viewed {
            match self.picture.draw(&mut self.surface, target, drawing) {
                Ok(drawn) => damage.add_region(&drawn),
                Err(why) => app::report(
                    APP_NAME,
                    format_args!("the picture could not be drawn ({why:?})"),
                ),
            }
        }
        if damage.intersects(strip) {
            let message = message_of(self.message.as_ref(), &self.pacer);
            let (lights, drives) = (self.lights, self.drives);
            let boot = self.boot.as_ref();
            let surface = &mut self.surface;
            match self.model {
                Model::ModelB => {
                    let lamps = [
                        ("CAPS LOCK", lights.caps_lock),
                        ("SHIFT LOCK", lights.shift_lock),
                        ("MOTOR", lights.motor),
                        ("DRIVE 0", drives[0]),
                        ("DRIVE 1", drives[1]),
                    ];
                    paint_strip(surface, strip, looks, (lamps, &message), boot);
                }
                Model::Electron => {
                    let lamps = [("CAPS LOCK", lights.caps_lock), ("MOTOR", lights.motor)];
                    paint_strip(surface, strip, looks, (lamps, &message), boot);
                }
                Model::C64 => {
                    let motor = ("MOTOR", lights.motor);
                    match self.fitted {
                        [true, true] => {
                            let lamps = [motor, ("DRIVE 8", drives[0]), ("DRIVE 9", drives[1])];
                            paint_strip(surface, strip, looks, (lamps, &message), boot);
                        }
                        [true, false] => {
                            let lamps = [motor, ("DRIVE 8", drives[0])];
                            paint_strip(surface, strip, looks, (lamps, &message), boot);
                        }
                        [false, true] => {
                            let lamps = [motor, ("DRIVE 9", drives[1])];
                            paint_strip(surface, strip, looks, (lamps, &message), boot);
                        }
                        [false, false] => {
                            paint_strip(surface, strip, looks, ([motor], &message), boot);
                        }
                    }
                }
            }
        }
        self.present(client, &mode, viewed);
    }

    /// Present what the paint drew into the window, shaped as `mode`, and
    /// where it carries a view, the canvas rows that changed.
    fn present(&mut self, client: &mut Client, mode: &DisplayMode, viewed: bool) {
        self.owed.owe(mode, Repaint::Reported, &self.damage);
        let parts = self.owed.take(mode);
        let view_damage = if viewed { self.take_view_rows() } else { None };
        let presented = match (parts, view_damage) {
            (parts, Some(view_damage)) => {
                self.pane
                    .present_view(client, &self.surface, parts.as_ref(), &view_damage)
            }
            (Some(parts), None) => self.pane.present_list(client, &self.surface, &parts),
            (None, None) => Ok(()),
        };
        if let Err(err) = presented {
            app::report(
                APP_NAME,
                format_args!("the window could not be presented ({err})"),
            );
        }
    }

    /// Copy the canvas rows changed since the view was last presented into
    /// its picture, answering them as the view's damage.
    fn take_view_rows(&mut self) -> Option<DamageList> {
        let rows = self.view_rows.take().filter(|rows| !rows.is_empty())?;
        let screen = self.picture.screen();
        let (width, _) = screen.size();
        let stride = usize::try_from(width).ok()?.checked_mul(DOT)?;
        let picture = self.pane.view_picture_mut()?;
        for row in rows.clone() {
            let start = usize::try_from(row).ok()?.checked_mul(stride)?;
            let (Some(dots), Some((_, into))) = (
                screen.pixels().get(start..start + stride),
                picture.row_span_mut(row, 0, width),
            ) else {
                continue;
            };
            for (out, dot) in into.iter_mut().zip(dots.as_chunks::<DOT>().0) {
                let [r, g, b, a] = *dot;
                *out = Pixel { r, g, b, a };
            }
        }
        DamageList::new(&[DamageRect {
            x: 0,
            y: rows.start,
            width_px: width,
            height_px: rows.end - rows.start,
        }])
        .ok()
    }

    /// Present what the pane held back for a frame to come back.
    pub fn present_deferred(&mut self, client: &mut Client) {
        if let Err(err) = self.pane.present_deferred(client, &self.surface) {
            app::report(
                APP_NAME,
                format_args!("the window could not be presented ({err})"),
            );
        }
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
        Pending::Deck(key) => Request::Deck(*key),
        Pending::InsertCartridge(image) => Request::InsertCartridge(image),
        Pending::RemoveCartridge => Request::RemoveCartridge,
        Pending::LoadProgram(image) => Request::LoadProgram(image),
        Pending::PowerOn => Request::PowerOn,
        Pending::TakeSnapshot | Pending::SuspendSnapshot => Request::TakeSnapshot,
        Pending::RestoreSnapshot(snapshot) | Pending::Resume(snapshot) => {
            Request::RestoreSnapshot(snapshot)
        }
        Pending::CollectDiscs if collect.contains(&true) => Request::CollectDiscs(collect),
        Pending::CollectDiscs => return None,
        Pending::SaveDisc(drive) => {
            Request::CollectDiscs(PhysicalDrive::ALL.map(|other| other == *drive))
        }
    })
}

/// Why a medium the machine has no drive, deck or port for is refused.
pub const NOTHING_TO_TAKE_IT: &str = "The machine has nothing to take it.";

/// What the user is told of `refusal`.
const fn detail_of(refusal: Refused) -> &'static str {
    match refusal {
        Refused::Unconfigured => "There is no machine yet.",
        Refused::Rom => "A ROM image does not fit the machine.",
        Refused::Disc => "It is not a disc image the machine reads.",
        Refused::Tape => "It is not a tape image the machine reads.",
        Refused::Cartridge => "It is not a cartridge the machine takes.",
        Refused::Program => "It is not a program the machine loads.",
        Refused::Snapshot => "It is not a snapshot of this machine.",
        Refused::TooLarge => "It is too large.",
        Refused::Memory => "The machine ran out of memory.",
        Refused::Malformed => "The request was malformed.",
        Refused::Model => NOTHING_TO_TAKE_IT,
    }
}

/// What a save holds for the picker to offer.
#[must_use]
pub fn save_formats(holding: Holding) -> tairix_abi::window_ipc::SaveFormats {
    let mut formats = tairix_abi::window_ipc::SaveFormats::ANY;
    let own = match holding {
        Holding::Disc(DiscFormat::Acorn(_)) => tairix_browse::MediaType::AcornDfsDisc,
        Holding::Disc(DiscFormat::D64) => tairix_browse::MediaType::CommodoreD64,
        Holding::Disc(DiscFormat::G64) => tairix_browse::MediaType::CommodoreG64,
        Holding::Tape(Model::ModelB | Model::Electron) => tairix_browse::MediaType::TapeUef,
        Holding::Tape(Model::C64) => tairix_browse::MediaType::TapeTap,
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

/// The status strip: each of the model's lamps, lit or not, the message,
/// and the SHIFT-BREAK button at its trailing end where the model has one.
fn paint_strip<const N: usize>(
    surface: &mut Surface,
    strip: Rect,
    (scale, theme): (Scale, &Theme),
    (lamps, message): ([(&'static str, bool); N], &(String, StatusTone)),
    boot: Option<&Button>,
) {
    let band = StatusBand::new(theme, scale);
    let lamp = band.lamp();
    let widths = lamps.map(|(label, _)| lamp.slot_width(label));
    let button = boot.map_or(0, |button| button.measured_width(scale, theme));
    let slots = band.slots::<N, 1>(strip, widths, [button]);
    let mut parts: Vec<(Rect, &str, StatusTone)> = slots
        .leading
        .iter()
        .zip(lamps.iter())
        .map(|(slot, (label, _))| (lamp.text(*slot), *label, StatusTone::Muted))
        .collect();
    parts.push((slots.message, message.0.as_str(), message.1));
    band.paint(surface, strip, theme, &parts);
    for (slot, (_, lit)) in slots.leading.iter().zip(lamps.iter()) {
        let tone = if *lit {
            StatusTone::Danger
        } else {
            StatusTone::Muted
        };
        lamp.paint(surface, *slot, theme, tone);
    }
    if let Some(button) = boot {
        let at = StatusBand::button_in(slots.trailing[0], scale, theme);
        button.render(surface, at, scale, theme);
    }
}

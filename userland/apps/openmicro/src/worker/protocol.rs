//! The session's messages and their wire form: a tag byte, then each field,
//! every length bounded and every message decoded whole.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_sandbox::proto::{FRAME_HEADER_LEN, MAX_FRAME};
use tairix_sandbox::wire::{Reader, WireError, Writer, BYTES_PREFIX};
use tairix_sandbox::{SessionBounds, SessionError};

use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::c64::keyboard::Keycap;
use crate::machine::c64::{joystick, sid, Standard, PADDLE_CHANNELS};
use crate::machine::canvas::{CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::{
    bbc, electron, LetterCase, Model, Timing, ANALOGUE_CHANNELS, CLOCK, FIRE_BUTTONS, ROM_SIZE,
};
use crate::media::c1541::{d64, g64};
use crate::media::disc::{max_image_bytes, LAST_TRACK};
use crate::media::{crt, tap};
use tairix_dfs::image::Layout;

pub use crate::machine::SLOTS;

/// The share: the picture's canvas, a byte a slot, row after row.
pub const SHARE_BYTES: usize = CANVAS_WIDTH * CANVAS_HEIGHT;
/// The rate a reply's sound is at.
pub const SAMPLE_RATE: u32 = 48_000;
/// The longest slice one request runs: a second of the fastest model's
/// cycles. A machine runs no more than a second of its own at a time.
pub const MAX_SLICE: u32 = 2_000_000;
const _: () = assert!(
    MAX_SLICE as u64 == CLOCK && Standard::Pal.clock() <= CLOCK && Standard::Ntsc.clock() <= CLOCK
);
/// The most input changes a slice carries.
pub const MAX_INPUTS: usize = 256;
/// The most samples a reply carries: a whole slice's, with the resampler's
/// slack.
pub const MAX_SAMPLES: usize = SAMPLE_RATE as usize + 64;
/// A sample's bytes: signed 16-bit, the least significant first.
pub const SAMPLE_BYTES: usize = 2;
/// The largest image a ROM socket takes.
pub const MAX_ROM: usize = ROM_SIZE;
/// The largest disc image: a G64 of every ring at its longest.
pub const MAX_DISC: usize = DiscFormat::G64.max_bytes();
const _: () = assert!(max_image_bytes(Layout::Double) <= MAX_DISC && d64::MAX_IMAGE <= MAX_DISC);
/// The largest tape image or snapshot either side sends: far above any
/// real Acorn tape's, and the most a C64 tape holds.
pub const MAX_BULK: usize = 4 << 20;
const _: () = assert!(tap::MAX_IMAGE <= MAX_BULK);
/// The largest cartridge image.
pub const MAX_CARTRIDGE: usize = crt::MAX_IMAGE;
const _: () = assert!(ANALOGUE_CHANNELS == PADDLE_CHANNELS);
/// A written disc of `format`'s most bytes: its drive, its format, the flag
/// saying whether it lost anything, an Acorn disc's side and track where it
/// did, and its image with its length.
const fn written_bytes(format: DiscFormat) -> usize {
    let lost = match format {
        DiscFormat::Acorn(_) => 2,
        DiscFormat::D64 | DiscFormat::G64 => 0,
    };
    3 + lost + BYTES_PREFIX + format.max_bytes()
}
/// The most bytes a written disc of any format takes.
const MOST_WRITTEN: usize = {
    let mut most = 0;
    let mut at = 0;
    while at < DISC_FORMATS.len() {
        let bytes = written_bytes(DISC_FORMATS[at]);
        if bytes > most {
            most = bytes;
        }
        at += 1;
    }
    most
};
/// A message's most bytes, the bound each direction's queue is committed to:
/// media taken out of both drives and the deck, with their tag, count, flag
/// and lengths.
pub const MAX_MESSAGE: usize = 2 + 2 * MOST_WRITTEN + 1 + BYTES_PREFIX + MAX_BULK;
const _: () = assert!(MAX_MESSAGE <= MAX_FRAME);
/// A reply's map of the picture's changed rows: a bit a row, and the rows
/// its last byte holds.
pub const ROW_MAP: usize = CANVAS_HEIGHT.div_ceil(8);
const LAST_BYTE_ROWS: usize = CANVAS_HEIGHT - 8 * (ROW_MAP - 1);
mod tag {
    pub const CONFIGURE: u8 = 1;
    pub const INSERT_DISC: u8 = 2;
    pub const EJECT_DISC: u8 = 3;
    pub const INSERT_TAPE: u8 = 4;
    pub const EJECT_TAPE: u8 = 5;
    pub const REWIND_TAPE: u8 = 6;
    pub const RECORD_TAPE: u8 = 7;
    pub const RUN: u8 = 8;
    pub const POWER_ON: u8 = 9;
    pub const TAKE_SNAPSHOT: u8 = 10;
    pub const RESTORE_SNAPSHOT: u8 = 11;
    pub const COLLECT_DISCS: u8 = 12;
    pub const DECK: u8 = 13;
    pub const INSERT_CARTRIDGE: u8 = 14;
    pub const REMOVE_CARTRIDGE: u8 = 15;
    pub const LOAD_PROGRAM: u8 = 16;

    pub const DONE: u8 = 1;
    pub const RAN: u8 = 2;
    pub const SNAPSHOT: u8 = 3;
    pub const MEDIA: u8 = 4;
    pub const REFUSED: u8 = 5;
}

/// What a sideways slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot<'a> {
    /// Nothing.
    Empty,
    /// A ROM image.
    Rom(&'a [u8]),
    /// Sixteen kilobytes of RAM.
    Ram,
}

/// What the Electron's Plus 1 holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plus1<'a> {
    /// Its ROM, in slot 12.
    pub rom: &'a [u8],
    /// Each cartridge slot's two images, as
    /// [`electron::CARTRIDGE_ROMS`] numbers their ROMs.
    pub cartridges: [[Option<&'a [u8]>; 2]; 2],
}

/// The machine a session builds. A session's model is its first
/// configure's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Configuration<'a> {
    /// A Model B.
    ModelB {
        /// The OS image.
        os: &'a [u8],
        /// Each slot, 0 to 15.
        slots: Box<[Slot<'a>; SLOTS]>,
        /// The start-up links, as the keyboard reads them.
        links: u8,
    },
    /// An Electron.
    Electron {
        /// The OS image.
        os: &'a [u8],
        /// BASIC's image, in ROMs 10 and 11.
        basic: &'a [u8],
        /// The Plus 1, if fitted.
        plus1: Option<Plus1<'a>>,
    },
    /// A Commodore 64.
    C64 {
        /// Its television standard.
        standard: Standard,
        /// Its SID.
        sid: sid::Model,
        /// The KERNAL.
        kernal: &'a [u8],
        /// BASIC.
        basic: &'a [u8],
        /// The character generator.
        character: &'a [u8],
        /// The DOS each drive fitted runs, devices 8 and 9.
        drives: [Option<&'a [u8]>; 2],
    },
}

impl Configuration<'_> {
    /// The model it builds.
    #[must_use]
    pub const fn model(&self) -> Model {
        match self {
            Self::ModelB { .. } => Model::ModelB,
            Self::Electron { .. } => Model::Electron,
            Self::C64 { .. } => Model::C64,
        }
    }

    /// How the machine it builds keeps time.
    #[must_use]
    pub const fn timing(&self) -> Timing {
        match self {
            Self::ModelB { .. } | Self::Electron { .. } => Timing::ACORN,
            Self::C64 { standard, .. } => standard.timing(),
        }
    }

    /// Which of its two drives it has.
    #[must_use]
    pub const fn drives(&self) -> [bool; 2] {
        match self {
            Self::ModelB { .. } => [true; 2],
            Self::Electron { .. } => [false; 2],
            Self::C64 { drives, .. } => [drives[0].is_some(), drives[1].is_some()],
        }
    }
}

/// How a disc image holds a disc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscFormat {
    /// An Acorn DFS disc, its sides laid out so.
    Acorn(Layout),
    /// A 1541's sector dump.
    D64,
    /// A 1541's recording as it is.
    G64,
}

impl DiscFormat {
    /// Its largest image.
    #[must_use]
    pub const fn max_bytes(self) -> usize {
        match self {
            Self::Acorn(layout) => max_image_bytes(layout),
            Self::D64 => d64::MAX_IMAGE,
            Self::G64 => g64::MAX_IMAGE,
        }
    }

    /// The model whose drives take it.
    #[must_use]
    pub const fn model(self) -> Model {
        match self {
            Self::Acorn(_) => Model::ModelB,
            Self::D64 | Self::G64 => Model::C64,
        }
    }
}

/// A key of one model's keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKey {
    /// The Model B's.
    ModelB(bbc::keyboard::Key),
    /// The Electron's.
    Electron(electron::keyboard::Key),
    /// The Commodore 64's, RESTORE among them.
    C64(Keycap),
}

impl ModelKey {
    /// The model whose keyboard it is on.
    #[must_use]
    pub const fn model(self) -> Model {
        match self {
            Self::ModelB(_) => Model::ModelB,
            Self::Electron(_) => Model::Electron,
            Self::C64(_) => Model::C64,
        }
    }

    const fn number(self) -> u8 {
        match self {
            Self::ModelB(key) => key.number(),
            Self::Electron(key) => key.number(),
            Self::C64(keycap) => keycap.number(),
        }
    }

    /// `model`'s key `number`, if its keyboard has one.
    fn of(model: Model, number: u8) -> Option<Self> {
        match model {
            Model::ModelB => bbc::keyboard::Key::from_number(number).map(Self::ModelB),
            Model::Electron => electron::keyboard::Key::from_number(number).map(Self::Electron),
            Model::C64 => Keycap::from_number(number).map(Self::C64),
        }
    }
}

/// A change to the machine's input at a cycle of a slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    /// Cycles from the slice's start.
    pub at: u32,
    /// The change.
    pub change: Change,
}

/// What changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A key goes down or up.
    Key(ModelKey, bool),
    /// BREAK goes down or up.
    Break(bool),
    /// An analogue channel's level, 0 to 65535.
    Analogue(u8, u16),
    /// The fire buttons held, bit 0 the first.
    Fire(u8),
    /// A C64 control port's joystick lines held, from [`joystick`]: its
    /// port, 0 for port 1, and the lines.
    Joystick(u8, u8),
}

/// A datasette's keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckKey {
    /// PLAY.
    Play,
    /// RECORD, with PLAY.
    Record,
    /// STOP, which lets both up.
    Stop,
}

const DECK_KEYS: [DeckKey; 3] = [DeckKey::Play, DeckKey::Record, DeckKey::Stop];

/// A request from the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    /// Build the machine anew from these images, at power-on, its media
    /// kept.
    Configure(Box<Configuration<'a>>),
    /// Put a disc image in a drive, answered with the disc it replaces if
    /// that was written since last collected.
    InsertDisc {
        /// The drive: on a C64, device 8 or 9.
        drive: PhysicalDrive,
        /// How the image holds its disc.
        format: DiscFormat,
        /// Whether its write-protect tab is open.
        write_protected: bool,
        /// The image.
        image: &'a [u8],
    },
    /// Take a drive's disc out, answered with it if it was written since
    /// last collected.
    EjectDisc(PhysicalDrive),
    /// Put a tape image in the recorder, answered with the tape it replaces
    /// if that was recorded on.
    InsertTape(&'a [u8]),
    /// Take the tape out, answered with it if it was recorded on.
    EjectTape,
    /// Wind the tape back to its start.
    RewindTape,
    /// Press or release RECORD.
    RecordTape(bool),
    /// Run a slice: its cycles, whether to keep its sound, and the input's
    /// changes within it in the order they happen.
    Run {
        /// Cycles to run, 1 to [`MAX_SLICE`] and to a second of the
        /// machine's.
        cycles: u32,
        /// Whether the reply carries the slice's sound.
        sound: bool,
        /// The changes, each at a cycle before `cycles`, none before the
        /// last.
        inputs: Vec<Input>,
    },
    /// Switch the machine off and on, its media kept.
    PowerOn,
    /// Snapshot the whole machine.
    TakeSnapshot,
    /// Take a snapshot back, answered with the media it replaces: the discs
    /// written since last collected, and the tape if it was recorded on.
    RestoreSnapshot(&'a [u8]),
    /// Hand over the disc in each drive marked if it was written since last
    /// collected; the discs stay in. Unmarked drives keep theirs unsaved, to
    /// be handed over as they are taken out.
    CollectDiscs([bool; 2]),
    /// Press a datasette key.
    Deck(DeckKey),
    /// Plug in the cartridge a CRT image holds, resetting the machine.
    InsertCartridge(&'a [u8]),
    /// Unplug the cartridge, resetting the machine.
    RemoveCartridge,
    /// Load a PRG, or a T64's first program, into memory as LOAD leaves it.
    LoadProgram(&'a [u8]),
}

/// Why the worker refused a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No machine is configured yet.
    Unconfigured,
    /// A ROM image the machine cannot hold.
    Rom,
    /// A disc image that does not decode.
    Disc,
    /// A tape image that does not decode.
    Tape,
    /// A cartridge image that does not decode, or whose hardware is not
    /// emulated.
    Cartridge,
    /// A program file that does not decode.
    Program,
    /// A snapshot that does not restore onto this machine.
    Snapshot,
    /// What was asked for, or the recorded tape it would take out, does not
    /// fit a message.
    TooLarge,
    /// The worker ran out of memory.
    Memory,
    /// The request did not decode.
    Malformed,
    /// The machine's model cannot take it: a configure for another model or
    /// one leaving a disc without its drive, a disc of another model's or
    /// for a drive not fitted, a key of another keyboard, an input or a
    /// medium it has no port for, or a slice longer than a second of its
    /// cycles.
    Model,
}

const REFUSALS: [Refusal; 11] = [
    Refusal::Unconfigured,
    Refusal::Rom,
    Refusal::Disc,
    Refusal::Tape,
    Refusal::Cartridge,
    Refusal::Program,
    Refusal::Snapshot,
    Refusal::TooLarge,
    Refusal::Memory,
    Refusal::Malformed,
    Refusal::Model,
];

/// What a slice left: the picture's changed rows, its sound, the machine's
/// lights and how its letter keys type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran<'a> {
    /// The cycles run.
    pub cycles: u32,
    /// Each row of the canvas that changed and is written in the share.
    pub rows: [u8; ROW_MAP],
    /// The slice's sound at [`SAMPLE_RATE`], if asked for: mono samples of
    /// [`SAMPLE_BYTES`].
    pub sound: &'a [u8],
    /// CAPS LOCK's and SHIFT LOCK's LEDs, and the cassette motor.
    pub lights: Lights,
    /// How the letter keys type letters as the slice ends.
    pub case: LetterCase,
    /// Each drive's activity light.
    pub drives: [bool; 2],
    /// Whether each drive's disc turns.
    pub turning: [bool; 2],
    /// Whether the machine's sound moved in the slice, heard or not.
    pub audible: bool,
    /// What is in the drives and the deck.
    pub loaded: Loaded,
}

/// The media the machine holds, and its deck's keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent medium or key"
)]
pub struct Loaded {
    /// Whether each drive holds a disc.
    pub discs: [bool; 2],
    /// Whether the deck holds a tape.
    pub tape: bool,
    /// Whether RECORD is down.
    pub recording: bool,
    /// Whether a C64 datasette's PLAY is down.
    pub playing: bool,
    /// Whether a C64 has a cartridge plugged in.
    pub cartridge: bool,
}

/// The keyboard's lock lights and the cassette motor's relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lights {
    /// CAPS LOCK's LED.
    pub caps_lock: bool,
    /// SHIFT LOCK's LED.
    pub shift_lock: bool,
    /// The cassette motor.
    pub motor: bool,
}

/// Each letter case's two bits on the wire, in their order.
const CASES: [LetterCase; 3] = [
    LetterCase::Small,
    LetterCase::Capitals,
    LetterCase::CapitalsOnly,
];

fn case_of(bits: u8) -> Option<LetterCase> {
    CASES.get(usize::from(bits)).copied()
}

impl Ran<'_> {
    /// Whether `row` changed.
    #[must_use]
    pub const fn row_changed(&self, row: usize) -> bool {
        row < CANVAS_HEIGHT && self.rows[row / 8] & (1 << (row % 8)) != 0
    }
}

/// Media a request took out of the machine, or the discs it collected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Media<'a> {
    /// Each disc written since last collected.
    pub discs: Vec<Written<'a>>,
    /// The tape, when it was recorded on: a UEF image of an Acorn's, a TAP
    /// image of a C64's.
    pub tape: Option<&'a [u8]>,
}

/// A disc written since last collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written<'a> {
    /// Its drive.
    pub drive: PhysicalDrive,
    /// How its image holds it: a 1541 disk no sector dump can hold any
    /// longer comes back as a G64.
    pub format: DiscFormat,
    /// Its image, no larger than its format's largest.
    pub image: &'a [u8],
    /// On an Acorn disc, the first side and track holding what the image
    /// cannot record, if any: the image is saved without it.
    pub lost: Option<(u8, u8)>,
}

/// A reply from the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply<'a> {
    /// The request is done.
    Done,
    /// A slice ran.
    Ran(Ran<'a>),
    /// The snapshot asked for.
    Snapshot(&'a [u8]),
    /// The media taken out or collected.
    Media(Media<'a>),
    /// The request was refused.
    Refused(Refusal),
}

impl Request<'_> {
    /// The request's wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::Configure(configuration) => {
                w.u8(tag::CONFIGURE);
                configuration.encode(&mut w);
            }
            Self::InsertDisc {
                drive,
                format,
                write_protected,
                image,
            } => {
                w.u8(tag::INSERT_DISC);
                w.u8(drive_byte(*drive));
                w.u8(disc_format_byte(*format));
                w.u8(u8::from(*write_protected));
                w.bytes(image);
            }
            Self::EjectDisc(drive) => {
                w.u8(tag::EJECT_DISC);
                w.u8(drive_byte(*drive));
            }
            Self::InsertTape(image) => {
                w.u8(tag::INSERT_TAPE);
                w.bytes(image);
            }
            Self::EjectTape => w.u8(tag::EJECT_TAPE),
            Self::RewindTape => w.u8(tag::REWIND_TAPE),
            Self::RecordTape(on) => {
                w.u8(tag::RECORD_TAPE);
                w.u8(u8::from(*on));
            }
            Self::Run {
                cycles,
                sound,
                inputs,
            } => {
                w.u8(tag::RUN);
                w.u32(*cycles);
                w.u8(u8::from(*sound));
                w.u16(u16::try_from(inputs.len()).unwrap_or(u16::MAX));
                for input in inputs {
                    w.u32(input.at);
                    input.change.encode(&mut w);
                }
            }
            Self::PowerOn => w.u8(tag::POWER_ON),
            Self::TakeSnapshot => w.u8(tag::TAKE_SNAPSHOT),
            Self::RestoreSnapshot(snapshot) => {
                w.u8(tag::RESTORE_SNAPSHOT);
                w.bytes(snapshot);
            }
            Self::CollectDiscs(drives) => {
                w.u8(tag::COLLECT_DISCS);
                w.u8(u8::from(drives[0]) | u8::from(drives[1]) << 1);
            }
            Self::Deck(key) => {
                w.u8(tag::DECK);
                w.u8(position(&DECK_KEYS, key));
            }
            Self::InsertCartridge(image) => {
                w.u8(tag::INSERT_CARTRIDGE);
                w.bytes(image);
            }
            Self::RemoveCartridge => w.u8(tag::REMOVE_CARTRIDGE),
            Self::LoadProgram(image) => {
                w.u8(tag::LOAD_PROGRAM);
                w.bytes(image);
            }
        }
        w.finish()
    }
}

impl Configuration<'_> {
    fn encode(&self, w: &mut Writer) {
        w.u8(self.model().number());
        match self {
            Self::ModelB { os, slots, links } => {
                w.bytes(os);
                for slot in slots.iter() {
                    match slot {
                        Slot::Empty => w.u8(0),
                        Slot::Rom(image) => {
                            w.u8(1);
                            w.bytes(image);
                        }
                        Slot::Ram => w.u8(2),
                    }
                }
                w.u8(*links);
            }
            Self::Electron { os, basic, plus1 } => {
                w.bytes(os);
                w.bytes(basic);
                match plus1 {
                    None => w.u8(0),
                    Some(plus1) => {
                        w.u8(1);
                        w.bytes(plus1.rom);
                        for image in plus1.cartridges.iter().flatten() {
                            optional(w, *image);
                        }
                    }
                }
            }
            Self::C64 {
                standard,
                sid,
                kernal,
                basic,
                character,
                drives,
            } => {
                w.u8(standard.number());
                w.u8(sid.number());
                w.bytes(kernal);
                w.bytes(basic);
                w.bytes(character);
                for dos in drives {
                    optional(w, *dos);
                }
            }
        }
    }
}

/// An image that may be absent: a flag, then the image where present.
fn optional(w: &mut Writer, image: Option<&[u8]>) {
    match image {
        None => w.u8(0),
        Some(image) => {
            w.u8(1);
            w.bytes(image);
        }
    }
}

/// An image [`optional`] wrote.
fn get_optional<'a>(r: &mut Reader<'a>, max: usize) -> Result<Option<&'a [u8]>, WireError> {
    Ok(if flag(r.u8()?)? {
        Some(r.bytes(max)?)
    } else {
        None
    })
}

/// `value`'s place in `all`, the byte the wire or a record carries it as.
pub(crate) fn position<T: PartialEq>(all: &[T], value: &T) -> u8 {
    all.iter()
        .position(|candidate| candidate == value)
        .and_then(|at| u8::try_from(at).ok())
        .unwrap_or(u8::MAX)
}

impl Change {
    fn encode(self, w: &mut Writer) {
        match self {
            Self::Key(key, down) => {
                w.u8(0);
                w.u8(key.model().number());
                w.u8(key.number());
                w.u8(u8::from(down));
            }
            Self::Break(down) => {
                w.u8(1);
                w.u8(u8::from(down));
            }
            Self::Analogue(channel, level) => {
                w.u8(2);
                w.u8(channel);
                w.u16(level);
            }
            Self::Fire(buttons) => {
                w.u8(3);
                w.u8(buttons);
            }
            Self::Joystick(port, lines) => {
                w.u8(4);
                w.u8(port);
                w.u8(lines);
            }
        }
    }
}

impl<'a> Request<'a> {
    /// The request `bytes` hold, refused unless every field is in bounds and
    /// nothing follows the last.
    ///
    /// # Errors
    ///
    /// [`WireError`] for one that does not decode.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        Reader::new(bytes).whole(|r| {
            Ok(match r.u8()? {
                tag::CONFIGURE => Self::Configure(Box::new(configuration(r)?)),
                tag::INSERT_DISC => {
                    let drive = drive_of(r.u8()?)?;
                    let format = disc_format_of(r.u8()?)?;
                    let write_protected = flag(r.u8()?)?;
                    Self::InsertDisc {
                        drive,
                        format,
                        write_protected,
                        image: r.bytes(format.max_bytes())?,
                    }
                }
                tag::EJECT_DISC => Self::EjectDisc(drive_of(r.u8()?)?),
                tag::INSERT_TAPE => Self::InsertTape(r.bytes(MAX_BULK)?),
                tag::EJECT_TAPE => Self::EjectTape,
                tag::REWIND_TAPE => Self::RewindTape,
                tag::RECORD_TAPE => Self::RecordTape(flag(r.u8()?)?),
                tag::RUN => {
                    let cycles = r.u32()?;
                    if cycles == 0 || cycles > MAX_SLICE {
                        return Err(WireError::Malformed);
                    }
                    let sound = flag(r.u8()?)?;
                    let count = usize::from(r.u16()?);
                    if count > MAX_INPUTS {
                        return Err(WireError::Malformed);
                    }
                    let mut inputs = Vec::new();
                    inputs
                        .try_reserve_exact(count)
                        .map_err(|_| WireError::Malformed)?;
                    let mut last = 0;
                    for _ in 0..count {
                        let at = r.u32()?;
                        if at < last || at >= cycles {
                            return Err(WireError::Malformed);
                        }
                        last = at;
                        inputs.push(Input {
                            at,
                            change: change(r)?,
                        });
                    }
                    Self::Run {
                        cycles,
                        sound,
                        inputs,
                    }
                }
                tag::POWER_ON => Self::PowerOn,
                tag::TAKE_SNAPSHOT => Self::TakeSnapshot,
                tag::RESTORE_SNAPSHOT => Self::RestoreSnapshot(r.bytes(MAX_BULK)?),
                tag::COLLECT_DISCS => {
                    let drives = r.u8()?;
                    if drives >> 2 != 0 {
                        return Err(WireError::Malformed);
                    }
                    Self::CollectDiscs([drives & 1 != 0, drives & 2 != 0])
                }
                tag::DECK => Self::Deck(
                    *DECK_KEYS
                        .get(usize::from(r.u8()?))
                        .ok_or(WireError::Malformed)?,
                ),
                tag::INSERT_CARTRIDGE => Self::InsertCartridge(r.bytes(MAX_CARTRIDGE)?),
                tag::REMOVE_CARTRIDGE => Self::RemoveCartridge,
                tag::LOAD_PROGRAM => Self::LoadProgram(r.bytes(MAX_BULK)?),
                _ => return Err(WireError::Malformed),
            })
        })
    }
}

fn configuration<'a>(r: &mut Reader<'a>) -> Result<Configuration<'a>, WireError> {
    Ok(match model_of(r.u8()?)? {
        Model::ModelB => {
            let os = r.bytes(MAX_ROM)?;
            let mut slots = Box::new([Slot::Empty; SLOTS]);
            for slot in slots.iter_mut() {
                *slot = match r.u8()? {
                    0 => Slot::Empty,
                    1 => Slot::Rom(r.bytes(MAX_ROM)?),
                    2 => Slot::Ram,
                    _ => return Err(WireError::Malformed),
                };
            }
            Configuration::ModelB {
                os,
                slots,
                links: r.u8()?,
            }
        }
        Model::Electron => {
            let (os, basic) = (r.bytes(MAX_ROM)?, r.bytes(MAX_ROM)?);
            let plus1 = if flag(r.u8()?)? {
                let rom = r.bytes(MAX_ROM)?;
                let mut cartridges = [[None; 2]; 2];
                for image in cartridges.iter_mut().flatten() {
                    *image = get_optional(r, MAX_ROM)?;
                }
                Some(Plus1 { rom, cartridges })
            } else {
                None
            };
            Configuration::Electron { os, basic, plus1 }
        }
        Model::C64 => {
            let standard = Standard::from_number(r.u8()?).ok_or(WireError::Malformed)?;
            let sid = sid::Model::from_number(r.u8()?).ok_or(WireError::Malformed)?;
            let (kernal, basic, character) =
                (r.bytes(MAX_ROM)?, r.bytes(MAX_ROM)?, r.bytes(MAX_ROM)?);
            let mut drives = [None; 2];
            for dos in &mut drives {
                *dos = get_optional(r, MAX_ROM)?;
            }
            Configuration::C64 {
                standard,
                sid,
                kernal,
                basic,
                character,
                drives,
            }
        }
    })
}

fn change(r: &mut Reader<'_>) -> Result<Change, WireError> {
    Ok(match r.u8()? {
        0 => {
            let model = model_of(r.u8()?)?;
            Change::Key(
                ModelKey::of(model, r.u8()?).ok_or(WireError::Malformed)?,
                flag(r.u8()?)?,
            )
        }
        1 => Change::Break(flag(r.u8()?)?),
        2 => {
            let channel = r.u8()?;
            if usize::from(channel) >= ANALOGUE_CHANNELS {
                return Err(WireError::Malformed);
            }
            Change::Analogue(channel, r.u16()?)
        }
        3 => {
            let buttons = r.u8()?;
            if buttons & !FIRE_BUTTONS != 0 {
                return Err(WireError::Malformed);
            }
            Change::Fire(buttons)
        }
        4 => {
            let (port, lines) = (r.u8()?, r.u8()?);
            if port > 1 || lines & !joystick::ALL != 0 {
                return Err(WireError::Malformed);
            }
            Change::Joystick(port, lines)
        }
        _ => return Err(WireError::Malformed),
    })
}

impl Reply<'_> {
    /// The reply's wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::Done => w.u8(tag::DONE),
            Self::Ran(ran) => {
                w.u8(tag::RAN);
                w.u32(ran.cycles);
                w.raw(&ran.rows);
                w.bytes(ran.sound);
                w.u8(u8::from(ran.lights.caps_lock)
                    | u8::from(ran.lights.shift_lock) << 1
                    | u8::from(ran.lights.motor) << 2
                    | position(&CASES, &ran.case) << 3
                    | u8::from(ran.audible) << 5);
                w.u8(u8::from(ran.drives[0])
                    | u8::from(ran.drives[1]) << 1
                    | u8::from(ran.turning[0]) << 2
                    | u8::from(ran.turning[1]) << 3);
                w.u8(u8::from(ran.loaded.discs[0])
                    | u8::from(ran.loaded.discs[1]) << 1
                    | u8::from(ran.loaded.tape) << 2
                    | u8::from(ran.loaded.recording) << 3
                    | u8::from(ran.loaded.playing) << 4
                    | u8::from(ran.loaded.cartridge) << 5);
            }
            Self::Snapshot(snapshot) => {
                w.u8(tag::SNAPSHOT);
                w.bytes(snapshot);
            }
            Self::Media(media) => {
                w.u8(tag::MEDIA);
                w.u8(u8::try_from(media.discs.len()).unwrap_or(u8::MAX));
                for written in &media.discs {
                    w.u8(drive_byte(written.drive));
                    w.u8(disc_format_byte(written.format));
                    match written.lost {
                        None => w.u8(0),
                        Some((side, track)) => {
                            w.u8(1);
                            w.u8(side);
                            w.u8(track);
                        }
                    }
                    w.bytes(written.image);
                }
                match media.tape {
                    None => w.u8(0),
                    Some(image) => {
                        w.u8(1);
                        w.bytes(image);
                    }
                }
            }
            Self::Refused(refusal) => {
                w.u8(tag::REFUSED);
                w.u8(position(&REFUSALS, refusal));
            }
        }
        w.finish()
    }
}

impl<'a> Reply<'a> {
    /// The reply `bytes` hold, refused unless every field is in bounds and
    /// nothing follows the last.
    ///
    /// # Errors
    ///
    /// [`WireError`] for one that does not decode.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        Reader::new(bytes).whole(|r| {
            Ok(match r.u8()? {
                tag::DONE => Self::Done,
                tag::RAN => {
                    let cycles = r.u32()?;
                    let mut rows = [0; ROW_MAP];
                    rows.copy_from_slice(r.take(ROW_MAP)?);
                    if u16::from(rows[ROW_MAP - 1]) >> LAST_BYTE_ROWS != 0 {
                        return Err(WireError::Malformed);
                    }
                    let sound = r.bytes(MAX_SAMPLES * SAMPLE_BYTES)?;
                    if !sound.len().is_multiple_of(SAMPLE_BYTES) {
                        return Err(WireError::Malformed);
                    }
                    let (lights, drives, loaded) = (r.u8()?, r.u8()?, r.u8()?);
                    if lights >> 6 != 0 || drives >> 4 != 0 || loaded >> 6 != 0 {
                        return Err(WireError::Malformed);
                    }
                    let case = case_of((lights >> 3) & 3).ok_or(WireError::Malformed)?;
                    Self::Ran(Ran {
                        cycles,
                        rows,
                        sound,
                        lights: Lights {
                            caps_lock: lights & 1 != 0,
                            shift_lock: lights & 2 != 0,
                            motor: lights & 4 != 0,
                        },
                        case,
                        drives: [drives & 1 != 0, drives & 2 != 0],
                        turning: [drives & 4 != 0, drives & 8 != 0],
                        audible: lights & 0x20 != 0,
                        loaded: Loaded {
                            discs: [loaded & 1 != 0, loaded & 2 != 0],
                            tape: loaded & 4 != 0,
                            recording: loaded & 8 != 0,
                            playing: loaded & 0x10 != 0,
                            cartridge: loaded & 0x20 != 0,
                        },
                    })
                }
                tag::SNAPSHOT => Self::Snapshot(r.bytes(MAX_BULK)?),
                tag::MEDIA => {
                    let count = r.u8()?;
                    if count > 2 {
                        return Err(WireError::Malformed);
                    }
                    let mut discs = Vec::new();
                    for _ in 0..count {
                        let drive = drive_of(r.u8()?)?;
                        if discs.iter().any(|held: &Written<'_>| held.drive == drive) {
                            return Err(WireError::Malformed);
                        }
                        let format = disc_format_of(r.u8()?)?;
                        let lost = if flag(r.u8()?)? {
                            let (side, track) = (r.u8()?, r.u8()?);
                            let DiscFormat::Acorn(acorn) = format else {
                                return Err(WireError::Malformed);
                            };
                            if usize::from(side) >= acorn.sides() || track > LAST_TRACK {
                                return Err(WireError::Malformed);
                            }
                            Some((side, track))
                        } else {
                            None
                        };
                        discs.push(Written {
                            drive,
                            format,
                            image: r.bytes(format.max_bytes())?,
                            lost,
                        });
                    }
                    let tape = if flag(r.u8()?)? {
                        Some(r.bytes(MAX_BULK)?)
                    } else {
                        None
                    };
                    Self::Media(Media { discs, tape })
                }
                tag::REFUSED => Self::Refused(
                    *REFUSALS
                        .get(usize::from(r.u8()?))
                        .ok_or(WireError::Malformed)?,
                ),
                _ => return Err(WireError::Malformed),
            })
        })
    }
}

/// The session's bounds: a whole message queued each way.
///
/// # Errors
///
/// None in practice: both exceed the session's minimum, which
/// [`SessionBounds::new`] still checks.
pub const fn session_bounds() -> Result<SessionBounds, SessionError> {
    SessionBounds::new(
        FRAME_HEADER_LEN + MAX_MESSAGE,
        FRAME_HEADER_LEN + MAX_MESSAGE,
    )
}

const fn drive_byte(drive: PhysicalDrive) -> u8 {
    match drive {
        PhysicalDrive::Zero => 0,
        PhysicalDrive::One => 1,
    }
}

const fn drive_of(byte: u8) -> Result<PhysicalDrive, WireError> {
    match byte {
        0 => Ok(PhysicalDrive::Zero),
        1 => Ok(PhysicalDrive::One),
        _ => Err(WireError::Malformed),
    }
}

const DISC_FORMATS: [DiscFormat; 4] = [
    DiscFormat::Acorn(Layout::Single),
    DiscFormat::Acorn(Layout::Double),
    DiscFormat::D64,
    DiscFormat::G64,
];

fn disc_format_byte(format: DiscFormat) -> u8 {
    position(&DISC_FORMATS, &format)
}

fn disc_format_of(byte: u8) -> Result<DiscFormat, WireError> {
    DISC_FORMATS
        .get(usize::from(byte))
        .copied()
        .ok_or(WireError::Malformed)
}

fn model_of(byte: u8) -> Result<Model, WireError> {
    Model::from_number(byte).ok_or(WireError::Malformed)
}

const fn flag(byte: u8) -> Result<bool, WireError> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

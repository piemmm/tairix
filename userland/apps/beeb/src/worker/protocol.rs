//! The session's messages and their wire form: a tag byte, then each field,
//! every length bounded and every message decoded whole.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_sandbox::proto::{FRAME_HEADER_LEN, MAX_FRAME};
use tairix_sandbox::wire::{Reader, WireError, Writer, BYTES_PREFIX};
use tairix_sandbox::{SessionBounds, SessionError};

use crate::machine::fdc::PhysicalDrive;
use crate::machine::keyboard::Key;
use crate::machine::video::{CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::CLOCK;
use crate::media::disc::{ImageFormat, LAST_TRACK};

/// The share: the picture's canvas, a byte a slot, row after row.
pub const SHARE_BYTES: usize = CANVAS_WIDTH * CANVAS_HEIGHT;
/// The rate a reply's sound is at.
pub const SAMPLE_RATE: u32 = 48_000;
/// The longest slice one request runs: a second.
pub const MAX_SLICE: u32 = 2_000_000;
const _: () = assert!(MAX_SLICE as u64 == CLOCK);
/// The most input changes a slice carries.
pub const MAX_INPUTS: usize = 256;
/// The most samples a reply carries: a whole slice's, with the resampler's
/// slack.
pub const MAX_SAMPLES: usize = SAMPLE_RATE as usize + 64;
/// A sample's bytes: signed 16-bit, the least significant first.
pub const SAMPLE_BYTES: usize = 2;
/// The sideways slots.
pub const SLOTS: usize = 16;
/// The largest image a slot or the OS takes.
pub const MAX_ROM: usize = 0x4000;
/// The largest disc image: both sides of every track.
pub const MAX_DISC: usize = ImageFormat::Dsd.max_bytes();
/// The largest tape image or snapshot either side sends: far above any
/// real tape's.
pub const MAX_BULK: usize = 4 << 20;
/// A written disc's bytes besides its image: its drive, its format, what it
/// lost with their flag, and its length.
const WRITTEN_FIELDS: usize = 5 + BYTES_PREFIX;
/// A message's most bytes, the bound each direction's queue is committed to:
/// media taken out of both drives and the deck, with their tag, count, flag
/// and lengths.
pub const MAX_MESSAGE: usize = 2 + 2 * (WRITTEN_FIELDS + MAX_DISC) + 1 + BYTES_PREFIX + MAX_BULK;
const _: () = assert!(MAX_MESSAGE <= MAX_FRAME);
/// A reply's map of the picture's changed rows: a bit a row, and the rows
/// its last byte holds.
pub const ROW_MAP: usize = CANVAS_HEIGHT.div_ceil(8);
const LAST_BYTE_ROWS: usize = CANVAS_HEIGHT - 8 * (ROW_MAP - 1);
/// The analogue channels.
pub const ANALOGUE_CHANNELS: u8 = 4;
/// The fire buttons' bits.
pub const FIRE_BUTTONS: u8 = 0b11;

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

/// The machine a session builds: its OS, what each sideways slot holds,
/// and the keyboard's start-up links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Configuration<'a> {
    /// The OS image.
    pub os: &'a [u8],
    /// Each slot, 0 to 15.
    pub slots: [Slot<'a>; SLOTS],
    /// The start-up links, as the keyboard reads them.
    pub links: u8,
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
    Key(Key, bool),
    /// BREAK goes down or up.
    Break(bool),
    /// An analogue channel's level, 0 to 65535.
    Analogue(u8, u16),
    /// The fire buttons held, bit 0 the first.
    Fire(u8),
}

/// A request from the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    /// Build the machine anew from these images, at power-on, its media
    /// kept.
    Configure(Box<Configuration<'a>>),
    /// Put a disc image in a drive, answered with the disc it replaces if
    /// that was written since last collected.
    InsertDisc {
        /// The drive.
        drive: PhysicalDrive,
        /// How its sides are laid out.
        format: ImageFormat,
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
        /// Cycles to run, 1 to [`MAX_SLICE`].
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
    /// A snapshot that does not restore onto this machine.
    Snapshot,
    /// What was asked for, or the recorded tape it would take out, does not
    /// fit a message.
    TooLarge,
    /// The worker ran out of memory.
    Memory,
    /// The request did not decode.
    Malformed,
}

const REFUSALS: [Refusal; 8] = [
    Refusal::Unconfigured,
    Refusal::Rom,
    Refusal::Disc,
    Refusal::Tape,
    Refusal::Snapshot,
    Refusal::TooLarge,
    Refusal::Memory,
    Refusal::Malformed,
];

/// What a slice left: the picture's changed rows, its sound and the
/// machine's lights.
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
    /// Each drive's activity.
    pub drives: [bool; 2],
    /// What is in the drives and the deck.
    pub loaded: Loaded,
}

/// The media the machine holds, and its deck's record key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    /// Whether each drive holds a disc.
    pub discs: [bool; 2],
    /// Whether the deck holds a tape.
    pub tape: bool,
    /// Whether RECORD is down.
    pub recording: bool,
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
    /// The tape, as a UEF image, when it was recorded on.
    pub tape: Option<&'a [u8]>,
}

/// A disc written since last collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written<'a> {
    /// Its drive.
    pub drive: PhysicalDrive,
    /// How its image lays its sides out.
    pub format: ImageFormat,
    /// Its image, no larger than its format's largest.
    pub image: &'a [u8],
    /// The first side and track holding what the image cannot record, if
    /// any: the image is saved without it.
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
                w.bytes(configuration.os);
                for slot in &configuration.slots {
                    match slot {
                        Slot::Empty => w.u8(0),
                        Slot::Rom(image) => {
                            w.u8(1);
                            w.bytes(image);
                        }
                        Slot::Ram => w.u8(2),
                    }
                }
                w.u8(configuration.links);
            }
            Self::InsertDisc {
                drive,
                format,
                write_protected,
                image,
            } => {
                w.u8(tag::INSERT_DISC);
                w.u8(drive_byte(*drive));
                w.u8(format_byte(*format));
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
                    match input.change {
                        Change::Key(key, down) => {
                            w.u8(0);
                            w.u8(key.number());
                            w.u8(u8::from(down));
                        }
                        Change::Break(down) => {
                            w.u8(1);
                            w.u8(u8::from(down));
                        }
                        Change::Analogue(channel, level) => {
                            w.u8(2);
                            w.u8(channel);
                            w.u16(level);
                        }
                        Change::Fire(buttons) => {
                            w.u8(3);
                            w.u8(buttons);
                        }
                    }
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
        }
        w.finish()
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
                tag::CONFIGURE => {
                    let os = r.bytes(MAX_ROM)?;
                    let mut slots = [Slot::Empty; SLOTS];
                    for slot in &mut slots {
                        *slot = match r.u8()? {
                            0 => Slot::Empty,
                            1 => Slot::Rom(r.bytes(MAX_ROM)?),
                            2 => Slot::Ram,
                            _ => return Err(WireError::Malformed),
                        };
                    }
                    Self::Configure(Box::new(Configuration {
                        os,
                        slots,
                        links: r.u8()?,
                    }))
                }
                tag::INSERT_DISC => Self::InsertDisc {
                    drive: drive_of(r.u8()?)?,
                    format: format_of(r.u8()?)?,
                    write_protected: flag(r.u8()?)?,
                    image: r.bytes(MAX_DISC)?,
                },
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
                _ => return Err(WireError::Malformed),
            })
        })
    }
}

fn change(r: &mut Reader<'_>) -> Result<Change, WireError> {
    Ok(match r.u8()? {
        0 => Change::Key(
            Key::from_number(r.u8()?).ok_or(WireError::Malformed)?,
            flag(r.u8()?)?,
        ),
        1 => Change::Break(flag(r.u8()?)?),
        2 => {
            let channel = r.u8()?;
            if channel >= ANALOGUE_CHANNELS {
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
                    | u8::from(ran.lights.motor) << 2);
                w.u8(u8::from(ran.drives[0]) | u8::from(ran.drives[1]) << 1);
                w.u8(u8::from(ran.loaded.discs[0])
                    | u8::from(ran.loaded.discs[1]) << 1
                    | u8::from(ran.loaded.tape) << 2
                    | u8::from(ran.loaded.recording) << 3);
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
                    w.u8(format_byte(written.format));
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
                w.u8(REFUSALS
                    .iter()
                    .position(|candidate| candidate == refusal)
                    .and_then(|at| u8::try_from(at).ok())
                    .unwrap_or(u8::MAX));
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
                    if lights >> 3 != 0 || drives >> 2 != 0 || loaded >> 4 != 0 {
                        return Err(WireError::Malformed);
                    }
                    Self::Ran(Ran {
                        cycles,
                        rows,
                        sound,
                        lights: Lights {
                            caps_lock: lights & 1 != 0,
                            shift_lock: lights & 2 != 0,
                            motor: lights & 4 != 0,
                        },
                        drives: [drives & 1 != 0, drives & 2 != 0],
                        loaded: Loaded {
                            discs: [loaded & 1 != 0, loaded & 2 != 0],
                            tape: loaded & 4 != 0,
                            recording: loaded & 8 != 0,
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
                        let format = format_of(r.u8()?)?;
                        let lost = if flag(r.u8()?)? {
                            let (side, track) = (r.u8()?, r.u8()?);
                            if usize::from(side) >= format.sides() || track > LAST_TRACK {
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

const fn format_byte(format: ImageFormat) -> u8 {
    match format {
        ImageFormat::Ssd => 0,
        ImageFormat::Dsd => 1,
    }
}

const fn format_of(byte: u8) -> Result<ImageFormat, WireError> {
    match byte {
        0 => Ok(ImageFormat::Ssd),
        1 => Ok(ImageFormat::Dsd),
        _ => Err(WireError::Malformed),
    }
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

//! The session between the application and its machine worker
//! (`plans/PCEMU.md` §3).
//!
//! The worker holds the whole machine and nothing else: no file, no device,
//! no capability. The front-end holds the media and the user's grants. They
//! speak this protocol over a [`tairix_sandbox`] duplex session and share one
//! region, the frame slots of [`layout`].
//!
//! * A frame crosses by naming the slot it was written into; the front-end
//!   copies it out and hands the slot back.
//! * Sound crosses in [`Event::Audio`] frames of 16-bit stereo, and the
//!   front-end reports how much its own buffer holds, which the worker steers
//!   its mix by.
//! * A sector crosses on request: the worker asks for a run with
//!   [`Event::Fetch`] or writes one with [`Event::Store`], and waits for the
//!   answer with the machine stopped, so emulated time never sees how long the
//!   front-end's storage took. At most one request is ever outstanding.
//! * A machine suspends whole: on [`Command::Suspend`] the worker sends its
//!   snapshot, compressed, in [`Event::Kept`] pieces and then
//!   [`Event::Suspended`]; one resumed asks for its snapshot back a piece at
//!   a time with [`Event::Recall`] before it is ready, and says with
//!   [`Event::Resumed`] whether it could take it back.
//! * Input is credited: the front-end sends at most [`INPUT_WINDOW`] credited
//!   commands beyond what the worker last reported applying
//!   ([`Event::Credit`]), so the commands a worker holds while it waits on a
//!   request are bounded however long the request takes.
//! * Text crosses in code page 437: [`Command::ReadText`] asks for the
//!   screen's, which [`Event::Text`] answers, and [`Command::Type`] hands
//!   over text to type within [`TYPE_WINDOW`], which [`Event::Typed`]
//!   reopens as the worker takes it.
//! * A hot block's bytes cross in [`Event::Compile`], at most
//!   [`COMPILE_WINDOW`] unanswered; [`Command::Compiled`] answers each with
//!   where its code was installed in the worker, or nothing, and
//!   [`Command::Interpret`] asks for no more.
//! * The debugger's requests cross in [`Command::Debug`] and its reports in
//!   [`Event::Debug`] ([`debug`]).
//!
//! Every decoder here is total: a malformed payload is a [`WireError`], and
//! the side that reads one ends the session.

pub mod config;
pub mod debug;
pub mod layout;

use crate::board::{ConfigError, Lights};
use crate::chips::keyboard::Scan;
use crate::cpu::jit::{MOST_BLOCK_BYTES, MOST_CODE_BYTES};
use crate::folder;
use crate::ports::game::Sticks;
use crate::storage::hdc::{AttachError, DiskType, LowLevel};
use crate::storage::{MediaError, SECTOR};
use crate::time::{CpuClock, Nanos};
use crate::video::{Unread, MOST_TEXT};
use tairix_abi::time::CivilTime;
use tairix_hash::HashSeed;
use tairix_reclaim::PressureBand;
use tairix_sandbox::proto::FRAME_HEADER_LEN;
use tairix_sandbox::wire::{Reader, Room, WireError, Writer, BYTES_PREFIX};

/// Credited commands the front-end may send beyond the worker's last
/// [`Event::Credit`].
pub const INPUT_WINDOW: u32 = 64;

/// Most input events a queue of them holds: the front-end's beyond the
/// window, the worker's waiting for the keyboard.
pub const QUEUE_MOST: usize = 256;

/// Of [`QUEUE_MOST`], the places kept for releases, so a key whose press
/// was queued is always released however full the queue.
pub const RELEASES_KEPT: usize = 128;

/// Keys whose press was dropped from a full queue, so that their release is
/// dropped too: a key the guest never saw go down never comes up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dropped([u64; 4]);

impl Dropped {
    fn bit(scan: Scan) -> (usize, u64) {
        let index = usize::from(scan.code & 0x7F) | if scan.extended { 0x80 } else { 0 };
        (index / 64, 1 << (index % 64))
    }

    /// `scan`'s press was dropped.
    pub fn press(&mut self, scan: Scan) {
        let (word, bit) = Self::bit(scan);
        self.0[word] |= bit;
    }

    /// Whether `scan`'s release is to be dropped with its press, which is
    /// then forgotten.
    pub fn release(&mut self, scan: Scan) -> bool {
        let (word, bit) = Self::bit(scan);
        let dropped = self.0[word] & bit != 0;
        self.0[word] &= !bit;
        dropped
    }

    /// Forget every dropped press: every key has come up.
    pub fn clear(&mut self) {
        self.0 = [0; 4];
    }
}

/// Whether a queue holding `len` input events takes a key event: a press
/// only below the places kept for releases, a release while any place is
/// left.
#[must_use]
pub const fn takes_key(len: usize, down: bool) -> bool {
    if down {
        len < QUEUE_MOST - RELEASES_KEPT
    } else {
        len < QUEUE_MOST
    }
}

/// Characters the front-end may give the worker to type beyond those
/// [`Event::Typed`] has accounted for.
pub const TYPE_WINDOW: usize = 1024;

/// Blocks the worker may have asked to be compiled with no answer yet.
pub const COMPILE_WINDOW: usize = 4;

/// Bytes a [`Command::Type`] takes beyond its text: its tag and its length.
pub const TYPE_OVERHEAD: usize = 1 + BYTES_PREFIX;

/// Whether [`Command::Type`] carries `code`: a character that acts as no
/// control but Tab and Enter, which `09h` and `0Dh` press.
#[must_use]
pub const fn typeable(code: u8) -> bool {
    matches!(code, 0x09 | 0x0D | 0x20..=0x7E | 0x80..=0xFF)
}

/// Most sectors one [`Event::Fetch`] asks for.
pub const MAX_RUN: u16 = 64;

/// Most stereo frames one [`Event::Audio`] carries.
pub const MAX_AUDIO_FRAMES: usize = 1024;

/// Bytes a stereo frame of sound takes: two 16-bit samples.
pub const AUDIO_FRAME: usize = 4;

/// Most media a machine names at once: two fixed disks, a diskette in each
/// of four drives, and one more going in as another comes out or being read
/// whole as an archive.
pub const MAX_MEDIA: usize = 7;

/// Most bytes of a floppy archive: the largest ImageDisk file of the largest
/// diskette, with room for its comment and headers.
pub const MAX_ARCHIVE: u32 = 4 << 20;

/// Most bytes of a machine's snapshot before it is compressed: the largest
/// memory, expanded memory and display cards' memories and frames a machine
/// can be built with, and room for its chips.
pub const MAX_SNAPSHOT: usize = 20 << 20;

/// Most bytes of a snapshot as it crosses and is kept: [`MAX_SNAPSHOT`]
/// compressed however badly.
#[must_use]
pub const fn most_kept() -> usize {
    tairix_compress::max_compressed_len(MAX_SNAPSHOT)
}

/// Most bytes of a snapshot one [`Event::Kept`] or [`Command::Recalled`]
/// carries: a fetch's worth.
pub const KEPT_PIECE: usize = MAX_RUN as usize * SECTOR;

/// The largest command: a [`Setup`] with two fixed disks whose every track
/// is marked bad, and a snapshot to resume.
const LARGEST_COMMAND: usize = 80 + config::CONFIG_BYTES + 2 * (16 + config::FIXED_BYTES);

const _: () = assert!(16 + KEPT_PIECE <= LARGEST_COMMAND && 16 + KEPT_PIECE <= LARGEST_EVENT);
// A tag before it.
const _: () = assert!(debug::MOST_SAID < LARGEST_EVENT);
const _: () = assert!(
    16 + TYPE_WINDOW <= LARGEST_COMMAND
        && 6 + MOST_TEXT.0 as usize * MOST_TEXT.1 as usize <= LARGEST_EVENT
);

/// The largest event: a format whose every track is marked bad.
const LARGEST_EVENT: usize = 16 + config::FIXED_BYTES;

/// The most a folder's request or answer takes: a write's bytes, or a read's,
/// with the fields beside them.
const FOLDER_MOST: usize = 32 + folder::IO_MOST;

const _: () = assert!(FOLDER_MOST <= LARGEST_EVENT && FOLDER_MOST <= LARGEST_COMMAND);

/// The most a block asked to be compiled takes: its tag, number, place and
/// bytes.
const COMPILE_MOST: usize = 9 + BYTES_PREFIX + MOST_BLOCK_BYTES;

const _: () = assert!(COMPILE_MOST <= LARGEST_EVENT);

/// The most an answer to a block takes, framed.
const COMPILED_MOST: usize = FRAME_HEADER_LEN + 22;

/// Bytes of framed traffic the worker takes in at once: its largest command.
pub const WORKER_INBOUND: usize = FRAME_HEADER_LEN + LARGEST_COMMAND;

/// Bytes of framed traffic the front-end takes in at once: two of the
/// worker's largest events, so the worker can write one while the front-end
/// takes the other.
pub const FRONT_INBOUND: usize = 2 * (FRAME_HEADER_LEN + LARGEST_EVENT);

/// Bytes of framed traffic the front-end queues for the worker: its largest
/// command beside a full window of input, a window of text to type, an
/// answer, and the answers to a window of blocks.
pub const FRONT_OUTBOUND: usize = WORKER_INBOUND
    + CREDITED_MOST * INPUT_WINDOW as usize
    + TYPED_MOST
    + FETCHED_MOST
    + COMPILED_MOST * COMPILE_WINDOW;

/// The most a window of text to type takes, framed: the two pieces it is
/// sent in at most.
const TYPED_MOST: usize = 2 * (FRAME_HEADER_LEN + TYPE_OVERHEAD) + TYPE_WINDOW;

/// The most any credited command but [`Command::Start`] takes, framed.
pub const CREDITED_MOST: usize = 64;

/// The most an answer to a fetch takes, framed.
const FETCHED_MOST: usize = FRAME_HEADER_LEN + 16 + MAX_RUN as usize * SECTOR;

/// How a machine is started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// The machine.
    pub config: crate::board::BoardConfig,
    /// Its fixed disks, which its firmware is built for.
    pub fixed: [Option<FixedDisk>; 2],
    /// Frames of sound the front-end's own buffer is steered to hold.
    pub audio_target: u32,
    /// The most bytes the worker may map: what it sizes its cache within.
    pub room: u64,
    /// The key its cache's keys are hashed with, drawn by the front-end, so
    /// no guest can choose which of its sectors collide.
    pub seed: [u8; HashSeed::LEN],
    /// The memory pressure the machine stands at.
    pub band: PressureBand,
    /// The time the clock card is set to, if the host knows it.
    pub clock: Option<CivilTime>,
    /// The snapshot to resume, if the machine was suspended.
    pub resume: Option<Resume>,
}

/// A suspended machine's snapshot, which the worker recalls before it is
/// ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resume {
    /// Its bytes, as the worker kept them: at most [`most_kept`].
    pub len: u32,
    /// How long the machine was suspended, by the host's calendar: what
    /// its time of day moves on by.
    pub away: Nanos,
}

/// A fixed disk the machine starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedDisk {
    /// The front-end's name for its image.
    pub medium: u16,
    /// The image's sectors.
    pub sectors: u64,
    /// Whether the image refuses writes.
    pub read_only: bool,
    /// The drive.
    pub kind: DiskType,
    /// Its low-level format, which the image does not record.
    pub format: LowLevel,
}

/// How a diskette's image is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// A raw image: its sectors in order.
    Sectors {
        /// How many.
        count: u64,
        /// Whether it refuses writes.
        read_only: bool,
    },
    /// An ImageDisk archive of this many bytes, which the worker reads whole
    /// and parses itself. Archives are read-only.
    Archive {
        /// Its length.
        bytes: u32,
    },
}

impl Form {
    /// The 512-byte units a fetch of the medium counts in: an archive's last
    /// is padded.
    #[must_use]
    pub const fn units(self) -> u64 {
        match self {
            Self::Sectors { count, .. } => count,
            Self::Archive { bytes } => (bytes as u64).div_ceil(SECTOR as u64),
        }
    }

    /// Whether the medium refuses writes.
    #[must_use]
    pub const fn read_only(self) -> bool {
        match self {
            Self::Sectors { read_only, .. } => read_only,
            Self::Archive { .. } => true,
        }
    }
}

/// What the front-end tells the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "a command is decoded onto the stack one at a time and never kept in bulk; the worker keeps waiting commands as their bytes"
)]
pub enum Command<'a> {
    /// Build the machine. The first command, and only the first.
    Start(Setup),
    /// Put the diskette `medium` names in drive `drive`.
    Insert {
        /// The drive.
        drive: u8,
        /// The front-end's name for the image, above every name before it.
        medium: u16,
        /// Its layout.
        form: Form,
    },
    /// Take the diskette out of drive `drive`.
    Eject {
        /// The drive.
        drive: u8,
    },
    /// Run the machine, paced to the host's clock.
    Run,
    /// Stop running it.
    Hold,
    /// Power it off and on, its media left in their drives.
    Reset,
    /// Run the processor at this clock from now on: the turbo switch.
    Speed(CpuClock),
    /// A key goes down or comes up.
    Key {
        /// The key.
        scan: Scan,
        /// Whether it goes down.
        down: bool,
    },
    /// Every held key comes up: the window lost the keyboard.
    ReleaseKeys,
    /// The serial mouse moves, with these buttons held.
    Mouse {
        /// Rightward motion.
        dx: i32,
        /// Downward motion.
        dy: i32,
        /// The buttons held.
        buttons: u8,
    },
    /// The host's pointer stands at this dot of adapter `adapter`'s frame,
    /// with these buttons held: the mouse's own driver follows it.
    Pointer {
        /// The adapter whose frame it is over.
        adapter: u8,
        /// Dots from the frame's left.
        x: u16,
        /// Lines from its top.
        y: u16,
        /// The buttons held.
        buttons: u8,
    },
    /// The joysticks stand so.
    Sticks(Sticks),
    /// Set the clock card.
    Clock(CivilTime),
    /// The answer to fetch `request`: its sectors, or why they could not be
    /// read.
    Fetched {
        /// The request it answers.
        request: u32,
        /// The sectors.
        answer: Result<&'a [u8], MediaError>,
    },
    /// The answer to store `request`.
    Stored {
        /// The request it answers.
        request: u32,
        /// Whether the sector is written.
        answer: Result<(), MediaError>,
    },
    /// Frame slot `slot` is the worker's again.
    Released {
        /// The slot.
        slot: u8,
    },
    /// The front-end's sound buffer holds this many frames.
    Fill {
        /// Frames held.
        frames: u32,
    },
    /// The machine's memory pressure is this band now.
    Pressure(PressureBand),
    /// Stop the machine and keep it whole: its snapshot comes back in
    /// [`Event::Kept`] pieces.
    Suspend,
    /// The answer to recall `request`: the snapshot's bytes it asked for.
    Recalled {
        /// The request it answers.
        request: u32,
        /// The bytes.
        bytes: &'a [u8],
    },
    /// Read the text adapter 0 shows; [`Event::Text`] answers.
    ReadText,
    /// Type these characters on the keyboard, each once the guest has read
    /// the last: code page 437 bytes [`typeable`] admits.
    Type(&'a [u8]),
    /// Leave what waits to be typed untyped; the character under way is
    /// finished.
    StopTyping,
    /// Share the folder the front-end holds as share `slot`, labelled
    /// `label`; [`Event::Shared`] answers which drive it is.
    Share {
        /// The share, below [`folder::SHARES`].
        slot: u8,
        /// The label its root carries.
        label: folder::DosName,
        /// Whether it may be changed.
        writable: bool,
    },
    /// Stop sharing share `slot`; its drive goes, and the files open on it
    /// are lost to the machine.
    Unshare {
        /// The share.
        slot: u8,
    },
    /// The answer to folder request `request`.
    Foldered {
        /// The request it answers.
        request: u32,
        /// What the folder answered.
        answer: Result<folder::Reply<'a>, folder::Refusal>,
    },
    /// The answer to block request `request`: where its code was installed
    /// in the worker, or none.
    Compiled {
        /// The request it answers.
        request: u32,
        /// The code.
        code: Option<Installed>,
    },
    /// Ask for no more blocks to be compiled.
    Interpret,
    /// Ask the debugger.
    Debug(debug::Debug),
}

/// A block's code as the front-end installed it in the worker: `len` bytes,
/// at most [`MOST_CODE_BYTES`], at `address`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Where.
    pub address: u64,
    /// How many bytes.
    pub len: u64,
}

impl Command<'_> {
    /// Whether the command counts against [`INPUT_WINDOW`]. Answers and
    /// releases are bounded by what the worker asked for and holds, text to
    /// type by [`TYPE_WINDOW`], and the two readings replace the last of
    /// their kind, so only the rest are counted.
    #[must_use]
    pub const fn credited(&self) -> bool {
        !matches!(
            self,
            Self::Fetched { .. }
                | Self::Stored { .. }
                | Self::Recalled { .. }
                | Self::Foldered { .. }
                | Self::Released { .. }
                | Self::Fill { .. }
                | Self::Pressure(_)
                | Self::Type(_)
                | Self::Compiled { .. }
                | Self::Interpret
        )
    }
}

/// Why a machine could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The board refused its configuration.
    Board(ConfigError),
    /// The firmware could not be built for it: a defect of the emulator's.
    Firmware,
    /// Fixed disk `n` could not be attached.
    Fixed(u8, AttachError),
    /// The shared region is missing or smaller than the machine's layout.
    Share,
    /// The worker could not have the memory its own state needs.
    Memory,
}

/// Why a diskette could not be inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertRefusal {
    /// The machine has no such drive.
    NoDrive,
    /// The image's name is not above every name before it.
    Medium,
    /// A raw image of no diskette's size.
    Format,
    /// An archive that could not be read or parsed.
    Archive,
}

/// Why a machine's snapshot could not be kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unkept {
    /// The worker could not have the memory to take it.
    Memory,
    /// It is larger than [`MAX_SNAPSHOT`]: a defect of the emulator's.
    Size,
}

/// A text screen as it crosses: `rows` rows of `columns` code page 437
/// codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Text<'a> {
    /// Characters a row.
    pub columns: u16,
    /// Rows.
    pub rows: u16,
    /// Each row's codes, the top row first.
    pub codes: &'a [u8],
}

impl<'a> Text<'a> {
    /// Each row, the top first, its trailing blanks dropped.
    pub fn lines(&self) -> impl DoubleEndedIterator<Item = &'a [u8]> {
        self.codes
            .chunks(usize::from(self.columns).max(1))
            .map(<[u8]>::trim_ascii_end)
    }

    /// The last row holding anything: where a prompt stands.
    #[must_use]
    pub fn last_line(&self) -> &'a [u8] {
        self.lines()
            .rev()
            .find(|line| !line.is_empty())
            .unwrap_or_default()
    }
}

/// What the worker tells the front-end.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "an event is decoded onto the stack one at a time and never kept in bulk"
)]
pub enum Event<'a> {
    /// The machine is built and waits to be run.
    Ready,
    /// The machine could not be built; the worker ends.
    Refused(Refusal),
    /// A diskette was inserted, or why it was not.
    Inserted {
        /// The drive.
        drive: u8,
        /// The image.
        medium: u16,
        /// Whether it went in.
        outcome: Result<(), InsertRefusal>,
    },
    /// A diskette left its drive; the worker asks nothing more of it.
    Ejected {
        /// The drive.
        drive: u8,
        /// The image.
        medium: u16,
    },
    /// A frame is in slot `slot`.
    Frame {
        /// The slot.
        slot: u8,
        /// Its dots.
        width: u16,
        /// Its lines.
        height: u16,
    },
    /// Adapter `adapter`'s guest programmed a frame its monitor cannot show.
    OutOfRange {
        /// The adapter, counted in order among those fitted.
        adapter: u8,
    },
    /// Sound: interleaved 16-bit little-endian stereo frames.
    Audio(&'a [u8]),
    /// Read `count` sectors of `medium` from `first`.
    Fetch {
        /// The request's number, which its answer names.
        request: u32,
        /// The image.
        medium: u16,
        /// The first sector.
        first: u64,
        /// How many.
        count: u16,
    },
    /// Write sector `sector` of `medium`.
    Store {
        /// The request's number, which its answer names.
        request: u32,
        /// The image.
        medium: u16,
        /// The sector.
        sector: u64,
        /// What it holds.
        data: &'a [u8; SECTOR],
    },
    /// Fixed disk `drive` was low-level formatted: what the front-end keeps
    /// beside its image.
    Formatted {
        /// The drive.
        drive: u8,
        /// The format.
        format: LowLevel,
    },
    /// The worker has applied this many credited commands since it started,
    /// counted modulo 2³².
    Credit {
        /// Commands applied.
        applied: u32,
    },
    /// Whether the guest now reads its mouse through the emulator's own
    /// driver, which follows the host's pointer; otherwise the guest reads
    /// the mouse itself and wants its motion.
    Pointing {
        /// Whether the driver follows the host's pointer.
        follows: bool,
    },
    /// The guest set its clock card, which now reads this.
    ClockSet(CivilTime),
    /// What the drives' lamps show now.
    Lights(Lights),
    /// Whether the machine took back the snapshot it was started to
    /// resume, or was built afresh; it comes before [`Event::Ready`].
    Resumed(bool),
    /// Send `len` bytes of the snapshot to resume from `offset`.
    Recall {
        /// The request's number, which its answer names.
        request: u32,
        /// Where in the snapshot.
        offset: u32,
        /// How many bytes: at most [`KEPT_PIECE`].
        len: u32,
    },
    /// The next piece of the snapshot the machine was suspended to.
    Kept(&'a [u8]),
    /// The snapshot is whole, this many bytes, or why it was not kept; the
    /// machine stays stopped.
    Suspended(Result<u32, Unkept>),
    /// The text adapter 0 shows, or why it was not read.
    Text(Result<Text<'a>, Unread>),
    /// This many more of the characters the front-end gave to type were
    /// begun, or left untyped.
    Typed(u32),
    /// Whether adapter 0 now shows text, which [`Command::ReadText`] reads;
    /// sent when it changes.
    TextShown(bool),
    /// Compile the block at `cs:ip`, whose bytes these are: block request
    /// `request`, which [`Command::Compiled`] answers.
    Compile {
        /// The request's number, which its answer names.
        request: u32,
        /// Its code segment.
        cs: u16,
        /// Its first byte's offset.
        ip: u16,
        /// Its bytes from there: at most [`MOST_BLOCK_BYTES`].
        bytes: &'a [u8],
    },
    /// A translated block misbehaved: the machine runs interpreted from now
    /// on, and asks for no more blocks.
    Interpreting,
    /// Ask share `slot` `op`, waiting for the answer.
    Folder {
        /// The request's number, which its answer names.
        request: u32,
        /// The share.
        slot: u8,
        /// What is asked.
        op: folder::Op<'a>,
    },
    /// Share `slot` is drive `drive`, from `0` for `A:`, or took none: no
    /// drive letter was free, or the machine runs no DOS of the emulator's.
    Shared {
        /// The share.
        slot: u8,
        /// Its drive.
        drive: Option<u8>,
    },
    /// The debugger reports.
    Debug(debug::Said<'a>),
}

/// The tag a command or event begins with.
mod tag {
    pub const START: u8 = 0x01;
    pub const INSERT: u8 = 0x02;
    pub const EJECT: u8 = 0x03;
    pub const RUN: u8 = 0x04;
    pub const HOLD: u8 = 0x05;
    pub const RESET: u8 = 0x06;
    pub const SPEED: u8 = 0x07;
    pub const KEY: u8 = 0x08;
    pub const RELEASE_KEYS: u8 = 0x09;
    pub const MOUSE: u8 = 0x0A;
    pub const STICKS: u8 = 0x0B;
    pub const CLOCK: u8 = 0x0C;
    pub const FETCHED: u8 = 0x0D;
    pub const STORED: u8 = 0x0E;
    pub const RELEASED: u8 = 0x0F;
    pub const FILL: u8 = 0x10;
    pub const PRESSURE: u8 = 0x11;
    pub const POINTER: u8 = 0x12;
    pub const SUSPEND: u8 = 0x13;
    pub const RECALLED: u8 = 0x14;
    pub const READ_TEXT: u8 = 0x15;
    pub const TYPE: u8 = 0x16;
    pub const STOP_TYPING: u8 = 0x17;
    pub const SHARE: u8 = 0x18;
    pub const UNSHARE: u8 = 0x19;
    pub const FOLDERED: u8 = 0x1A;
    pub const COMPILED: u8 = 0x1B;
    pub const INTERPRET: u8 = 0x1C;
    pub const DEBUG: u8 = 0x1D;

    pub const READY: u8 = 0x81;
    pub const REFUSED: u8 = 0x82;
    pub const INSERTED: u8 = 0x83;
    pub const EJECTED: u8 = 0x84;
    pub const FRAME: u8 = 0x85;
    pub const OUT_OF_RANGE: u8 = 0x86;
    pub const AUDIO: u8 = 0x87;
    pub const FETCH: u8 = 0x88;
    pub const STORE: u8 = 0x89;
    pub const FORMATTED: u8 = 0x8A;
    pub const CREDIT: u8 = 0x8B;
    pub const POINTING: u8 = 0x8C;
    pub const CLOCK_SET: u8 = 0x8D;
    pub const LIGHTS: u8 = 0x8E;
    pub const RESUMED: u8 = 0x8F;
    pub const RECALL: u8 = 0x90;
    pub const KEPT: u8 = 0x91;
    pub const SUSPENDED: u8 = 0x92;
    pub const TEXT: u8 = 0x93;
    pub const TYPED: u8 = 0x94;
    pub const TEXT_SHOWN: u8 = 0x95;
    pub const FOLDER: u8 = 0x96;
    pub const SHARED: u8 = 0x97;
    pub const COMPILE: u8 = 0x98;
    pub const SAID: u8 = 0x99;
    pub const INTERPRETING: u8 = 0x9A;
}

const MEDIA_ERRORS: [MediaError; 3] = [
    MediaError::OutOfRange,
    MediaError::WriteProtected,
    MediaError::Failed,
];
const CONFIG_ERRORS: [ConfigError; 7] = [
    ConfigError::Memory,
    ConfigError::OutOfMemory,
    ConfigError::VideoConflict,
    ConfigError::StreamRate,
    ConfigError::SoundConflict,
    ConfigError::PortConflict,
    ConfigError::Expanded,
];
const ATTACH_ERRORS: [AttachError; 4] = [
    AttachError::NoSuchDrive,
    AttachError::Geometry,
    AttachError::Size,
    AttachError::BadTrack,
];
const INSERT_REFUSALS: [InsertRefusal; 4] = [
    InsertRefusal::NoDrive,
    InsertRefusal::Medium,
    InsertRefusal::Format,
    InsertRefusal::Archive,
];
const UNKEPT: [Unkept; 2] = [Unkept::Memory, Unkept::Size];
const UNREAD: [Unread; 3] = [Unread::NoText, Unread::TooLarge, Unread::Memory];

/// The value numbered `n` of `all`.
pub(crate) fn nth<T: Copy>(all: &[T], n: u8) -> Result<T, WireError> {
    all.get(usize::from(n)).copied().ok_or(WireError::Malformed)
}

/// `value`'s number among `all`, which lists every value of its type.
pub(crate) fn number<T: PartialEq>(all: &[T], value: &T) -> u8 {
    all.iter()
        .position(|candidate| candidate == value)
        .and_then(|n| u8::try_from(n).ok())
        .unwrap_or(u8::MAX)
}

/// Where a block's code was installed, or none: a length of no code, or more
/// than any block compiles to, refused.
fn installed(r: &mut Reader<'_>) -> Result<Option<Installed>, WireError> {
    if !flag(r)? {
        return Ok(None);
    }
    let (address, len) = (r.u64()?, r.u64()?);
    if (1..=MOST_CODE_BYTES as u64).contains(&len) {
        Ok(Some(Installed { address, len }))
    } else {
        Err(WireError::Malformed)
    }
}

/// A block's bytes: some, and no more than any block holds.
fn block_bytes<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], WireError> {
    let bytes = r.bytes(MOST_BLOCK_BYTES)?;
    if bytes.is_empty() {
        Err(WireError::Malformed)
    } else {
        Ok(bytes)
    }
}

/// A piece of a snapshot: the rest of the payload, some and no more than a
/// piece holds.
fn kept_piece<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], WireError> {
    let bytes = r.take(r.remaining())?;
    if bytes.is_empty() || bytes.len() > KEPT_PIECE {
        return Err(WireError::Malformed);
    }
    Ok(bytes)
}

/// A share's slot: below [`folder::SHARES`].
fn share_slot(r: &mut Reader<'_>) -> Result<u8, WireError> {
    let slot = r.u8()?;
    if usize::from(slot) < folder::SHARES {
        Ok(slot)
    } else {
        Err(WireError::Malformed)
    }
}

/// A flag byte: `0` or `1` and nothing else.
pub(crate) fn flag(r: &mut Reader<'_>) -> Result<bool, WireError> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

fn put_media_result<R: Room>(w: &mut Writer<R>, result: Result<(), MediaError>) {
    w.u8(match result {
        Ok(()) => 0,
        Err(error) => number(&MEDIA_ERRORS, &error).saturating_add(1),
    });
}

fn media_result(r: &mut Reader<'_>) -> Result<Result<(), MediaError>, WireError> {
    match r.u8()? {
        0 => Ok(Ok(())),
        n => nth(&MEDIA_ERRORS, n - 1).map(Err),
    }
}

fn put_civil<R: Room>(w: &mut Writer<R>, time: &CivilTime) {
    w.u64(time.year.cast_unsigned());
    for part in [time.month, time.day, time.hour, time.minute, time.second] {
        w.u8(u8::try_from(part).unwrap_or(u8::MAX));
    }
}

fn civil(r: &mut Reader<'_>) -> Result<CivilTime, WireError> {
    let year = r.u64()?.cast_signed();
    let mut parts = [0u32; 5];
    for part in &mut parts {
        *part = u32::from(r.u8()?);
    }
    let [month, day, hour, minute, second] = parts;
    let valid = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60;
    valid
        .then_some(CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
        .ok_or(WireError::Malformed)
}

fn band(r: &mut Reader<'_>) -> Result<PressureBand, WireError> {
    PressureBand::from_known_depth(r.u8()?).ok_or(WireError::Malformed)
}

fn put_form<R: Room>(w: &mut Writer<R>, form: Form) {
    match form {
        Form::Sectors { count, read_only } => {
            w.u8(0);
            w.u64(count);
            w.u8(u8::from(read_only));
        }
        Form::Archive { bytes } => {
            w.u8(1);
            w.u32(bytes);
        }
    }
}

fn put_sticks<R: Room>(w: &mut Writer<R>, sticks: &Sticks) {
    for axis in sticks.axes {
        match axis {
            None => w.u8(0),
            Some(at) => {
                w.u8(1);
                w.u16(at);
            }
        }
    }
    w.u8(sticks.buttons);
}

fn put_refusal<R: Room>(w: &mut Writer<R>, refusal: Refusal) {
    match refusal {
        Refusal::Board(error) => w.raw(&[0, number(&CONFIG_ERRORS, &error), 0]),
        Refusal::Firmware => w.raw(&[1, 0, 0]),
        Refusal::Fixed(n, error) => w.raw(&[2, n, number(&ATTACH_ERRORS, &error)]),
        Refusal::Share => w.raw(&[3, 0, 0]),
        Refusal::Memory => w.raw(&[4, 0, 0]),
    }
}

fn put_suspended<R: Room>(w: &mut Writer<R>, outcome: Result<u32, Unkept>) {
    match outcome {
        Ok(len) => {
            w.u8(0);
            w.u32(len);
        }
        Err(why) => w.u8(number(&UNKEPT, &why).saturating_add(1)),
    }
}

fn put_text<R: Room>(w: &mut Writer<R>, outcome: Result<&Text<'_>, Unread>) {
    w.u8(tag::TEXT);
    match outcome {
        Ok(text) => {
            w.u8(0);
            w.u16(text.columns);
            w.u16(text.rows);
            w.raw(text.codes);
        }
        Err(why) => w.u8(number(&UNREAD, &why).saturating_add(1)),
    }
}

fn text<'a>(r: &mut Reader<'a>) -> Result<Result<Text<'a>, Unread>, WireError> {
    let n = r.u8()?;
    if n != 0 {
        return nth(&UNREAD, n - 1).map(Err);
    }
    let (columns, rows) = (r.u16()?, r.u16()?);
    let (most_columns, most_rows) = MOST_TEXT;
    if !(1..=most_columns).contains(&columns) || !(1..=most_rows).contains(&rows) {
        return Err(WireError::Malformed);
    }
    let codes = r.take(usize::from(columns) * usize::from(rows))?;
    Ok(Ok(Text {
        columns,
        rows,
        codes,
    }))
}

/// A request's tag, number, medium and first sector.
fn put_request<R: Room>(w: &mut Writer<R>, tag: u8, request: u32, medium: u16, sector: u64) {
    w.u8(tag);
    w.u32(request);
    w.u16(medium);
    w.u64(sector);
}

fn put_scan<R: Room>(w: &mut Writer<R>, scan: Scan) {
    w.u8(scan.code | if scan.extended { 0x80 } else { 0 });
}

fn scan(r: &mut Reader<'_>) -> Result<Scan, WireError> {
    let byte = r.u8()?;
    Ok(if byte & 0x80 == 0 {
        Scan::plain(byte)
    } else {
        Scan::extended(byte)
    })
}

impl Setup {
    fn encode<R: Room>(&self, w: &mut Writer<R>) {
        config::put_config(w, &self.config);
        for disk in &self.fixed {
            match disk {
                None => w.u8(0),
                Some(disk) => {
                    w.u8(1);
                    w.u16(disk.medium);
                    w.u64(disk.sectors);
                    w.u8(u8::from(disk.read_only));
                    config::put_fixed(w, &disk.kind, &disk.format);
                }
            }
        }
        w.u32(self.audio_target);
        w.u64(self.room);
        w.raw(&self.seed);
        w.u8(self.band.depth());
        match &self.clock {
            None => w.u8(0),
            Some(time) => {
                w.u8(1);
                put_civil(w, time);
            }
        }
        match &self.resume {
            None => w.u8(0),
            Some(resume) => {
                w.u8(1);
                w.u32(resume.len);
                w.u64(resume.away);
            }
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        let config = config::config(r)?;
        let mut fixed = [None, None];
        for disk in &mut fixed {
            if flag(r)? {
                let (medium, sectors, read_only) = (r.u16()?, r.u64()?, flag(r)?);
                let (kind, format) = config::fixed(r)?;
                *disk = Some(FixedDisk {
                    medium,
                    sectors,
                    read_only,
                    kind,
                    format,
                });
            }
        }
        let audio_target = r.u32()?;
        let room = r.u64()?;
        let mut seed = [0; HashSeed::LEN];
        seed.copy_from_slice(r.take(HashSeed::LEN)?);
        let band = band(r)?;
        let clock = if flag(r)? { Some(civil(r)?) } else { None };
        let resume = if flag(r)? {
            let (len, away) = (r.u32()?, r.u64()?);
            if len == 0 || usize::try_from(len).map_or(true, |len| len > most_kept()) {
                return Err(WireError::Malformed);
            }
            Some(Resume { len, away })
        } else {
            None
        };
        Ok(Self {
            config,
            fixed,
            audio_target,
            room,
            seed,
            band,
            clock,
            resume,
        })
    }
}

impl Command<'_> {
    /// Append the command to `w`.
    pub fn encode<R: Room>(&self, w: &mut Writer<R>) {
        match self {
            Self::Start(setup) => {
                w.u8(tag::START);
                setup.encode(w);
            }
            Self::Insert {
                drive,
                medium,
                form,
            } => {
                w.u8(tag::INSERT);
                w.u8(*drive);
                w.u16(*medium);
                put_form(w, *form);
            }
            Self::Eject { drive } => w.raw(&[tag::EJECT, *drive]),
            Self::Run => w.u8(tag::RUN),
            Self::Hold => w.u8(tag::HOLD),
            Self::Reset => w.u8(tag::RESET),
            Self::Speed(rate) => {
                w.u8(tag::SPEED);
                config::put_cpu_clock(w, *rate);
            }
            Self::Key { scan, down } => {
                w.u8(tag::KEY);
                put_scan(w, *scan);
                w.u8(u8::from(*down));
            }
            Self::ReleaseKeys => w.u8(tag::RELEASE_KEYS),
            Self::Mouse { dx, dy, buttons } => {
                w.u8(tag::MOUSE);
                w.i32(*dx);
                w.i32(*dy);
                w.u8(*buttons);
            }
            Self::Pointer {
                adapter,
                x,
                y,
                buttons,
            } => {
                w.u8(tag::POINTER);
                w.u8(*adapter);
                w.u16(*x);
                w.u16(*y);
                w.u8(*buttons);
            }
            Self::Sticks(sticks) => {
                w.u8(tag::STICKS);
                put_sticks(w, sticks);
            }
            Self::Clock(time) => {
                w.u8(tag::CLOCK);
                put_civil(w, time);
            }
            Self::Fetched { request, answer } => {
                w.u8(tag::FETCHED);
                w.u32(*request);
                match answer {
                    Ok(data) => {
                        w.u8(0);
                        w.bytes(data);
                    }
                    Err(error) => put_media_result(w, Err(*error)),
                }
            }
            Self::Stored { request, answer } => {
                w.u8(tag::STORED);
                w.u32(*request);
                put_media_result(w, *answer);
            }
            Self::Released { slot } => w.raw(&[tag::RELEASED, *slot]),
            Self::Fill { frames } => {
                w.u8(tag::FILL);
                w.u32(*frames);
            }
            Self::Pressure(band) => w.raw(&[tag::PRESSURE, band.depth()]),
            Self::Suspend => w.u8(tag::SUSPEND),
            Self::Recalled { request, bytes } => {
                w.u8(tag::RECALLED);
                w.u32(*request);
                w.bytes(bytes);
            }
            Self::ReadText => w.u8(tag::READ_TEXT),
            Self::Type(codes) => {
                w.u8(tag::TYPE);
                w.bytes(codes);
            }
            Self::StopTyping => w.u8(tag::STOP_TYPING),
            Self::Share { .. } | Self::Unshare { .. } | Self::Foldered { .. } => {
                self.encode_sharing(w);
            }
            Self::Compiled { .. } | Self::Interpret => self.encode_compiling(w),
            Self::Debug(debug) => debug.encode(w),
        }
    }

    /// Append a command of the blocks compiled to `w`; nothing for another.
    fn encode_compiling<R: Room>(&self, w: &mut Writer<R>) {
        match self {
            Self::Compiled { request, code } => {
                w.u8(tag::COMPILED);
                w.u32(*request);
                match code {
                    Some(code) => {
                        w.u8(1);
                        w.u64(code.address);
                        w.u64(code.len);
                    }
                    None => w.u8(0),
                }
            }
            Self::Interpret => w.u8(tag::INTERPRET),
            _ => {}
        }
    }

    /// Append a command of the shared folders to `w`; nothing for another.
    fn encode_sharing<R: Room>(&self, w: &mut Writer<R>) {
        match self {
            Self::Share {
                slot,
                label,
                writable,
            } => {
                w.raw(&[tag::SHARE, *slot]);
                w.raw(label.bytes());
                w.u8(u8::from(*writable));
            }
            Self::Unshare { slot } => w.raw(&[tag::UNSHARE, *slot]),
            Self::Foldered { request, answer } => {
                w.u8(tag::FOLDERED);
                w.u32(*request);
                folder::encode_answer(answer, w);
            }
            _ => {}
        }
    }
}

impl<'a> Command<'a> {
    /// The command `payload` holds.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a payload that is not a whole command.
    pub fn decode(payload: &'a [u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        r.whole(|r| {
            Ok(match r.u8()? {
                tag::START => Self::Start(Setup::decode(r)?),
                tag::INSERT => {
                    let (drive, medium) = (r.u8()?, r.u16()?);
                    let form = match r.u8()? {
                        0 => Form::Sectors {
                            count: r.u64()?,
                            read_only: flag(r)?,
                        },
                        1 => {
                            let bytes = r.u32()?;
                            if bytes == 0 || bytes > MAX_ARCHIVE {
                                return Err(WireError::Malformed);
                            }
                            Form::Archive { bytes }
                        }
                        _ => return Err(WireError::Malformed),
                    };
                    Self::Insert {
                        drive,
                        medium,
                        form,
                    }
                }
                tag::EJECT => Self::Eject { drive: r.u8()? },
                tag::RUN => Self::Run,
                tag::HOLD => Self::Hold,
                tag::RESET => Self::Reset,
                tag::SPEED => Self::Speed(config::cpu_clock(r)?),
                tag::KEY => Self::Key {
                    scan: scan(r)?,
                    down: flag(r)?,
                },
                tag::RELEASE_KEYS => Self::ReleaseKeys,
                tag::MOUSE => Self::Mouse {
                    dx: r.i32()?,
                    dy: r.i32()?,
                    buttons: r.u8()?,
                },
                tag::POINTER => Self::Pointer {
                    adapter: r.u8()?,
                    x: r.u16()?,
                    y: r.u16()?,
                    buttons: r.u8()?,
                },
                tag::STICKS => {
                    let mut axes = [None; 4];
                    for axis in &mut axes {
                        *axis = if flag(r)? { Some(r.u16()?) } else { None };
                    }
                    Self::Sticks(Sticks {
                        axes,
                        buttons: r.u8()?,
                    })
                }
                tag::CLOCK => Self::Clock(civil(r)?),
                tag::FETCHED => {
                    let request = r.u32()?;
                    let answer = match r.u8()? {
                        0 => Ok(r.bytes(usize::from(MAX_RUN) * SECTOR)?),
                        n => Err(nth(&MEDIA_ERRORS, n - 1)?),
                    };
                    Self::Fetched { request, answer }
                }
                tag::STORED => Self::Stored {
                    request: r.u32()?,
                    answer: media_result(r)?,
                },
                tag::RELEASED => Self::Released { slot: r.u8()? },
                tag::FILL => Self::Fill { frames: r.u32()? },
                tag::PRESSURE => Self::Pressure(band(r)?),
                tag::SUSPEND => Self::Suspend,
                tag::RECALLED => Self::Recalled {
                    request: r.u32()?,
                    bytes: r.bytes(KEPT_PIECE)?,
                },
                tag::READ_TEXT => Self::ReadText,
                tag::TYPE => {
                    let codes = r.bytes(TYPE_WINDOW)?;
                    if codes.is_empty() || !codes.iter().all(|&code| typeable(code)) {
                        return Err(WireError::Malformed);
                    }
                    Self::Type(codes)
                }
                tag::STOP_TYPING => Self::StopTyping,
                found @ (tag::SHARE | tag::UNSHARE | tag::FOLDERED) => {
                    Self::decode_sharing(found, r)?
                }
                tag::COMPILED => Self::Compiled {
                    request: r.u32()?,
                    code: installed(r)?,
                },
                tag::INTERPRET => Self::Interpret,
                tag::DEBUG => Self::Debug(debug::Debug::decode(r)?),
                _ => return Err(WireError::Malformed),
            })
        })
    }

    /// The command of the shared folders tagged `found`, its fields read
    /// from `r`.
    fn decode_sharing(found: u8, r: &mut Reader<'a>) -> Result<Self, WireError> {
        Ok(match found {
            tag::SHARE => Self::Share {
                slot: share_slot(r)?,
                label: folder::DosName::decode(r, false)?,
                writable: flag(r)?,
            },
            tag::UNSHARE => Self::Unshare {
                slot: share_slot(r)?,
            },
            tag::FOLDERED => Self::Foldered {
                request: r.u32()?,
                answer: folder::decode_answer(r)?,
            },
            _ => return Err(WireError::Malformed),
        })
    }
}

impl Event<'_> {
    /// Append the event to `w`.
    pub fn encode<R: Room>(&self, w: &mut Writer<R>) {
        match self {
            Self::Ready => w.u8(tag::READY),
            Self::Refused(refusal) => {
                w.u8(tag::REFUSED);
                put_refusal(w, *refusal);
            }
            Self::Inserted {
                drive,
                medium,
                outcome,
            } => {
                w.u8(tag::INSERTED);
                w.u8(*drive);
                w.u16(*medium);
                w.u8(match outcome {
                    Ok(()) => 0,
                    Err(refusal) => number(&INSERT_REFUSALS, refusal).saturating_add(1),
                });
            }
            Self::Ejected { drive, medium } => {
                w.u8(tag::EJECTED);
                w.u8(*drive);
                w.u16(*medium);
            }
            Self::Frame {
                slot,
                width,
                height,
            } => {
                w.u8(tag::FRAME);
                w.u8(*slot);
                w.u16(*width);
                w.u16(*height);
            }
            Self::OutOfRange { adapter } => w.raw(&[tag::OUT_OF_RANGE, *adapter]),
            Self::Audio(frames) => {
                w.u8(tag::AUDIO);
                w.raw(frames);
            }
            Self::Fetch {
                request,
                medium,
                first,
                count,
            } => {
                put_request(w, tag::FETCH, *request, *medium, *first);
                w.u16(*count);
            }
            Self::Store {
                request,
                medium,
                sector,
                data,
            } => {
                put_request(w, tag::STORE, *request, *medium, *sector);
                w.raw(&data[..]);
            }
            Self::Formatted { drive, format } => {
                w.u8(tag::FORMATTED);
                w.u8(*drive);
                config::put_low_level(w, format);
            }
            Self::Credit { applied } => {
                w.u8(tag::CREDIT);
                w.u32(*applied);
            }
            Self::Pointing { follows } => w.raw(&[tag::POINTING, u8::from(*follows)]),
            Self::ClockSet(time) => {
                w.u8(tag::CLOCK_SET);
                put_civil(w, time);
            }
            Self::Lights(lights) => w.raw(&[tag::LIGHTS, lights.floppy, lights.fixed]),
            Self::Resumed(_) | Self::Recall { .. } | Self::Kept(_) | Self::Suspended(_) => {
                self.encode_keeping(w);
            }
            Self::Text(outcome) => put_text(w, outcome.as_ref().map_err(|why| *why)),
            Self::Typed(count) => {
                w.u8(tag::TYPED);
                w.u32(*count);
            }
            Self::TextShown(text) => w.raw(&[tag::TEXT_SHOWN, u8::from(*text)]),
            Self::Folder { .. } | Self::Shared { .. } => self.encode_sharing(w),
            Self::Compile { .. } => self.encode_block(w),
            Self::Interpreting => w.u8(tag::INTERPRETING),
            Self::Debug(said) => said.encode(w),
        }
    }

    /// Append an event of a snapshot kept or taken back to `w`; nothing for
    /// another.
    fn encode_keeping<R: Room>(&self, w: &mut Writer<R>) {
        match self {
            Self::Resumed(resumed) => w.raw(&[tag::RESUMED, u8::from(*resumed)]),
            Self::Recall {
                request,
                offset,
                len,
            } => {
                w.u8(tag::RECALL);
                w.u32(*request);
                w.u32(*offset);
                w.u32(*len);
            }
            Self::Kept(bytes) => {
                w.u8(tag::KEPT);
                w.raw(bytes);
            }
            Self::Suspended(outcome) => {
                w.u8(tag::SUSPENDED);
                put_suspended(w, *outcome);
            }
            _ => {}
        }
    }

    /// Append a block asked for to `w`; nothing for another event.
    fn encode_block<R: Room>(&self, w: &mut Writer<R>) {
        if let Self::Compile {
            request,
            cs,
            ip,
            bytes,
        } = self
        {
            w.u8(tag::COMPILE);
            w.u32(*request);
            w.u16(*cs);
            w.u16(*ip);
            w.bytes(bytes);
        }
    }

    /// Append an event of the shared folders to `w`; nothing for another.
    fn encode_sharing<R: Room>(&self, w: &mut Writer<R>) {
        match self {
            Self::Folder { request, slot, op } => {
                w.u8(tag::FOLDER);
                w.u32(*request);
                w.u8(*slot);
                op.encode(w);
            }
            Self::Shared { slot, drive } => match drive {
                Some(drive) => w.raw(&[tag::SHARED, *slot, 1, *drive]),
                None => w.raw(&[tag::SHARED, *slot, 0]),
            },
            _ => {}
        }
    }
}

impl<'a> Event<'a> {
    /// The event `payload` holds.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a payload that is not a whole event.
    pub fn decode(payload: &'a [u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        r.whole(|r| {
            Ok(match r.u8()? {
                tag::READY => Self::Ready,
                tag::REFUSED => {
                    let (kind, first, second) = (r.u8()?, r.u8()?, r.u8()?);
                    Self::Refused(match (kind, first, second) {
                        (0, error, 0) => Refusal::Board(nth(&CONFIG_ERRORS, error)?),
                        (1, 0, 0) => Refusal::Firmware,
                        (2, drive, error) => Refusal::Fixed(drive, nth(&ATTACH_ERRORS, error)?),
                        (3, 0, 0) => Refusal::Share,
                        (4, 0, 0) => Refusal::Memory,
                        _ => return Err(WireError::Malformed),
                    })
                }
                tag::INSERTED => {
                    let (drive, medium) = (r.u8()?, r.u16()?);
                    let outcome = match r.u8()? {
                        0 => Ok(()),
                        n => Err(nth(&INSERT_REFUSALS, n - 1)?),
                    };
                    Self::Inserted {
                        drive,
                        medium,
                        outcome,
                    }
                }
                tag::EJECTED => Self::Ejected {
                    drive: r.u8()?,
                    medium: r.u16()?,
                },
                tag::FRAME => Self::Frame {
                    slot: r.u8()?,
                    width: r.u16()?,
                    height: r.u16()?,
                },
                tag::OUT_OF_RANGE => Self::OutOfRange { adapter: r.u8()? },
                tag::AUDIO => {
                    let frames = r.take(r.remaining())?;
                    let whole = !frames.is_empty() && frames.len().is_multiple_of(AUDIO_FRAME);
                    if !whole || frames.len() > MAX_AUDIO_FRAMES * AUDIO_FRAME {
                        return Err(WireError::Malformed);
                    }
                    Self::Audio(frames)
                }
                tag::FETCH => Self::Fetch {
                    request: r.u32()?,
                    medium: r.u16()?,
                    first: r.u64()?,
                    count: r.u16()?,
                },
                tag::STORE => {
                    let (request, medium, sector) = (r.u32()?, r.u16()?, r.u64()?);
                    let data = r
                        .take(SECTOR)?
                        .try_into()
                        .map_err(|_| WireError::Malformed)?;
                    Self::Store {
                        request,
                        medium,
                        sector,
                        data,
                    }
                }
                tag::FORMATTED => Self::Formatted {
                    drive: r.u8()?,
                    format: config::low_level(r)?,
                },
                tag::CREDIT => Self::Credit { applied: r.u32()? },
                tag::POINTING => Self::Pointing { follows: flag(r)? },
                tag::CLOCK_SET => Self::ClockSet(civil(r)?),
                tag::LIGHTS => Self::Lights(Lights {
                    floppy: r.u8()?,
                    fixed: r.u8()?,
                }),
                tag::RESUMED => Self::Resumed(flag(r)?),
                tag::RECALL => Self::Recall {
                    request: r.u32()?,
                    offset: r.u32()?,
                    len: r.u32()?,
                },
                tag::KEPT => Self::Kept(kept_piece(r)?),
                tag::SUSPENDED => Self::Suspended(match r.u8()? {
                    0 => Ok(r.u32()?),
                    n => Err(nth(&UNKEPT, n - 1)?),
                }),
                tag::TEXT => Self::Text(text(r)?),
                tag::TYPED => Self::Typed(r.u32()?),
                tag::TEXT_SHOWN => Self::TextShown(flag(r)?),
                found @ (tag::FOLDER | tag::SHARED) => Self::decode_sharing(found, r)?,
                tag::COMPILE => Self::decode_block(r)?,
                tag::INTERPRETING => Self::Interpreting,
                tag::SAID => Self::Debug(debug::Said::decode(r)?),
                _ => return Err(WireError::Malformed),
            })
        })
    }

    /// The block asked for, its fields read from `r`.
    fn decode_block(r: &mut Reader<'a>) -> Result<Self, WireError> {
        Ok(Self::Compile {
            request: r.u32()?,
            cs: r.u16()?,
            ip: r.u16()?,
            bytes: block_bytes(r)?,
        })
    }

    /// The event of the shared folders tagged `found`, its fields read from
    /// `r`.
    fn decode_sharing(found: u8, r: &mut Reader<'a>) -> Result<Self, WireError> {
        Ok(match found {
            tag::FOLDER => Self::Folder {
                request: r.u32()?,
                slot: share_slot(r)?,
                op: folder::Op::decode(r)?,
            },
            tag::SHARED => Self::Shared {
                slot: share_slot(r)?,
                drive: if flag(r)? {
                    let drive = r.u8()?;
                    if drive >= crate::dos::data::DRIVES {
                        return Err(WireError::Malformed);
                    }
                    Some(drive)
                } else {
                    None
                },
            },
            _ => return Err(WireError::Malformed),
        })
    }
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;

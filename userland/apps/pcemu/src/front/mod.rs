//! The front-end's side of the machine session (`plans/PCEMU.md` §3).
//!
//! The worker has run guest code, so nothing it sends is believed until it is
//! checked: every frame it names lies in the share and fits its slot, every
//! sector it asks for lies on a medium the front-end named and is the one
//! request the protocol lets it have, every format it reports fits its drive.
//! The first event that does not is the end of the worker.
//!
//! The host never blocks. Its owner registers [`Host::descriptors`] on its
//! wait-set as [`Host::wants_read`] and [`Host::wants_write`] say, calls
//! [`Host::on_readable`] and [`Host::on_writable`] when they fire, and drains
//! [`Host::event`]. A request for sectors is answered later, through
//! [`Host::answer_fetch`] or [`Host::answer_store`], so the owner's storage
//! may be as slow as it is without stalling anything but the guest.
//!
//! Input beyond the credit window waits here, in order: motion run together,
//! and a key pressed into a full queue dropped with its release, as a
//! keyboard's own buffer overruns.
//!
//! A machine resumed is handed its snapshot here, the host answering each
//! piece the worker recalls; one suspended sends its snapshot here, which the
//! host gathers, bounded, for [`Host::take_kept`].
//!
//! Text to type waits here behind every credited command held before it, and
//! crosses within the worker's typing window; a key pressed stops it.
//!
//! A block the worker asks to be compiled is its owner's to compile and
//! install, within the worker's window of them and in the order asked; once
//! told to interpret, the host answers every block itself with no code.

mod debugger;

use crate::board::Lights;
use crate::chips::keyboard::Scan;
use crate::cpu::CpuModel;
use crate::debug::probe::Reason;
use crate::debug::Registers;
use crate::folder;
use crate::link::debug::{Debug, Seen};
use crate::link::layout::{Layout, MAX_ADAPTERS, PIXEL};
use crate::link::{
    most_kept, takes_key, typeable, Command, Dropped, Event, Form, InsertRefusal, Installed,
    Refusal, Resume, Setup, Text, Unkept, COMPILE_WINDOW, FRONT_INBOUND, FRONT_OUTBOUND,
    INPUT_WINDOW, KEPT_PIECE, MAX_MEDIA, MAX_RUN, QUEUE_MOST, TYPE_WINDOW,
};
use crate::ports::game::Sticks;
use crate::storage::hdc::{LowLevel, MAX_TRACKS};
use crate::storage::{MediaError, SECTOR};
use crate::time::{CpuClock, Nanos};
use crate::video::Unread;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use tairix_abi::time::CivilTime;
use tairix_log::Sink;
use tairix_reclaim::PressureBand;
use tairix_sandbox::session::{
    SandboxSession, SessionBounds, SessionDescriptors, SessionError, SessionTransport,
};
use tairix_sandbox::wire::Writer;

/// Why the worker is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// It crashed, or its session failed; the session has logged how.
    Failed,
    /// It sent what the protocol does not allow, and was ended for it.
    Violation(&'static str),
    /// It closed the session itself.
    Finished,
}

/// What the worker did, checked.
#[derive(Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "an event is lent one at a time, borrowing the host, and never kept"
)]
pub enum HostEvent<'a> {
    /// The machine is built and waits to be run.
    Ready,
    /// The machine could not be built.
    Refused(Refusal),
    /// A frame of adapter `adapter`, its pixels in the host's own memory.
    Frame {
        /// The adapter, counted in order among those fitted.
        adapter: u8,
        /// Its dots.
        width: u32,
        /// Its lines.
        height: u32,
        /// `width * height` pixels of red, green, blue and alpha.
        pixels: &'a [u8],
    },
    /// The adapter's guest programmed a frame its monitor cannot show.
    OutOfRange {
        /// The adapter.
        adapter: u8,
    },
    /// Sound: interleaved 16-bit little-endian stereo frames.
    Audio(&'a [u8]),
    /// Read `count` sectors of `medium` from `first`, and answer through
    /// [`Host::answer_fetch`].
    Fetch {
        /// The image.
        medium: u16,
        /// Its first sector.
        first: u64,
        /// How many.
        count: u16,
    },
    /// Write sector `sector` of `medium`, and answer through
    /// [`Host::answer_store`].
    Store {
        /// The image.
        medium: u16,
        /// The sector.
        sector: u64,
        /// What it holds.
        data: &'a [u8; SECTOR],
    },
    /// A diskette went in, or why it did not.
    Inserted {
        /// The drive.
        drive: u8,
        /// The image.
        medium: u16,
        /// Whether it went in.
        outcome: Result<(), InsertRefusal>,
    },
    /// A diskette came out; nothing more is asked of its image.
    Ejected {
        /// The drive.
        drive: u8,
        /// The image.
        medium: u16,
    },
    /// Fixed disk `drive` was low-level formatted: keep this beside its
    /// image.
    Formatted {
        /// The drive.
        drive: u8,
        /// Its format.
        format: LowLevel,
    },
    /// The guest now reads its mouse through the emulator's own driver,
    /// which follows [`Host::point`], or (`false`) reads the mouse itself,
    /// and wants its motion through [`Host::mouse`].
    Pointing {
        /// Whether the driver follows the host's pointer.
        follows: bool,
    },
    /// The guest set its clock card, which now reads this: what the machine
    /// keeps as its offset from the host's clock.
    ClockSet(CivilTime),
    /// What the drives' lamps show now.
    Lights(Lights),
    /// Whether the machine took back its snapshot, or was built afresh; it
    /// comes before [`HostEvent::Ready`] for a machine resumed.
    Resumed(bool),
    /// The machine stopped and its snapshot came whole, for
    /// [`Host::take_kept`], or why it was not kept.
    Suspended(Result<(), Unkept>),
    /// The text [`Host::read_text`] asked for, or why it was not read.
    Text(Result<Text<'a>, Unread>),
    /// Whether adapter 0 now shows text.
    TextShown(bool),
    /// Ask share `slot` `op`, and answer through [`Host::answer_folder`].
    Folder {
        /// The share.
        slot: u8,
        /// What is asked.
        op: folder::Op<'a>,
    },
    /// Share `slot` is drive `drive`, or took none.
    Shared {
        /// The share.
        slot: u8,
        /// Its drive, `0` for `A:`.
        drive: Option<u8>,
    },
    /// Compile the block of `model`'s code at `cs:ip`, whose bytes these
    /// are, install it in the worker, and answer through
    /// [`Host::answer_block`].
    Compile {
        /// The request, which the answer names.
        request: u32,
        /// The processor the machine is built around.
        model: CpuModel,
        /// The block's code segment.
        cs: u16,
        /// Its first byte's offset.
        ip: u16,
        /// Its bytes from there.
        bytes: &'a [u8],
    },
    /// A translated block misbehaved: the machine runs interpreted from now
    /// on, and its block requests are answered here.
    Interpreting,
    /// The processor stopped under the debugger.
    Stopped {
        /// Why.
        reason: Reason,
        /// How it stands.
        registers: Registers,
    },
    /// The answer to a [`Host::look`].
    Seen(Seen<'a>),
    /// The worker is gone.
    Ended(Ending),
}

/// The most text [`Host::type_text`] takes at once: a segment's worth, as
/// much as a DOS program holds in one buffer.
pub const MAX_TYPED: usize = 64 * 1024;

/// Why a machine's session could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The session could not be admitted.
    Session(SessionError),
    /// The worker was handed no shared region, or one smaller than the
    /// machine's frame slots.
    Share,
    /// The snapshot to resume is empty, or larger than any.
    Snapshot,
}

/// Why a command could not be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The worker is gone.
    Ended,
    /// So much input waits that the worker is plainly not taking it.
    Full,
    /// No machine has a diskette drive of that number.
    NoDrive,
    /// The drive's last diskette has not yet been answered for.
    Busy,
    /// No request with that number is waiting for an answer.
    NoRequest,
    /// The answer is not the size the request asked for.
    Size,
    /// The machine is already being suspended.
    Suspending,
    /// Text with none in it, more than [`MAX_TYPED`], or a byte
    /// [`typeable`] does not admit.
    Untypeable,
    /// No debugger is attached.
    Detached,
    /// A breakpoint added that a probe cannot hold, or is held already, or
    /// one removed that is not held.
    Breakpoint,
}

/// A medium the host named.
#[derive(Clone, Copy, Debug)]
struct Named {
    id: u16,
    form: Form,
}

/// The request the worker has outstanding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Fetch { request: u32, count: u16 },
    Store { request: u32 },
    Folder { request: u32, slot: u8 },
}

/// A credited command waiting for room in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    Key(Scan, bool),
    ReleaseKeys,
    Mouse {
        dx: i32,
        dy: i32,
        buttons: u8,
    },
    Pointer {
        adapter: u8,
        x: u16,
        y: u16,
        buttons: u8,
    },
    Sticks(Sticks),
    Insert {
        drive: u8,
        medium: u16,
        form: Form,
    },
    Eject {
        drive: u8,
    },
    Share {
        slot: u8,
        label: folder::DosName,
        writable: bool,
    },
    Unshare {
        slot: u8,
    },
    Run,
    Hold,
    Reset,
    Speed(CpuClock),
    Clock(CivilTime),
    Suspend,
    ReadText,
    StopTyping,
    Debug(Debug),
}

impl Held {
    fn command(&self) -> Command<'static> {
        match *self {
            Self::Key(scan, down) => Command::Key { scan, down },
            Self::ReleaseKeys => Command::ReleaseKeys,
            Self::Mouse { dx, dy, buttons } => Command::Mouse { dx, dy, buttons },
            Self::Pointer {
                adapter,
                x,
                y,
                buttons,
            } => Command::Pointer {
                adapter,
                x,
                y,
                buttons,
            },
            Self::Sticks(sticks) => Command::Sticks(sticks),
            Self::Insert {
                drive,
                medium,
                form,
            } => Command::Insert {
                drive,
                medium,
                form,
            },
            Self::Eject { drive } => Command::Eject { drive },
            Self::Share {
                slot,
                label,
                writable,
            } => Command::Share {
                slot,
                label,
                writable,
            },
            Self::Unshare { slot } => Command::Unshare { slot },
            Self::Run => Command::Run,
            Self::Hold => Command::Hold,
            Self::Reset => Command::Reset,
            Self::Speed(rate) => Command::Speed(rate),
            Self::Clock(time) => Command::Clock(time),
            Self::Suspend => Command::Suspend,
            Self::ReadText => Command::ReadText,
            Self::StopTyping => Command::StopTyping,
            Self::Debug(debug) => Command::Debug(debug),
        }
    }
}

/// The front-end's side of one machine's session.
pub struct Host<T: SessionTransport, S: Sink> {
    session: SandboxSession<T, S>,
    layout: Layout,
    started: bool,
    ended: Option<Ending>,
    reported_end: bool,
    media: Vec<Named>,
    next_medium: u16,
    /// Each fixed drive's tracks, which bound the bad tracks a format marks.
    tracks: [Option<u32>; 2],
    /// What each diskette drive holds, as the worker last said.
    drives: [Option<u16>; 4],
    /// The diskette drives and fixed disks fitted, a bit each, which bound
    /// the lamps the worker may light.
    fitted: Lights,
    /// The diskette each drive was last asked to take, not yet answered.
    inserting: [Option<u16>; 4],
    next_request: u32,
    asked: Option<Asked>,
    /// An answer, encoded, the session had no room for yet.
    answer: Vec<u8>,
    /// Slots copied out and not yet handed back.
    releases: u32,
    fill: Option<u32>,
    band: Option<PressureBand>,
    /// Credited commands sent, and the count the worker last acknowledged.
    sent: u32,
    acked: u32,
    held: VecDeque<Held>,
    dropped: Dropped,
    /// The last event's payload, which an event lends from.
    event: Vec<u8>,
    frames: [Vec<u8>; MAX_ADAPTERS],
    out: Vec<u8>,
    /// The snapshot a machine resumed recalls, until it says whether it took
    /// it.
    recall: Option<Vec<u8>>,
    /// The snapshot of a machine being suspended, as it comes.
    keeping: Option<Vec<u8>>,
    /// The snapshot of a machine suspended, whole, until it is taken.
    kept: Option<Vec<u8>>,
    /// Whether a read of the text is unanswered.
    reading: bool,
    /// Text to type, and how much of it has been sent.
    typing: Vec<u8>,
    typing_sent: usize,
    /// Characters sent to type that the worker has not yet accounted for.
    typing_out: usize,
    /// Which share slots hold a folder.
    shared: [bool; folder::SHARES],
    /// The processor the machine is built around, and whether an 8087 is
    /// beside it.
    chips: Chips,
    /// The blocks the worker asked to be compiled and not yet answered, and
    /// the number its next request takes.
    blocks: Vec<u32>,
    next_block: u32,
    /// The answers to blocks not yet sent.
    compiled: VecDeque<(u32, Option<Installed>)>,
    interpreting: Interpreting,
    /// The debugger, while one is attached, and the detaches the worker has
    /// yet to say it took.
    debugging: Option<debugger::Debugging>,
    detaching: u32,
}

/// The processor a machine is built around, and whether an 8087 is fitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Chips {
    model: CpuModel,
    coprocessor: bool,
}

/// Whether the worker is to ask for no more blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interpreting {
    /// It asks for what it grows hot on.
    No,
    /// It is to ask for no more, and is yet to be told.
    Owed,
    /// It has been told.
    Told,
    /// It said a translated block misbehaved, and asks for no more.
    Misbehaved,
}

impl<T: SessionTransport, S: Sink> Host<T, S> {
    /// The host for the machine `setup` describes, over `transport`, whose
    /// fixed disks the front-end names `1` and `2` in order: the machine is
    /// started at once, and waits to be run.
    ///
    /// # Errors
    ///
    /// [`StartError`]: the session's refusal to be admitted, or a share
    /// smaller than the machine's layout. The worker is disposed of.
    pub fn start(transport: T, sink: S, setup: Setup) -> Result<Self, StartError> {
        Self::open(transport, sink, setup, None)
    }

    /// The host for the suspended machine `setup` describes, which recalls
    /// `kept`, its snapshot, before it is ready; `away` is how long it was
    /// suspended. [`HostEvent::Resumed`] says whether it took it.
    ///
    /// # Errors
    ///
    /// As [`Self::start`], and [`StartError::Snapshot`] for a snapshot empty
    /// or larger than any.
    pub fn resume(
        transport: T,
        sink: S,
        mut setup: Setup,
        kept: Vec<u8>,
        away: Nanos,
    ) -> Result<Self, StartError> {
        let len = u32::try_from(kept.len())
            .ok()
            .filter(|&len| len > 0 && kept.len() <= most_kept())
            .ok_or(StartError::Snapshot)?;
        setup.resume = Some(Resume { len, away });
        Self::open(transport, sink, setup, Some(kept))
    }

    fn open(
        transport: T,
        sink: S,
        setup: Setup,
        recall: Option<Vec<u8>>,
    ) -> Result<Self, StartError> {
        let layout = Layout::of(&setup.config.video);
        let bounds =
            SessionBounds::new(FRONT_OUTBOUND, FRONT_INBOUND).map_err(StartError::Session)?;
        let session = SandboxSession::new(transport, bounds, sink).map_err(StartError::Session)?;
        if session
            .share()
            .is_none_or(|share| share.byte_len() < layout.len())
        {
            return Err(StartError::Share);
        }
        let memory = StartError::Session(SessionError::OutOfMemory);
        let mut frames: [Vec<u8>; MAX_ADAPTERS] = Default::default();
        for (n, frame) in (0u8..).zip(frames.iter_mut()) {
            if let Some((_, slot)) = layout.slots().find(|(_, slot)| slot.adapter == n) {
                frame.try_reserve_exact(slot.len()).map_err(|_| memory)?;
            }
        }
        let mut media = Vec::new();
        media.try_reserve_exact(MAX_MEDIA).map_err(|_| memory)?;
        let mut held = VecDeque::new();
        held.try_reserve_exact(QUEUE_MOST).map_err(|_| memory)?;
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(COMPILE_WINDOW)
            .map_err(|_| memory)?;
        let mut compiled = VecDeque::new();
        compiled
            .try_reserve_exact(COMPILE_WINDOW)
            .map_err(|_| memory)?;
        let mut next_medium = 1;
        let mut tracks = [None; 2];
        for (disk, drive) in setup.fixed.iter().zip(&mut tracks) {
            let Some(disk) = disk else { continue };
            *drive = Some(u32::from(disk.kind.cylinders) * u32::from(disk.kind.heads));
            media.push(Named {
                id: disk.medium,
                form: Form::Sectors {
                    count: disk.sectors,
                    read_only: disk.read_only,
                },
            });
            next_medium = disk.medium.saturating_add(1);
        }
        let floppy = setup.config.floppy.map_or(0, |floppy| {
            (0..floppy.drives.len())
                .filter(|&n| floppy.drives[n].is_some())
                .fold(0, |fitted, n| fitted | 1 << n)
        });
        let fixed = (0..tracks.len())
            .filter(|&n| tracks[n].is_some())
            .fold(0, |fitted, n| fitted | 1 << n);
        let mut host = Self {
            session,
            layout,
            started: false,
            ended: None,
            reported_end: false,
            media,
            next_medium,
            tracks,
            drives: [None; 4],
            fitted: Lights { floppy, fixed },
            inserting: [None; 4],
            next_request: 0,
            asked: None,
            answer: Vec::new(),
            releases: 0,
            fill: None,
            band: None,
            sent: 0,
            acked: 0,
            held,
            dropped: Dropped::default(),
            event: Vec::new(),
            frames,
            out: Vec::new(),
            recall,
            keeping: None,
            kept: None,
            reading: false,
            typing: Vec::new(),
            typing_sent: 0,
            typing_out: 0,
            shared: [false; folder::SHARES],
            chips: Chips {
                model: setup.config.cpu,
                coprocessor: setup.config.coprocessor,
            },
            debugging: None,
            detaching: 0,
            blocks,
            next_block: 0,
            compiled,
            interpreting: Interpreting::No,
        };
        host.send_now(&Command::Start(setup))
            .map_err(StartError::Session)?;
        host.sent = 1;
        Ok(host)
    }

    /// The descriptors to register on the owner's wait-set, while the worker
    /// lives.
    #[must_use]
    pub fn descriptors(&self) -> Option<SessionDescriptors> {
        self.session.descriptors()
    }

    /// Whether to arm read readiness.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.session.wants_read()
    }

    /// Whether to arm write readiness: bytes are queued, or waiting to be.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        self.session.wants_write()
    }

    /// The worker's stream is readable.
    pub fn on_readable(&mut self) {
        if self.session.on_readable().is_err() {
            self.fail();
        }
    }

    /// The worker's stream has room: send what waits.
    pub fn on_writable(&mut self) {
        if self.session.on_writable().is_err() {
            self.fail();
            return;
        }
        self.flush();
        if self.session.on_writable().is_err() {
            self.fail();
        }
    }

    /// The session failed, and has contained its worker.
    fn fail(&mut self) {
        self.ended.get_or_insert(Ending::Failed);
    }

    /// The worker sent what cannot be believed: end it.
    fn condemn(&mut self, why: &'static str) {
        self.session.condemn(why);
        self.ended.get_or_insert(Ending::Violation(why));
    }

    /// The host itself can go no further with this worker — it has no memory
    /// to frame a command or take an event — so the worker is ended rather
    /// than left running out of reach.
    fn lose(&mut self, why: &'static str) {
        self.session.condemn(why);
        self.ended.get_or_insert(Ending::Failed);
    }

    /// Encode and queue `command` at once; it fits, or the session is full.
    fn send_now(&mut self, command: &Command<'_>) -> Result<(), SessionError> {
        let mut w = Writer::refusable_in(core::mem::take(&mut self.out));
        command.encode(&mut w);
        let Some(payload) = w.finish() else {
            return Err(SessionError::OutOfMemory);
        };
        let sent = self.session.send(&payload);
        self.out = payload;
        sent
    }

    /// Send what waits, most urgent first: the answer the guest is stopped
    /// for, the slots handed back, the readings, then held input as far as
    /// the window allows.
    fn flush(&mut self) {
        if self.ended.is_some() {
            return;
        }
        if !self.answer.is_empty() {
            match self.session.send(&self.answer) {
                Ok(()) => self.answer.clear(),
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        while self.releases != 0 {
            let slot = u8::try_from(self.releases.trailing_zeros()).unwrap_or(u8::MAX);
            match self.send_now(&Command::Released { slot }) {
                Ok(()) => self.releases &= self.releases - 1,
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        if let Some(frames) = self.fill {
            match self.send_now(&Command::Fill { frames }) {
                Ok(()) => self.fill = None,
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        if let Some(band) = self.band {
            match self.send_now(&Command::Pressure(band)) {
                Ok(()) => self.band = None,
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        while let Some(&(request, code)) = self.compiled.front() {
            match self.send_now(&Command::Compiled { request, code }) {
                Ok(()) => drop(self.compiled.pop_front()),
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        if self.interpreting == Interpreting::Owed {
            match self.send_now(&Command::Interpret) {
                Ok(()) => self.interpreting = Interpreting::Told,
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        while self.sent.wrapping_sub(self.acked) < INPUT_WINDOW {
            let Some(held) = self.held.front().copied() else {
                break;
            };
            match self.send_now(&held.command()) {
                Ok(()) => {
                    self.held.pop_front();
                    self.sent = self.sent.wrapping_add(1);
                }
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        if self.held.is_empty() {
            self.send_typing();
        }
    }

    /// Send the text to type as the typing window opens, in pieces of at
    /// least half of it, or of what is left.
    fn send_typing(&mut self) {
        while self.typing_sent < self.typing.len() {
            let rest = self.typing.len() - self.typing_sent;
            let room = TYPE_WINDOW - self.typing_out;
            if room < rest.min(TYPE_WINDOW / 2) {
                return;
            }
            let piece = self.typing_sent..self.typing_sent + rest.min(room);
            let typing = core::mem::take(&mut self.typing);
            let sent = self.send_now(&Command::Type(&typing[piece.clone()]));
            self.typing = typing;
            match sent {
                Ok(()) => {
                    self.typing_sent = piece.end;
                    self.typing_out += piece.len();
                }
                Err(SessionError::OutboundFull) => return,
                Err(_) => return self.lose("the front-end could not frame a command"),
            }
        }
        self.typing = Vec::new();
        self.typing_sent = 0;
    }

    /// Queue credited `held` behind what already waits, and send what the
    /// window allows.
    fn hold(&mut self, held: Held) -> Result<(), Refused> {
        if self.ended.is_some() {
            return Err(Refused::Ended);
        }
        if self.held.len() >= QUEUE_MOST {
            return Err(Refused::Full);
        }
        self.held.push_back(held);
        self.flush();
        Ok(())
    }

    /// Run the machine.
    ///
    /// # Errors
    ///
    /// [`Refused`] when the worker is gone or no longer taking input.
    pub fn run(&mut self) -> Result<(), Refused> {
        self.hold(Held::Run)
    }

    /// Stop running it.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn pause(&mut self) -> Result<(), Refused> {
        self.hold(Held::Hold)
    }

    /// Power it off and on; what was being typed is not typed.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn reset(&mut self) -> Result<(), Refused> {
        // Dropped first, so no piece follows the reset to the new machine.
        self.forget_typing();
        self.hold(Held::Reset)?;
        self.debug_reset();
        Ok(())
    }

    /// Run the processor at `rate`.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn speed(&mut self, rate: CpuClock) -> Result<(), Refused> {
        self.hold(Held::Speed(rate))
    }

    /// Set the clock card to `time`.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn clock(&mut self, time: CivilTime) -> Result<(), Refused> {
        self.hold(Held::Clock(time))
    }

    /// Stop the machine and have it whole: [`HostEvent::Suspended`] says
    /// when its snapshot has come, for [`Self::take_kept`].
    ///
    /// # Errors
    ///
    /// As [`Self::run`], or [`Refused::Suspending`] once asked.
    pub fn suspend(&mut self) -> Result<(), Refused> {
        if self.keeping.is_some() || self.kept.is_some() {
            return Err(Refused::Suspending);
        }
        self.forget_typing();
        self.hold(Held::Suspend)?;
        self.keeping = Some(Vec::new());
        Ok(())
    }

    /// The snapshot of the machine suspended, once [`HostEvent::Suspended`]
    /// has said it came whole.
    pub fn take_kept(&mut self) -> Option<Vec<u8>> {
        self.kept.take()
    }

    /// Put the diskette laid out as `form` in drive `drive`, answering the
    /// name its requests will carry; [`HostEvent::Inserted`] says whether it
    /// went in.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn insert(&mut self, drive: u8, form: Form) -> Result<u16, Refused> {
        let waiting = *self
            .inserting
            .get(usize::from(drive))
            .ok_or(Refused::NoDrive)?;
        if waiting.is_some() {
            return Err(Refused::Busy);
        }
        if self.media.len() == MAX_MEDIA || self.next_medium == u16::MAX {
            return Err(Refused::Full);
        }
        let medium = self.next_medium;
        self.hold(Held::Insert {
            drive,
            medium,
            form,
        })?;
        self.next_medium += 1;
        self.media.push(Named { id: medium, form });
        self.inserting[usize::from(drive)] = Some(medium);
        Ok(medium)
    }

    /// Take the diskette out of drive `drive`.
    ///
    /// # Errors
    ///
    /// As [`Self::run`], or [`Refused::NoDrive`].
    pub fn eject(&mut self, drive: u8) -> Result<(), Refused> {
        if usize::from(drive) >= self.drives.len() {
            return Err(Refused::NoDrive);
        }
        self.hold(Held::Eject { drive })
    }

    /// A key goes down or comes up. A press the queue has no room for is
    /// dropped, and its release with it. A press stops the typing: the
    /// keyboard is the user's again.
    pub fn key(&mut self, scan: Scan, down: bool) {
        if down {
            self.stop_typing();
        }
        if !down && self.dropped.release(scan) {
            return;
        }
        if !takes_key(self.held.len(), down) {
            self.dropped.press(scan);
            return;
        }
        let _ = self.hold(Held::Key(scan, down));
    }

    /// Every held key comes up: the window lost the keyboard. It is never
    /// refused for a full queue: the key events still waiting are moot once
    /// it is sent, so they give way to it.
    pub fn release_keys(&mut self) {
        self.dropped.clear();
        if self.held.len() >= QUEUE_MOST {
            self.held
                .retain(|held| !matches!(held, Held::Key(..) | Held::ReleaseKeys));
        }
        let _ = self.hold(Held::ReleaseKeys);
    }

    /// The serial mouse moves with `buttons` held: run together with the
    /// motion already waiting while the buttons stand as they did.
    pub fn mouse(&mut self, dx: i32, dy: i32, buttons: u8) {
        if let Some(Held::Mouse {
            dx: x,
            dy: y,
            buttons: held,
        }) = self.held.back_mut()
        {
            if *held == buttons {
                *x = x.saturating_add(dx);
                *y = y.saturating_add(dy);
                return;
            }
        }
        let _ = self.hold(Held::Mouse { dx, dy, buttons });
    }

    /// The host's pointer stands at dot `x` of line `y` of adapter
    /// `adapter`'s frame with `buttons` held: the latest place in a run with
    /// the buttons as they stand is the one that matters.
    pub fn point(&mut self, adapter: u8, x: u16, y: u16, buttons: u8) {
        if let Some(Held::Pointer {
            adapter: on,
            x: at_x,
            y: at_y,
            buttons: held,
        }) = self.held.back_mut()
        {
            if (*on, *held) == (adapter, buttons) {
                (*at_x, *at_y) = (x, y);
                return;
            }
        }
        let _ = self.hold(Held::Pointer {
            adapter,
            x,
            y,
            buttons,
        });
    }

    /// The joysticks stand as `sticks`: the latest of a run of readings is
    /// the one that matters.
    pub fn sticks(&mut self, sticks: Sticks) {
        if let Some(Held::Sticks(held)) = self.held.back_mut() {
            *held = sticks;
            return;
        }
        let _ = self.hold(Held::Sticks(sticks));
    }

    /// The front-end's sound buffer holds `frames`.
    pub fn fill(&mut self, frames: u32) {
        self.fill = Some(frames);
        self.flush();
    }

    /// The machine's memory pressure is `band`.
    pub fn pressure(&mut self, band: PressureBand) {
        self.band = Some(band);
        self.flush();
    }

    /// Answer the fetch the worker has outstanding: `data` its sectors, or
    /// why they could not be read.
    ///
    /// # Errors
    ///
    /// [`Refused::NoRequest`] when no fetch waits, [`Refused::Size`] for data
    /// not the run asked for, [`Refused::Ended`] when the worker is gone.
    pub fn answer_fetch(&mut self, answer: Result<&[u8], MediaError>) -> Result<(), Refused> {
        let Some(Asked::Fetch { request, count }) = self.asked else {
            return Err(Refused::NoRequest);
        };
        if answer.is_ok_and(|data| data.len() != usize::from(count) * SECTOR) {
            return Err(Refused::Size);
        }
        self.answered(&Command::Fetched { request, answer })
    }

    /// Share the folder the front-end serves as share `slot`, labelled
    /// `label`; [`HostEvent::Shared`] says which drive it is.
    ///
    /// # Errors
    ///
    /// As [`Self::run`], or [`Refused::Busy`] for a slot already shared or
    /// past [`folder::SHARES`].
    pub fn share(
        &mut self,
        slot: u8,
        label: folder::DosName,
        writable: bool,
    ) -> Result<(), Refused> {
        let held = self
            .shared
            .get(usize::from(slot))
            .copied()
            .ok_or(Refused::Busy)?;
        if held {
            return Err(Refused::Busy);
        }
        self.hold(Held::Share {
            slot,
            label,
            writable,
        })?;
        self.shared[usize::from(slot)] = true;
        Ok(())
    }

    /// Stop sharing share `slot`: a request of it the worker has outstanding
    /// is answered that the share is gone.
    ///
    /// # Errors
    ///
    /// As [`Self::run`], or [`Refused::NoRequest`] for a slot not shared.
    pub fn unshare(&mut self, slot: u8) -> Result<(), Refused> {
        if !self.shared.get(usize::from(slot)).copied().unwrap_or(false) {
            return Err(Refused::NoRequest);
        }
        self.hold(Held::Unshare { slot })?;
        self.shared[usize::from(slot)] = false;
        if let Some(Asked::Folder { slot: asked, .. }) = self.asked {
            if asked == slot {
                self.answer_folder(Err(folder::Refusal::Gone))?;
            }
        }
        Ok(())
    }

    /// Answer the folder request the worker has outstanding.
    ///
    /// # Errors
    ///
    /// [`Refused::NoRequest`] when none waits, [`Refused::Size`] for a read
    /// past [`folder::IO_MOST`], [`Refused::Ended`] when the worker is gone.
    pub fn answer_folder(
        &mut self,
        answer: Result<folder::Reply<'_>, folder::Refusal>,
    ) -> Result<(), Refused> {
        let Some(Asked::Folder { request, .. }) = self.asked else {
            return Err(Refused::NoRequest);
        };
        if matches!(answer, Ok(folder::Reply::Read(bytes)) if bytes.len() > folder::IO_MOST) {
            return Err(Refused::Size);
        }
        self.answered(&Command::Foldered { request, answer })
    }

    /// Answer block request `request`: `code`, where its code was installed
    /// in the worker, or none.
    ///
    /// # Errors
    ///
    /// [`Refused::NoRequest`] when that block waits for no answer,
    /// [`Refused::Ended`] when the worker is gone.
    pub fn answer_block(&mut self, request: u32, code: Option<Installed>) -> Result<(), Refused> {
        if self.ended.is_some() {
            return Err(Refused::Ended);
        }
        let at = self
            .blocks
            .iter()
            .position(|&asked| asked == request)
            .ok_or(Refused::NoRequest)?;
        self.blocks.swap_remove(at);
        self.compiled.push_back((request, code));
        self.flush();
        Ok(())
    }

    /// The worker's word that a translated block misbehaved: said once,
    /// and its blocks answered here from now on.
    fn misbehaved(&mut self) -> Result<Kind, &'static str> {
        if self.interpreting == Interpreting::Misbehaved {
            return Err("a second word that its blocks misbehaved");
        }
        self.interpreting = Interpreting::Misbehaved;
        Ok(Kind::Interpreting)
    }

    /// Have the worker ask for no more blocks: its machine is interpreted
    /// from now on, but for the blocks it has installed.
    pub fn interpret(&mut self) {
        if self.interpreting == Interpreting::No {
            self.interpreting = Interpreting::Owed;
            self.flush();
        }
    }

    /// The worker's request `request`, for the block at `cs:ip` whose `len`
    /// bytes end the event: the next in turn, within the window. Once
    /// interpreting, it is answered at once with no code.
    fn compile(
        &mut self,
        request: u32,
        (cs, ip): (u16, u16),
        len: usize,
    ) -> Result<Option<Kind>, &'static str> {
        if request != self.next_block {
            return Err("a block asked for out of turn");
        }
        if self.blocks.len() >= COMPILE_WINDOW {
            return Err("more blocks asked for than the window holds");
        }
        self.next_block = request.wrapping_add(1);
        self.blocks.push(request);
        if self.interpreting != Interpreting::No {
            let _ = self.answer_block(request, None);
            return Ok(None);
        }
        Ok(Some(Kind::Compile {
            request,
            cs,
            ip,
            len,
        }))
    }

    /// Answer the store the worker has outstanding.
    ///
    /// # Errors
    ///
    /// As [`Self::answer_fetch`], but for a store.
    pub fn answer_store(&mut self, answer: Result<(), MediaError>) -> Result<(), Refused> {
        let Some(Asked::Store { request }) = self.asked else {
            return Err(Refused::NoRequest);
        };
        self.answered(&Command::Stored { request, answer })
    }

    fn answered(&mut self, command: &Command<'_>) -> Result<(), Refused> {
        if self.ended.is_some() {
            return Err(Refused::Ended);
        }
        let mut w = Writer::refusable_in(core::mem::take(&mut self.answer));
        command.encode(&mut w);
        let Some(answer) = w.finish() else {
            self.lose("the front-end could not frame an answer");
            return Err(Refused::Ended);
        };
        self.answer = answer;
        self.asked = None;
        self.flush();
        Ok(())
    }

    /// The next thing the worker did, checked; `None` once nothing more has
    /// arrived. [`HostEvent::Ended`] comes once, when the worker is gone.
    pub fn event(&mut self) -> Option<HostEvent<'_>> {
        loop {
            if let Some(ending) = self.ended {
                if core::mem::replace(&mut self.reported_end, true) {
                    return None;
                }
                return Some(HostEvent::Ended(ending));
            }
            let Self { session, event, .. } = self;
            let got = session.recv(|payload| {
                event.clear();
                event
                    .try_reserve(payload.len())
                    .map(|()| event.extend_from_slice(payload))
                    .is_ok()
            });
            match got {
                Ok(Some(true)) => {}
                Ok(Some(false)) => {
                    self.lose("the front-end had no memory for an event");
                    continue;
                }
                Ok(None) => {
                    if self.session.peer_finished() {
                        self.ended = Some(Ending::Finished);
                        continue;
                    }
                    return None;
                }
                Err(_) => {
                    self.fail();
                    continue;
                }
            }
            match self.check() {
                Ok(Some(kind)) => return Some(self.lend(kind)),
                Ok(None) => {}
                Err(why) => self.condemn(why),
            }
        }
    }

    /// Check the event just taken, acting on what it means for the host's
    /// own state; answers what to hand the owner, if anything.
    fn check(&mut self) -> Result<Option<Kind>, &'static str> {
        let event = core::mem::take(&mut self.event);
        let checked = self.check_event(&event);
        self.event = event;
        checked
    }

    fn check_event(&mut self, payload: &[u8]) -> Result<Option<Kind>, &'static str> {
        let event = Event::decode(payload).map_err(|_| "a malformed event")?;
        let opening = matches!(
            event,
            Event::Ready | Event::Refused(_) | Event::Recall { .. } | Event::Resumed(_)
        );
        if !self.started && !opening {
            return Err("an event before the machine was ready");
        }
        Ok(Some(match event {
            Event::Ready if self.recall.is_some() => {
                return Err("ready before saying whether it resumed");
            }
            Event::Ready if !self.started => {
                self.started = true;
                Kind::Ready
            }
            Event::Recall {
                request,
                offset,
                len,
            } => return self.recall(request, offset, len),
            Event::Resumed(resumed) => {
                if self.started || self.recall.take().is_none() {
                    return Err("a resume never asked");
                }
                Kind::Resumed(resumed)
            }
            Event::Kept(piece) => return self.kept_piece(piece).map(|()| None),
            Event::Suspended(outcome) => self.suspended(outcome)?,
            Event::Ready => return Err("ready twice"),
            Event::Refused(refusal) => {
                self.ended = Some(Ending::Finished);
                Kind::Refused(refusal)
            }
            Event::Credit { applied } => {
                if applied.wrapping_sub(self.acked) > self.sent.wrapping_sub(self.acked) {
                    return Err("credit for commands never sent");
                }
                self.acked = applied;
                self.flush();
                return Ok(None);
            }
            Event::Frame {
                slot,
                width,
                height,
            } => return self.copy_frame(slot, u32::from(width), u32::from(height)),
            Event::OutOfRange { adapter } => {
                if self.layout.slots().all(|(_, slot)| slot.adapter != adapter) {
                    return Err("an adapter the machine does not have");
                }
                Kind::OutOfRange(adapter)
            }
            Event::Audio(_) => Kind::Audio,
            Event::Fetch {
                request,
                medium,
                first,
                count,
            } => self.fetch(request, medium, first, count)?,
            Event::Store {
                request,
                medium,
                sector,
                ..
            } => self.store(request, medium, sector)?,
            Event::Inserted {
                drive,
                medium,
                outcome,
            } => self.inserted(drive, medium, outcome)?,
            Event::Ejected { drive, medium } => self.ejected(drive, medium)?,
            Event::Formatted { drive, format } => self.formatted(drive, format)?,
            Event::Pointing { follows } => Kind::Pointing(follows),
            Event::ClockSet(time) => Kind::ClockSet(time),
            Event::Lights(lights) => self.lit(lights)?,
            Event::Text(outcome) => {
                if !core::mem::replace(&mut self.reading, false) {
                    return Err("text never asked for");
                }
                Kind::Text(outcome.map(|text| (text.columns, text.rows)))
            }
            Event::Typed(count) => return self.typed(count).map(|()| None),
            Event::TextShown(text) => Kind::TextShown(text),
            Event::Folder { request, slot, op } => return self.folder(request, slot, op),
            Event::Compile {
                request,
                cs,
                ip,
                bytes,
            } => return self.compile(request, (cs, ip), bytes.len()),
            Event::Interpreting => return self.misbehaved().map(Some),
            Event::Shared { slot, drive } => {
                if !self.shared[usize::from(slot)] {
                    // A drive told of a share stopped meanwhile.
                    return Ok(None);
                }
                Kind::Shared { slot, drive }
            }
            Event::Debug(said) => return self.said(payload, said),
        }))
    }

    /// The worker ejected `medium` from `drive`, which must have held it.
    fn ejected(&mut self, drive: u8, medium: u16) -> Result<Kind, &'static str> {
        let held = self
            .drives
            .get_mut(usize::from(drive))
            .ok_or("a drive the machine does not have")?;
        if *held != Some(medium) {
            return Err("an image ejected that was not in its drive");
        }
        *held = None;
        self.forget(medium);
        Ok(Kind::Ejected { drive, medium })
    }

    /// The worker's request of share `slot`: a share stopped since it was
    /// asked is answered gone at once, unseen by the owner.
    fn folder(
        &mut self,
        request: u32,
        slot: u8,
        op: folder::Op<'_>,
    ) -> Result<Option<Kind>, &'static str> {
        self.take_request(request)?;
        self.asked = Some(Asked::Folder { request, slot });
        if !self.shared[usize::from(slot)] {
            let _ = self.answered(&Command::Foldered {
                request,
                answer: Err(folder::Refusal::Gone),
            });
            return Ok(None);
        }
        let (op, bytes) = op.detach();
        Ok(Some(Kind::Folder {
            slot,
            op,
            written: bytes.map_or(0, <[u8]>::len),
        }))
    }

    /// Lamps the worker lit, which can only be of drives the machine has.
    fn lit(&self, lights: Lights) -> Result<Kind, &'static str> {
        if lights.floppy & !self.fitted.floppy != 0 || lights.fixed & !self.fitted.fixed != 0 {
            return Err("a lamp of a drive the machine does not have");
        }
        Ok(Kind::Lights(lights))
    }

    /// The worker's answer to an insert into `drive`: the drive holds
    /// `medium` once it went in, and forgets it where it did not.
    fn inserted(
        &mut self,
        drive: u8,
        medium: u16,
        outcome: Result<(), InsertRefusal>,
    ) -> Result<Kind, &'static str> {
        let waiting = self
            .inserting
            .get_mut(usize::from(drive))
            .ok_or("a drive the machine does not have")?;
        if *waiting != Some(medium) {
            return Err("an answer to an insert never asked");
        }
        *waiting = None;
        if outcome.is_ok() {
            self.drives[usize::from(drive)] = Some(medium);
        } else {
            self.forget(medium);
        }
        Ok(Kind::Inserted {
            drive,
            medium,
            outcome,
        })
    }

    /// A guest's low-level format of fixed disk `drive`, which can only name
    /// tracks the drive has, each once and in order.
    fn formatted(&self, drive: u8, format: LowLevel) -> Result<Kind, &'static str> {
        let tracks = self
            .tracks
            .get(usize::from(drive))
            .copied()
            .flatten()
            .ok_or("a format of a drive the machine does not have")?;
        let ascending = format.bad_tracks.windows(2).all(|pair| pair[0] < pair[1]);
        if !ascending
            || format.bad_tracks.len() > MAX_TRACKS
            || format.bad_tracks.last().is_some_and(|&t| t >= tracks)
        {
            return Err("a format the drive cannot have");
        }
        Ok(Kind::Formatted { drive, format })
    }

    /// The worker's request for `count` sectors of `medium` from `first`,
    /// which must lie on it.
    fn fetch(
        &mut self,
        request: u32,
        medium: u16,
        first: u64,
        count: u16,
    ) -> Result<Kind, &'static str> {
        self.take_request(request)?;
        let named = self.named(medium)?;
        if count == 0
            || count > MAX_RUN
            || first
                .checked_add(u64::from(count))
                .is_none_or(|end| end > named.form.units())
        {
            return Err("a run past its medium");
        }
        self.asked = Some(Asked::Fetch { request, count });
        Ok(Kind::Fetch {
            medium,
            first,
            count,
        })
    }

    /// The worker's request to write sector `sector` of `medium`, which must
    /// take it.
    fn store(&mut self, request: u32, medium: u16, sector: u64) -> Result<Kind, &'static str> {
        self.take_request(request)?;
        let named = self.named(medium)?;
        if named.form.read_only() || sector >= named.form.units() {
            return Err("a store the medium cannot take");
        }
        self.asked = Some(Asked::Store { request });
        Ok(Kind::Store { medium, sector })
    }

    /// Take the next piece of the snapshot of a machine being suspended.
    fn kept_piece(&mut self, piece: &[u8]) -> Result<(), &'static str> {
        let keeping = self.keeping.as_mut().ok_or("a snapshot never asked for")?;
        if keeping.len() + piece.len() > most_kept() {
            return Err("a snapshot larger than any");
        }
        if keeping.try_reserve(piece.len()).is_err() {
            self.lose("the front-end had no memory for a snapshot");
            return Ok(());
        }
        keeping.extend_from_slice(piece);
        Ok(())
    }

    /// The suspend asked for ended: the snapshot whole, of the length the
    /// worker says, or why it was not kept.
    fn suspended(&mut self, outcome: Result<u32, Unkept>) -> Result<Kind, &'static str> {
        let keeping = self.keeping.take().ok_or("a suspend never asked")?;
        match outcome {
            Ok(len) if usize::try_from(len).ok() == Some(keeping.len()) => {
                self.kept = Some(keeping);
                Ok(Kind::Suspended(Ok(())))
            }
            Ok(_) => Err("a snapshot not the length it was said to be"),
            Err(why) => Ok(Kind::Suspended(Err(why))),
        }
    }

    /// Answer the worker's recall of `len` bytes of its snapshot from
    /// `offset`, which must lie in it.
    fn recall(
        &mut self,
        request: u32,
        offset: u32,
        len: u32,
    ) -> Result<Option<Kind>, &'static str> {
        self.take_request(request)?;
        let kept = self
            .recall
            .take()
            .ok_or("a recall with nothing to resume")?;
        let span = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(len).ok())
            .filter(|&(_, len)| (1..=KEPT_PIECE).contains(&len))
            .and_then(|(offset, len)| Some(offset..offset.checked_add(len)?))
            .filter(|span| span.end <= kept.len());
        let Some(span) = span else {
            return Err("a recall past the snapshot");
        };
        let answered = self.answered(&Command::Recalled {
            request,
            bytes: &kept[span],
        });
        self.recall = Some(kept);
        answered.map(|()| None).or(Ok(None))
    }

    fn take_request(&mut self, request: u32) -> Result<(), &'static str> {
        if self.asked.is_some() {
            return Err("a second request before the first was answered");
        }
        if request != self.next_request {
            return Err("a request out of turn");
        }
        self.next_request = request.wrapping_add(1);
        Ok(())
    }

    fn named(&self, medium: u16) -> Result<Named, &'static str> {
        self.media
            .iter()
            .copied()
            .find(|named| named.id == medium)
            .ok_or("a medium never named")
    }

    fn forget(&mut self, medium: u16) {
        self.media.retain(|named| named.id != medium);
    }

    /// Copy the frame the worker named out of the share, and hand the slot
    /// straight back.
    fn copy_frame(
        &mut self,
        slot: u8,
        width: u32,
        height: u32,
    ) -> Result<Option<Kind>, &'static str> {
        let place = self.layout.slot(slot).ok_or("a slot past the layout")?;
        let bytes = place
            .frame_bytes(width, height)
            .ok_or("a frame larger than its slot")?;
        if self.releases & (1 << slot) != 0 {
            return Err("a slot named again before it was handed back");
        }
        let adapter = place.adapter;
        let frame = &mut self.frames[usize::from(adapter)];
        frame.clear();
        frame.resize(bytes, 0);
        let copied = self
            .session
            .share()
            .is_some_and(|share| share.copy_out(place.offset, frame));
        if !copied {
            return Err("a frame outside the share");
        }
        self.releases |= 1 << slot;
        self.flush();
        Ok(Some(Kind::Frame {
            adapter,
            width,
            height,
        }))
    }

    fn lend(&self, kind: Kind) -> HostEvent<'_> {
        match kind {
            Kind::Ready => HostEvent::Ready,
            Kind::Refused(refusal) => HostEvent::Refused(refusal),
            Kind::Frame {
                adapter,
                width,
                height,
            } => {
                let len = width as usize * height as usize * PIXEL;
                HostEvent::Frame {
                    adapter,
                    width,
                    height,
                    pixels: &self.frames[usize::from(adapter)][..len],
                }
            }
            Kind::OutOfRange(adapter) => HostEvent::OutOfRange { adapter },
            Kind::Audio => HostEvent::Audio(&self.event[1..]),
            Kind::Fetch {
                medium,
                first,
                count,
            } => HostEvent::Fetch {
                medium,
                first,
                count,
            },
            Kind::Store { medium, sector } => {
                let at = self.event.len() - SECTOR;
                let data = self.event[at..].try_into().unwrap_or(&[0; SECTOR]);
                HostEvent::Store {
                    medium,
                    sector,
                    data,
                }
            }
            Kind::Inserted {
                drive,
                medium,
                outcome,
            } => HostEvent::Inserted {
                drive,
                medium,
                outcome,
            },
            Kind::Ejected { drive, medium } => HostEvent::Ejected { drive, medium },
            Kind::Formatted { drive, format } => HostEvent::Formatted { drive, format },
            Kind::Pointing(follows) => HostEvent::Pointing { follows },
            Kind::ClockSet(time) => HostEvent::ClockSet(time),
            Kind::Lights(lights) => HostEvent::Lights(lights),
            Kind::Resumed(resumed) => HostEvent::Resumed(resumed),
            Kind::Suspended(outcome) => HostEvent::Suspended(outcome),
            Kind::Text(outcome) => HostEvent::Text(outcome.map(|(columns, rows)| Text {
                columns,
                rows,
                codes: self.event.get(TEXT_CODES..).unwrap_or_default(),
            })),
            Kind::TextShown(text) => HostEvent::TextShown(text),
            Kind::Folder { slot, op, written } => HostEvent::Folder {
                slot,
                op: match op {
                    // A write's bytes end its frame, as a store's sector does.
                    folder::Op::Write { handle, offset, .. } => folder::Op::Write {
                        handle,
                        offset,
                        bytes: &self.event[self.event.len() - written..],
                    },
                    other => other,
                },
            },
            Kind::Shared { slot, drive } => HostEvent::Shared { slot, drive },
            Kind::Compile {
                request,
                cs,
                ip,
                len,
            } => HostEvent::Compile {
                request,
                model: self.chips.model,
                cs,
                ip,
                bytes: &self.event[self.event.len() - len..],
            },
            Kind::Stopped(reason, ref registers) => HostEvent::Stopped {
                reason,
                registers: Registers::clone(registers),
            },
            Kind::Seen(ref answered) => self.lend_seen(answered),
            Kind::Interpreting => HostEvent::Interpreting,
        }
    }

    /// Credited commands held, waiting for the window.
    #[must_use]
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Read the text adapter 0 shows: [`HostEvent::Text`] answers.
    ///
    /// # Errors
    ///
    /// As [`Self::run`], or [`Refused::Busy`] while a read is unanswered.
    pub fn read_text(&mut self) -> Result<(), Refused> {
        if self.reading {
            return Err(Refused::Busy);
        }
        self.hold(Held::ReadText)?;
        self.reading = true;
        Ok(())
    }

    /// Type `codes` on the keyboard, behind every command already held:
    /// code page 437, each byte one [`typeable`] admits.
    ///
    /// # Errors
    ///
    /// [`Refused::Ended`] once the worker is gone, [`Refused::Busy`] while
    /// text is still being typed, [`Refused::Untypeable`] for text it does
    /// not admit.
    pub fn type_text(&mut self, codes: Vec<u8>) -> Result<(), Refused> {
        if self.ended.is_some() {
            return Err(Refused::Ended);
        }
        if self.typing() {
            return Err(Refused::Busy);
        }
        if codes.is_empty() || codes.len() > MAX_TYPED || !codes.iter().all(|&c| typeable(c)) {
            return Err(Refused::Untypeable);
        }
        self.typing = codes;
        self.flush();
        Ok(())
    }

    /// Whether text is being typed: some waits here, or the worker has some
    /// still to account for.
    #[must_use]
    pub fn typing(&self) -> bool {
        self.typing_sent < self.typing.len() || self.typing_out > 0
    }

    /// Type no more of what was given: what waits here is dropped, and the
    /// worker drops what it holds once the character under way is typed.
    pub fn stop_typing(&mut self) {
        self.forget_typing();
        if self.typing_out > 0 && self.ended.is_none() {
            // A full queue leaves the worker the window's worth to type.
            let _ = self.hold(Held::StopTyping);
        }
    }

    /// Drop the text not yet sent to type.
    fn forget_typing(&mut self) {
        self.typing = Vec::new();
        self.typing_sent = 0;
    }

    /// The worker accounted for `count` characters sent to type.
    fn typed(&mut self, count: u32) -> Result<(), &'static str> {
        let count = usize::try_from(count).map_err(|_| "typed more than it was given")?;
        if count > self.typing_out {
            return Err("typed more than it was given");
        }
        self.typing_out -= count;
        self.flush();
        Ok(())
    }
}

/// An event checked, before what it lends is lent.
enum Kind {
    Ready,
    Refused(Refusal),
    Frame {
        adapter: u8,
        width: u32,
        height: u32,
    },
    OutOfRange(u8),
    Audio,
    Fetch {
        medium: u16,
        first: u64,
        count: u16,
    },
    Store {
        medium: u16,
        sector: u64,
    },
    Inserted {
        drive: u8,
        medium: u16,
        outcome: Result<(), InsertRefusal>,
    },
    Ejected {
        drive: u8,
        medium: u16,
    },
    Formatted {
        drive: u8,
        format: LowLevel,
    },
    Pointing(bool),
    ClockSet(CivilTime),
    Lights(Lights),
    Resumed(bool),
    Suspended(Result<(), Unkept>),
    Text(Result<(u16, u16), Unread>),
    TextShown(bool),
    Folder {
        slot: u8,
        op: folder::Op<'static>,
        written: usize,
    },
    Shared {
        slot: u8,
        drive: Option<u8>,
    },
    Compile {
        request: u32,
        cs: u16,
        ip: u16,
        len: usize,
    },
    Stopped(Reason, Box<Registers>),
    Seen(debugger::Answered),
    Interpreting,
}

/// Where a text event's codes begin: its tag, its outcome, and the screen's
/// columns and rows.
const TEXT_CODES: usize = 6;

#[cfg(test)]
#[path = "front_tests.rs"]
mod tests;

//! The machine worker: the whole machine, run in a capability-empty sandbox
//! and reached only through its session (`plans/PCEMU.md` §3).
//!
//! It runs the machine in slices paced to the host's clock, waiting on its
//! pipe between them no longer than the next slice is due, so a held machine
//! costs nothing and input is taken as soon as it arrives. After each slice it
//! hands the front-end every finished frame, the sound rendered, and any
//! change to a fixed disk's low-level format. Emulated time stops while a disk
//! waits on the front-end, and that wait is not caught up afterwards: the
//! guest never sees how fast the host's storage is.
//!
//! Suspended, it sends the machine whole, compressed; resumed, it recalls
//! that snapshot before it says it is ready, and builds the machine afresh
//! where the snapshot does not take.
//!
//! Its machine runs its hot blocks as translated code, which it cannot make
//! itself: it asks the front-end for each block, and the code it is answered
//! with the front-end installed in it. What it holds is a cache within a
//! share of its room, given up whole when it would pass it.

mod cache;
mod debugger;
mod folder;
mod lamps;
pub mod port;
mod typist;

use crate::board::BoardConfig;
use crate::chips::keyboard::Scan;
use crate::cpu::jit::cache::{Cache, Key};
use crate::cpu::jit::dispatch::{Execute, Jit};
use crate::dos::MouseFollower;
use crate::link::layout::{Layout, MAX_ADAPTERS, MAX_SLOTS};
use crate::link::{
    config, takes_key, Command, Dropped, Event, FixedDisk, Form, InsertRefusal, Installed, Refusal,
    Setup, Text, Unkept, AUDIO_FRAME, COMPILE_WINDOW, KEPT_PIECE, MAX_AUDIO_FRAMES, MAX_RUN,
    MAX_SNAPSHOT, QUEUE_MOST,
};
use crate::machine::{Exit, Machine};
use crate::ports::mouse::HostPointer;
use crate::storage::floppy::Floppy;
use crate::storage::hdc::{DiskType, LowLevel};
use crate::storage::imd::Image;
use crate::storage::SECTOR;
use crate::time::{CpuClock, Nanos, NANOS_PER_MILLISECOND as MS};
use crate::video::raster::Area;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use cache::SectorCache;
use core::cell::RefCell;
use port::{End, Link, Port, RemoteDisk};
use tairix_hash::HashSeed;
use tairix_log::Sink;
use tairix_reclaim::{shrink_target, CacheBudget, PressureBand, PressureGauge, ReclaimClass};
use tairix_sandbox::link::Waited;
use tairix_sandbox::session::ShareWrite;
use tairix_sandbox::wire::Writer;
use typist::Typist;

/// The most emulated time one slice runs, so input is taken between slices
/// however far behind the machine is.
pub const SLICE: Nanos = 4 * MS;

/// How far behind the host's clock the machine runs before a slice is due.
pub const TICK: Nanos = 4 * MS;

/// The most the machine is let fall behind the host's clock. A host too slow
/// to keep up runs the guest slower than real time rather than ever faster.
pub const MAX_LAG: Nanos = 100 * MS;

/// The process's memory-pressure gauge, which the worker cannot read for
/// itself and is told by the front-end.
pub trait Memory {
    /// The gauge the cache shrinks by.
    fn gauge(&self) -> &'static (dyn PressureGauge + 'static);

    /// Publish `band`, which the front-end reported.
    fn report(&self, band: PressureBand);
}

/// Whose a frame slot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotUse {
    /// The worker's to write.
    Free,
    /// Named to the front-end, which has not yet handed it back.
    Lent,
}

/// A key event waiting for room in the keyboard's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyEvent {
    /// A key goes down or comes up.
    Key(Scan, bool),
    /// Every held key comes up.
    ReleaseAll,
}

/// The geometry of an adapter's last finished frame, which the host's pointer
/// is placed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Picture {
    width: u32,
    height: u32,
    displayed: Option<Area>,
}

impl Picture {
    /// Dot `x` of line `y` as fractions of what the card displays, the
    /// border taken as its nearest edge.
    fn pointer_at(self, x: u16, y: u16) -> HostPointer {
        let area = self.displayed.unwrap_or(Area {
            left: 0,
            top: 0,
            right: self.width,
            bottom: self.height,
        });
        let fraction = |at: u16, from: u32, to: u32| {
            let span = to.saturating_sub(from);
            if span <= 1 {
                return 0;
            }
            let along = u32::from(at).clamp(from, to - 1) - from;
            u16::try_from(along * u32::from(u16::MAX) / (span - 1)).unwrap_or(u16::MAX)
        };
        HostPointer {
            x: fraction(x, area.left, area.right),
            y: fraction(y, area.top, area.bottom),
        }
    }
}

/// The last frame an adapter sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sent {
    slot: u8,
    width: u32,
    height: u32,
}

/// A fixed disk's drive and image, for a power cycle to attach again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drive {
    kind: DiskType,
    medium: u16,
    sectors: u64,
    read_only: bool,
}

/// Why a machine was not started.
enum Unstarted {
    /// It could not be built.
    Refused(Refusal),
    /// The front-end named its fixed disks in a way the protocol forbids.
    Violation(&'static str),
}

/// What a running machine holds beside itself.
struct Run {
    machine: Machine,
    config: BoardConfig,
    fixed: [Option<Drive>; 2],
    layout: Layout,
    slots: [SlotUse; MAX_SLOTS],
    sent: [Option<Sent>; MAX_ADAPTERS],
    out_of_range: [bool; MAX_ADAPTERS],
    pictures: [Option<Picture>; MAX_ADAPTERS],
    /// Whether the front-end was last told the mouse's driver follows the
    /// host's pointer.
    follows: bool,
    lamps: lamps::Lamps,
    /// The image in each diskette drive.
    drives: [Option<u16>; 4],
    /// Key events in order, given to the keyboard as fast as its buffer
    /// takes them, so a burst is typed rather than overrun; a guest that
    /// reads no keys fills this, and further presses are dropped.
    keys: VecDeque<KeyEvent>,
    dropped: Dropped,
    /// Text the front-end gave to type.
    typist: Typist,
    /// Whether the front-end was last told adapter 0 shows text.
    text_shown: Option<bool>,
    /// The last low-level format reported for each fixed disk.
    formats: [Option<LowLevel>; 2],
    /// The highest image name the front-end has used.
    last_medium: Option<u16>,
    speed: Option<CpuClock>,
    running: bool,
    /// The host time and emulated time the pacing counts from.
    origin: (Nanos, Nanos),
    audio_target: u32,
    audio: Vec<f32>,
    pcm: Vec<u8>,
    room: u64,
    /// The blocks asked to be compiled and not yet answered, by request.
    asked: Vec<(u32, Key)>,
    asking: Asking,
    debugging: debugger::Debugging,
}

/// Which blocks the worker asks to be compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asking {
    /// Those the machine grows hot on.
    Hot,
    /// None: the front-end said to interpret.
    Stopped,
    /// None: a translated block misbehaved, as the front-end has been told.
    Misbehaved,
}

/// The worker, started.
struct Worker<L, S, M> {
    port: Rc<RefCell<Port<L>>>,
    share: S,
    memory: M,
    run: Run,
    /// What recognises the emulator's own mouse driver; with none the mouse
    /// is always the guest's to read.
    follower: Option<MouseFollower>,
    inbox: Vec<u8>,
    /// Credited commands applied, and the count last reported.
    applied: u32,
    reported: u32,
}

/// Serve the session over `link`, writing frames into `share`, until the
/// front-end closes it or breaks it; answers why it ended.
///
/// The first command must be [`Command::Start`]; a machine that cannot be
/// built is [`Event::Refused`] and the worker ends.
///
/// Its machine runs the blocks `executor` adopts, the code the front-end
/// installs in it.
pub fn serve<L: Link + 'static, S: ShareWrite, M: Memory>(
    link: L,
    share: S,
    (memory, executor): (M, Box<dyn Execute>),
    sink: &'static (dyn Sink + Sync),
) -> End {
    match begin(link, share, (memory, executor), sink) {
        Ok(worker) => worker.serve(),
        Err(end) => end,
    }
}

/// The worker for the start the front-end sends first, its machine built.
fn begin<L: Link + 'static, S: ShareWrite, M: Memory>(
    mut link: L,
    share: S,
    (memory, executor): (M, Box<dyn Execute>),
    sink: &'static (dyn Sink + Sync),
) -> Result<Worker<L, S, M>, End> {
    let first = match link.recv(None, |payload| match Command::decode(payload) {
        Ok(Command::Start(setup)) => Some(setup),
        _ => None,
    }) {
        Ok(Waited::Frame(Some(setup))) => setup,
        Ok(Waited::Frame(None)) => {
            return Err(End::Violation("a first command other than a start"))
        }
        Ok(Waited::Nothing) => return Err(End::Violation("a first command cut short")),
        Ok(Waited::Closed) => return Err(End::Closed),
        Err(error) => return Err(End::Failed(error)),
    };
    Worker::open(link, share, (memory, executor), sink, &first)
}

/// A worker begun over `link`, run a turn at a time by a test that plays the
/// front-end in the same process.
#[cfg(test)]
pub(crate) struct Stepped<L, S, M>(Worker<L, S, M>);

#[cfg(test)]
impl<L: Link + 'static, S: ShareWrite, M: Memory> Stepped<L, S, M> {
    /// The worker for the start the front-end sends first.
    pub(crate) fn begin(
        link: L,
        share: S,
        (memory, executor): (M, Box<dyn Execute>),
        sink: &'static (dyn Sink + Sync),
    ) -> Result<Self, End> {
        begin(link, share, (memory, executor), sink).map(Self)
    }

    /// One turn of the run loop.
    pub(crate) fn turn(&mut self) -> Option<End> {
        self.0.turn()
    }

    /// The machine.
    pub(crate) fn machine(&self) -> &Machine {
        &self.0.run.machine
    }
}

/// The drives of the fixed disks `disks` describes, each image named to
/// `port`, and the last name taken.
fn name_fixed<L: Link>(
    port: &Rc<RefCell<Port<L>>>,
    disks: &[Option<FixedDisk>; 2],
) -> Result<([Option<Drive>; 2], Option<u16>), Unstarted> {
    let mut last_medium: Option<u16> = None;
    let mut fixed = [None, None];
    for (place, disk) in fixed.iter_mut().zip(disks) {
        let Some(disk) = disk else { continue };
        if last_medium.is_some_and(|last| disk.medium <= last) {
            return Err(Unstarted::Violation("two fixed disks named alike"));
        }
        let form = Form::Sectors {
            count: disk.sectors,
            read_only: disk.read_only,
        };
        if !port.borrow_mut().name(disk.medium, form) {
            return Err(Unstarted::Refused(Refusal::Memory));
        }
        last_medium = Some(disk.medium);
        *place = Some(Drive {
            kind: disk.kind,
            medium: disk.medium,
            sectors: disk.sectors,
            read_only: disk.read_only,
        });
    }
    Ok((fixed, last_medium))
}

/// The cache's budget: a share of the media it caches, within the worker's
/// room.
fn budget(media_bytes: u64, room: u64) -> CacheBudget {
    CacheBudget::from_backing(usize::try_from(media_bytes.min(room)).unwrap_or(usize::MAX))
}

fn media_bytes(fixed: &[Option<FixedDisk>; 2]) -> u64 {
    fixed
        .iter()
        .flatten()
        .map(|disk| disk.sectors.saturating_mul(SECTOR as u64))
        .fold(0, u64::saturating_add)
}

/// The mix's samples as 16-bit little-endian PCM, clamped and rounded.
fn to_pcm(samples: &[f32], out: &mut Vec<u8>) {
    out.clear();
    for &sample in samples {
        let level =
            tairix_util::mathf::round_i32(f64::from(sample.clamp(-1.0, 1.0)) * f64::from(i16::MAX));
        out.extend_from_slice(&i16::try_from(level).unwrap_or(0).to_le_bytes());
    }
}

/// A buffer of `len` zeroes, if it can be had.
/// The key queue and the typist, each with its whole bound held.
fn input_queues() -> Option<(VecDeque<KeyEvent>, Typist)> {
    let mut keys = VecDeque::new();
    keys.try_reserve_exact(QUEUE_MOST).ok()?;
    Some((keys, Typist::new().ok()?))
}

fn zeroed<T: Clone + Default>(len: usize) -> Option<Vec<T>> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(len).ok()?;
    buffer.resize(len, T::default());
    Some(buffer)
}

impl<L: Link + 'static, S: ShareWrite, M: Memory> Worker<L, S, M> {
    /// The worker for `setup`, its machine built and [`Event::Ready`] sent,
    /// or why it could not start, [`Event::Refused`] sent where the machine
    /// was refused.
    fn open(
        link: L,
        share: S,
        (memory, executor): (M, Box<dyn Execute>),
        sink: &'static (dyn Sink + Sync),
        setup: &Setup,
    ) -> Result<Self, End> {
        memory.report(setup.band);
        let backing = media_bytes(&setup.fixed);
        let cache = SectorCache::new(
            budget(backing, setup.room),
            HashSeed::from_bytes(setup.seed),
            memory.gauge(),
            sink,
        );
        let port = Rc::new(RefCell::new(
            Port::new(link, cache).ok_or(End::Refused(Refusal::Memory))?,
        ));
        match Self::start(Rc::clone(&port), share, (memory, executor), setup) {
            Ok((worker, resumed)) => {
                if let Some(resumed) = resumed {
                    worker.send(&Event::Resumed(resumed));
                }
                worker.send(&Event::Ready);
                Ok(worker)
            }
            Err(Unstarted::Refused(refusal)) => {
                let mut port = port.borrow_mut();
                port.send(&Event::Refused(refusal));
                port.flush();
                Err(End::Refused(refusal))
            }
            Err(Unstarted::Violation(why)) => Err(End::Violation(why)),
        }
    }

    /// The worker for `setup`, and whether it resumed the snapshot it was
    /// asked to.
    fn start(
        port: Rc<RefCell<Port<L>>>,
        share: S,
        (memory, executor): (M, Box<dyn Execute>),
        setup: &Setup,
    ) -> Result<(Self, Option<bool>), Unstarted> {
        let layout = Layout::of(&setup.config.video);
        if share.byte_len() < layout.len() {
            return Err(Unstarted::Refused(Refusal::Share));
        }
        let (fixed, last_medium) = name_fixed(&port, &setup.fixed)?;
        let formats = setup
            .fixed
            .each_ref()
            .map(|disk| disk.as_ref().map(|disk| disk.format.clone()));
        let mut machine =
            build(&port, &setup.config, &fixed, &formats).map_err(Unstarted::Refused)?;
        let resumed = match setup.resume {
            None => None,
            Some(resume) => {
                let taken = recalled(&port, resume.len).is_some_and(|kept| {
                    fingerprint(&setup.config, &fixed)
                        .is_some_and(|print| restored(&mut machine, &kept, &print))
                });
                if taken {
                    crate::bios::resumed(&mut machine, resume.away);
                    // A snapshot keeps no share: what DOS held of one is gone.
                    crate::dos::redirect::forget(&mut machine);
                    // A speed chosen from the menu was for the run before.
                    if machine.board().cpu_rate() != setup.config.clock.rate() {
                        machine.set_cpu_rate(setup.config.clock);
                    }
                } else {
                    machine = build(&port, &setup.config, &fixed, &formats)
                        .map_err(Unstarted::Refused)?;
                }
                Some(taken)
            }
        };
        let (Some(audio), Some(pcm)) = (
            zeroed(MAX_AUDIO_FRAMES * 2),
            zeroed(MAX_AUDIO_FRAMES * AUDIO_FRAME),
        ) else {
            return Err(Unstarted::Refused(Refusal::Memory));
        };
        let (keys, typist) = input_queues().ok_or(Unstarted::Refused(Refusal::Memory))?;
        let mut asked = Vec::new();
        asked
            .try_reserve_exact(COMPILE_WINDOW)
            .map_err(|_| Unstarted::Refused(Refusal::Memory))?;
        machine.attach(Jit::new(Cache::new(setup.config.cpu), executor));
        let mut run = Run {
            machine,
            config: setup.config,
            fixed,
            layout,
            slots: [SlotUse::Free; MAX_SLOTS],
            sent: [None; MAX_ADAPTERS],
            out_of_range: [false; MAX_ADAPTERS],
            pictures: [None; MAX_ADAPTERS],
            follows: false,
            lamps: lamps::Lamps::default(),
            drives: [None; 4],
            keys,
            dropped: Dropped::default(),
            typist,
            text_shown: None,
            formats,
            last_medium,
            speed: None,
            running: false,
            origin: (0, 0),
            audio_target: setup.audio_target,
            audio,
            pcm,
            room: setup.room,
            asked,
            asking: Asking::Hot,
            debugging: debugger::Debugging::default(),
        };
        if let Some(time) = &setup.clock {
            run.machine.board_mut().set_clock(time);
        }
        let worker = Self {
            port,
            share,
            memory,
            run,
            follower: MouseFollower::new().ok(),
            inbox: Vec::new(),
            applied: 1,
            reported: 0,
        };
        Ok((worker, resumed))
    }

    fn send(&self, event: &Event<'_>) {
        self.port.borrow_mut().send(event);
    }

    fn violation(&self, why: &'static str) {
        self.port.borrow_mut().end(End::Violation(why));
    }

    fn ended(&self) -> Option<End> {
        self.port.borrow().ended()
    }

    fn serve(mut self) -> End {
        loop {
            if let Some(end) = self.turn() {
                return end;
            }
        }
    }

    /// Wait for commands no longer than the next slice is due, apply every
    /// one that came, run the slice, and report the credit; answers why the
    /// session ended, if it did.
    fn turn(&mut self) -> Option<End> {
        // Nothing waits unsent while the worker waits: the front-end may be
        // waiting on it, as it waits on the ready that starts the machine.
        self.port.borrow_mut().flush();
        let mut wait = self.wait();
        while self.port.borrow_mut().next(wait, &mut self.inbox) {
            self.apply_inbox();
            if self.ended().is_some() {
                break;
            }
            wait = Some(0);
        }
        if let Some(end) = self.ended() {
            return Some(end);
        }
        self.feed_keys();
        self.step();
        self.feed_keys();
        if let Some(count) = self.run.typist.take_taken() {
            self.send(&Event::Typed(count));
        }
        self.report_credit();
        self.port.borrow_mut().flush();
        self.ended()
    }

    /// Give the keyboard the waiting key events its buffer has room for, in
    /// order: a release of every key lets them go one at a time. A character
    /// being typed is finished first, and the next is begun only once no key
    /// event waits; nothing is typed into a machine held still.
    fn feed_keys(&mut self) {
        let typing = self.run.running && !self.run.debugging.stopped;
        if typing {
            self.run.typist.feed(&mut self.run.machine, false);
        }
        self.feed_key_events();
        if typing {
            let may_begin = self.run.keys.is_empty();
            self.run.typist.feed(&mut self.run.machine, may_begin);
        }
    }

    fn feed_key_events(&mut self) {
        let board = self.run.machine.board_mut();
        while let Some(&event) = self.run.keys.front() {
            let (key, down) = match event {
                KeyEvent::Key(key, down) => (key, down),
                KeyEvent::ReleaseAll => {
                    let Some(key) = board.held_keys().next() else {
                        self.run.keys.pop_front();
                        continue;
                    };
                    (key, false)
                }
            };
            if !board.key_fits(key, down) {
                return;
            }
            board.key(key, down);
            if matches!(event, KeyEvent::Key(..)) {
                self.run.keys.pop_front();
            }
        }
    }

    /// How long to wait for a command before the next slice: for ever while
    /// the machine is held, or stopped.
    fn wait(&self) -> Option<Nanos> {
        if !self.advancing() {
            return None;
        }
        let behind = self.target().saturating_sub(self.run.machine.now());
        Some(TICK.saturating_sub(behind))
    }

    /// The emulated time the host's clock stands at.
    fn target(&self) -> Nanos {
        let now = self.port.borrow().now();
        let (host, emulated) = self.run.origin;
        emulated.saturating_add(now.saturating_sub(host))
    }

    /// Count emulated time from now.
    fn rebase(&mut self) {
        self.run.origin = (self.port.borrow().now(), self.run.machine.now());
    }

    /// Run one slice if one is due, then hand on what it produced.
    fn step(&mut self) {
        if self.advancing() {
            if self.target().saturating_sub(self.run.machine.now()) > MAX_LAG {
                let now = self.port.borrow().now();
                self.run.origin = (now.saturating_sub(MAX_LAG), self.run.machine.now());
            }
            let emulated = self.run.machine.now();
            let deadline = self.target().min(emulated.saturating_add(SLICE));
            if deadline > emulated
                && crate::bios::run_until(&mut self.run.machine, deadline) == Exit::Break
            {
                self.stopped();
            }
            let waited = self.port.borrow_mut().take_waited();
            self.run.origin.0 = self.run.origin.0.saturating_add(waited);
            self.notice_misbehaviour();
        }
        let (released, fill, band) = self.port.borrow_mut().take_waiting_readings();
        for slot in (0u8..32).filter(|n| released & (1 << n) != 0) {
            self.release(slot);
        }
        if let Some(frames) = fill {
            self.run
                .machine
                .board_mut()
                .steer_audio(frames, self.run.audio_target);
        }
        if let Some(band) = band {
            self.pressure(band);
        }
        self.emit_frames();
        self.emit_text_shown();
        self.emit_audio();
        self.emit_formats();
        self.emit_pointing();
        if let Some(time) = self.run.machine.board_mut().clock_set() {
            self.send(&Event::ClockSet(time));
        }
        let (lights, now) = (self.run.machine.board().lights(), self.run.machine.now());
        if let Some(shown) = self.run.lamps.observe(lights, now) {
            self.send(&Event::Lights(shown));
        }
        self.emit_shares();
        self.ask_for_blocks();
    }

    /// Ask for the blocks the machine has grown hot on, as far as the window
    /// allows and while there is room for their code.
    fn ask_for_blocks(&mut self) {
        if self.run.asking != Asking::Hot || self.code_room() == 0 {
            return;
        }
        let Run { machine, asked, .. } = &mut self.run;
        let Some((jit, _)) = machine.blocks() else {
            return;
        };
        while asked.len() < COMPILE_WINDOW {
            let Some(request) = jit.next_request() else {
                break;
            };
            self.port.borrow_mut().send(&Event::Compile {
                request: request.number,
                cs: request.key.cs,
                ip: request.key.ip,
                bytes: &request.bytes,
            });
            asked.push((request.number, request.key));
        }
    }

    /// Block request `request` answered: its code installed where it still
    /// stands, or the block left to the interpreter.
    fn compiled(&mut self, request: u32, code: Option<Installed>) {
        let Some(at) = self.run.asked.iter().position(|&(n, _)| n == request) else {
            return self.violation("an answer to a block never asked for");
        };
        let (_, key) = self.run.asked.swap_remove(at);
        if let Some((jit, board)) = self.run.machine.blocks() {
            let code = code.map(|code| (code.address, code.len));
            jit.answer((key, request), code, board);
        }
        self.hold_code_to_room();
    }

    /// Tells the front-end, once, that a translated block misbehaved: the
    /// machine runs interpreted and asks for no more, as a compiler that
    /// fails for good would leave it.
    fn notice_misbehaviour(&mut self) {
        if self.run.asking == Asking::Misbehaved {
            return;
        }
        let broken = self
            .run
            .machine
            .blocks()
            .is_some_and(|(jit, _)| jit.is_broken());
        if broken {
            self.interpret();
            self.run.asking = Asking::Misbehaved;
            self.send(&Event::Interpreting);
        }
    }

    /// Ask for no more blocks: those installed run on.
    fn interpret(&mut self) {
        if self.run.asking == Asking::Hot {
            self.run.asking = Asking::Stopped;
        }
        if let Some((jit, board)) = self.run.machine.blocks() {
            jit.stop_requesting(board);
        }
    }

    /// The bytes of translated code the worker may hold at the pressure it
    /// stands at: a share of its room, as for any cache of what can be made
    /// again at a cost.
    fn code_room(&self) -> u64 {
        let budget =
            CacheBudget::from_backing(usize::try_from(self.run.room).unwrap_or(usize::MAX));
        let band = self.memory.gauge().sample();
        let room = shrink_target(band, ReclaimClass::TransformCache, budget);
        u64::try_from(room).unwrap_or(u64::MAX)
    }

    /// Give up every block's code once they take more than their room: the
    /// blocks still hot are asked for again.
    fn hold_code_to_room(&mut self) {
        let room = self.code_room();
        if let Some((jit, board)) = self.run.machine.blocks() {
            if jit.held() > room {
                jit.flush(board);
            }
        }
    }

    /// Tell the front-end each share's drive as it changes.
    /// Share the folder the front-end holds as share `slot`, labelled
    /// `label`: the machine's DOS gives it a drive once its boot has given
    /// its disks theirs.
    fn share(&mut self, slot: u8, label: crate::folder::DosName, writable: bool) {
        let remote = Box::new(folder::RemoteFolder::new(Rc::clone(&self.port), slot));
        let share = crate::folder::Share::new(label, writable, remote);
        if self.run.machine.board_mut().share(slot, share).is_some() {
            self.violation("a folder shared in a slot already sharing one");
            return;
        }
        crate::dos::redirect::attach(&mut self.run.machine);
        self.emit_shares();
    }

    /// Stop sharing share `slot`: its drive goes, and every file open on it
    /// is cut off.
    fn unshare(&mut self, slot: u8) {
        if self.run.machine.board_mut().share_mut(slot).is_none() {
            self.violation("a share stopped that was not shared");
            return;
        }
        crate::dos::redirect::detach(&mut self.run.machine, slot);
        self.run.machine.board_mut().unshare(slot);
    }

    fn emit_shares(&mut self) {
        while let Some((slot, drive)) = crate::dos::redirect::unannounced(&mut self.run.machine) {
            self.send(&Event::Shared { slot, drive });
        }
    }

    /// Tell the front-end when adapter 0 goes between text and graphics.
    fn emit_text_shown(&mut self) {
        let shown = self.run.machine.board().shows_text(0);
        if self.run.text_shown != Some(shown) {
            self.run.text_shown = Some(shown);
            self.send(&Event::TextShown(shown));
        }
    }

    /// Tell the front-end when the guest starts or stops reading its mouse
    /// through the emulator's own driver.
    fn emit_pointing(&mut self) {
        let board = self.run.machine.board_mut();
        let follows = self
            .follower
            .as_ref()
            .is_some_and(|follower| follower.follows(board));
        if follows == self.run.follows {
            return;
        }
        self.run.follows = follows;
        if !follows {
            board.release_pointer();
        }
        self.send(&Event::Pointing { follows });
    }

    fn report_credit(&mut self) {
        if self.applied != self.reported {
            self.reported = self.applied;
            self.send(&Event::Credit {
                applied: self.applied,
            });
        }
    }

    fn apply_inbox(&mut self) {
        let inbox = core::mem::take(&mut self.inbox);
        match Command::decode(&inbox) {
            Ok(command) => {
                if command.credited() {
                    self.applied = self.applied.wrapping_add(1);
                }
                self.apply(&command);
            }
            Err(_) => self.violation("a malformed command"),
        }
        self.inbox = inbox;
    }

    /// Keep `event` for the keyboard. A press the queue has no room for is
    /// dropped with its release, as a keyboard's own buffer overruns.
    fn queue_key(&mut self, event: KeyEvent) {
        let keys = &mut self.run.keys;
        match event {
            KeyEvent::Key(scan, false) if self.run.dropped.release(scan) => {}
            KeyEvent::Key(scan, down) if !takes_key(keys.len(), down) => {
                self.run.dropped.press(scan);
            }
            KeyEvent::ReleaseAll => {
                // What still waits is moot once every key comes up, and a
                // release of every key must never be lost to a full queue.
                if keys.len() == QUEUE_MOST {
                    keys.clear();
                }
                self.run.dropped.clear();
                keys.push_back(event);
            }
            KeyEvent::Key(..) => keys.push_back(event),
        }
    }

    fn apply(&mut self, command: &Command<'_>) {
        match *command {
            Command::Start(_) => self.violation("a second start"),
            Command::Fetched { .. }
            | Command::Stored { .. }
            | Command::Recalled { .. }
            | Command::Foldered { .. } => {
                self.violation("an answer to nothing asked");
            }
            Command::Share {
                slot,
                label,
                writable,
            } => self.share(slot, label, writable),
            Command::Unshare { slot } => self.unshare(slot),
            Command::Suspend => self.suspend(),
            Command::Insert {
                drive,
                medium,
                form,
            } => {
                let outcome = self.insert(drive, medium, form);
                self.send(&Event::Inserted {
                    drive,
                    medium,
                    outcome,
                });
            }
            Command::Eject { drive } => self.eject(drive),
            Command::Run => {
                if !self.run.running {
                    self.run.running = true;
                    self.rebase();
                }
            }
            Command::Hold => self.run.running = false,
            Command::Reset => self.reset(),
            Command::Speed(rate) => {
                self.run.machine.set_cpu_rate(rate);
                self.run.speed = Some(rate);
            }
            Command::Key { scan, down } => self.queue_key(KeyEvent::Key(scan, down)),
            Command::ReleaseKeys => self.queue_key(KeyEvent::ReleaseAll),
            Command::Mouse { dx, dy, buttons } => {
                self.run.machine.board_mut().mouse(dx, dy, buttons);
            }
            Command::Pointer {
                adapter,
                x,
                y,
                buttons,
            } => {
                let picture = self
                    .run
                    .pictures
                    .get(usize::from(adapter))
                    .copied()
                    .flatten();
                if let Some(picture) = picture {
                    let at = picture.pointer_at(x, y);
                    self.run.machine.board_mut().point(at, buttons);
                }
            }
            Command::Sticks(sticks) => {
                self.run.machine.board_mut().set_sticks(sticks);
            }
            Command::Clock(time) => {
                self.run.machine.board_mut().set_clock(&time);
            }
            Command::Released { slot } => self.release(slot),
            Command::Fill { frames } => self
                .run
                .machine
                .board_mut()
                .steer_audio(frames, self.run.audio_target),
            Command::Pressure(band) => self.pressure(band),
            Command::ReadText => {
                let shown = self.run.machine.board().shown_text(0);
                let text = shown.as_ref().map(|shown| Text {
                    columns: shown.columns,
                    rows: shown.rows,
                    codes: &shown.codes,
                });
                self.send(&Event::Text(text.map_err(|why| *why)));
            }
            Command::Type(codes) => {
                if self.run.typist.take(codes).is_err() {
                    self.violation("more to type than the window holds");
                }
            }
            Command::StopTyping => self.run.typist.stop(),
            Command::Compiled { request, code } => self.compiled(request, code),
            Command::Interpret => self.interpret(),
            Command::Debug(ref debug) => self.debug(debug),
        }
    }

    fn pressure(&mut self, band: PressureBand) {
        self.memory.report(band);
        self.port.borrow_mut().cache().enforce_pressure();
        self.hold_code_to_room();
    }

    fn release(&mut self, slot: u8) {
        match self.run.slots.get_mut(usize::from(slot)) {
            Some(held @ SlotUse::Lent) if self.run.layout.slot(slot).is_some() => {
                *held = SlotUse::Free;
            }
            _ => self.violation("a slot released that was not lent"),
        }
    }

    fn insert(&mut self, drive: u8, medium: u16, form: Form) -> Result<(), InsertRefusal> {
        let n = usize::from(drive);
        let fitted = self
            .run
            .config
            .floppy
            .is_some_and(|floppy| floppy.drives.get(n).copied().flatten().is_some());
        if !fitted {
            return Err(InsertRefusal::NoDrive);
        }
        if self.run.last_medium.is_some_and(|last| medium <= last) {
            return Err(InsertRefusal::Medium);
        }
        self.run.last_medium = Some(medium);
        if !self.port.borrow_mut().name(medium, form) {
            return Err(InsertRefusal::Medium);
        }
        let floppy = match form {
            Form::Sectors { count, read_only } => {
                let disk = RemoteDisk::new(Rc::clone(&self.port), medium, count, read_only);
                Floppy::raw(Box::new(disk)).ok_or(InsertRefusal::Format)
            }
            Form::Archive { bytes } => {
                let image = self.read_archive(medium, bytes);
                self.port.borrow_mut().forget(medium);
                image.map(Floppy::Archive)
            }
        };
        let floppy = match floppy {
            Ok(floppy) => floppy,
            Err(refusal) => {
                self.port.borrow_mut().forget(medium);
                return Err(refusal);
            }
        };
        let Ok(old) = self.run.machine.board_mut().insert_floppy(n, floppy) else {
            self.port.borrow_mut().forget(medium);
            return Err(InsertRefusal::NoDrive);
        };
        if let (Some(_), Some(was)) = (old, self.run.drives[n]) {
            self.port.borrow_mut().forget(was);
            self.send(&Event::Ejected { drive, medium: was });
        }
        self.run.drives[n] = Some(medium);
        self.rebudget();
        Ok(())
    }

    /// The archive `medium` names, fetched whole and parsed here, in the
    /// sandbox, as untrusted input.
    fn read_archive(&mut self, medium: u16, bytes: u32) -> Result<Image, InsertRefusal> {
        let len = usize::try_from(bytes).map_err(|_| InsertRefusal::Archive)?;
        let mut whole = Vec::new();
        whole
            .try_reserve_exact(len.next_multiple_of(SECTOR))
            .map_err(|_| InsertRefusal::Archive)?;
        let units = Form::Archive { bytes }.units();
        let mut first = 0;
        while first < units {
            let count = (units - first).min(u64::from(MAX_RUN));
            let mut port = self.port.borrow_mut();
            port.fetch(medium, first, count)
                .map_err(|_| InsertRefusal::Archive)?;
            whole.extend_from_slice(port.landed());
            first += count;
        }
        whole.truncate(len);
        Image::parse(&whole).map_err(|_| InsertRefusal::Archive)
    }

    fn eject(&mut self, drive: u8) {
        let n = usize::from(drive);
        if self.run.machine.board_mut().eject_floppy(n).is_none() {
            return;
        }
        if let Some(medium) = self.run.drives.get_mut(n).and_then(Option::take) {
            self.port.borrow_mut().forget(medium);
            self.rebudget();
            self.send(&Event::Ejected { drive, medium });
        }
    }

    /// Power the machine off and on: the same configuration, the fixed disks
    /// as they are now formatted, the diskettes left in their drives, the
    /// clock card's battery-kept time and the turbo switch where it stood,
    /// and no key waiting for a keyboard that has started afresh.
    fn reset(&mut self) {
        let jit = self.run.machine.detach();
        let old = self.run.machine.board_mut();
        let probe = old.detach_probe();
        let formats = [old.fixed_format(0).cloned(), old.fixed_format(1).cloned()];
        let time = old.clock();
        let mut floppies: [Option<Floppy>; 4] = [None, None, None, None];
        for (n, floppy) in floppies.iter_mut().enumerate() {
            *floppy = old.eject_floppy(n);
        }
        // A share stays shared, and takes its drive again as DOS boots.
        let mut shares: [Option<crate::folder::Share>; crate::folder::SHARES] =
            [const { None }; crate::folder::SHARES];
        for (slot, kept) in (0u8..).zip(shares.iter_mut()) {
            *kept = old.unshare(slot).map(|mut share| {
                share.lose_drive();
                share
            });
        }
        let machine = match build(&self.port, &self.run.config, &self.run.fixed, &formats) {
            Ok(machine) => machine,
            Err(refusal) => {
                let mut port = self.port.borrow_mut();
                port.send(&Event::Refused(refusal));
                port.flush();
                port.end(End::Refused(refusal));
                return;
            }
        };
        self.run.machine = machine;
        if let Some(jit) = jit {
            self.run.machine.attach(jit);
        }
        // The breakpoints outlast the power cycle; a stop does not.
        self.probe_again(probe);
        self.run.keys.clear();
        self.run.dropped.clear();
        self.run.typist.clear();
        let board = self.run.machine.board_mut();
        for (n, floppy) in floppies.into_iter().enumerate() {
            if let Some(floppy) = floppy {
                let _ = board.insert_floppy(n, floppy);
            }
        }
        for (slot, share) in (0u8..).zip(shares) {
            if let Some(share) = share {
                board.share(slot, share);
            }
        }
        if let Some(time) = time {
            board.set_clock(&time);
        }
        if let Some(rate) = self.run.speed {
            self.run.machine.set_cpu_rate(rate);
        }
        self.run.sent = [None; MAX_ADAPTERS];
        self.rebase();
    }

    /// Hand each finished frame that fits its slot, and differs from the last
    /// the adapter sent, to the front-end through a free slot.
    fn emit_frames(&mut self) {
        let Self {
            port, share, run, ..
        } = self;
        for adapter in 0..MAX_ADAPTERS {
            let Some(frame) = run.machine.board_mut().frame(adapter) else {
                continue;
            };
            let (width, height) = (frame.width, frame.height);
            run.pictures[adapter] = Some(Picture {
                width,
                height,
                displayed: frame.displayed,
            });
            let n = u8::try_from(adapter).unwrap_or(u8::MAX);
            let mut lane = run.layout.slots().filter(|(_, slot)| slot.adapter == n);
            let Some((_, bound)) = lane.next() else {
                continue;
            };
            let Some(bytes) = bound.frame_bytes(width, height) else {
                if !core::mem::replace(&mut run.out_of_range[adapter], true) {
                    port.borrow_mut().send(&Event::OutOfRange { adapter: n });
                }
                continue;
            };
            run.out_of_range[adapter] = false;
            let pixels = &frame.pixels[..bytes.min(frame.pixels.len())];
            if let Some(last) =
                run.sent[adapter].filter(|last| (last.width, last.height) == (width, height))
            {
                let offset = run.layout.slot(last.slot).map_or(0, |slot| slot.offset);
                let mut same = false;
                share.write_with(&mut |region| {
                    same = region.get(offset..offset + bytes) == Some(pixels);
                    true
                });
                if same {
                    continue;
                }
            }
            let free = run.layout.slots().find(|(slot, place)| {
                place.adapter == n && run.slots[usize::from(*slot)] == SlotUse::Free
            });
            let Some((slot, place)) = free else { continue };
            let written = share.write_with(&mut |region| match region
                .get_mut(place.offset..place.offset + bytes)
            {
                Some(target) if pixels.len() == bytes => {
                    target.copy_from_slice(pixels);
                    true
                }
                _ => false,
            });
            let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
                continue;
            };
            if written {
                run.slots[usize::from(slot)] = SlotUse::Lent;
                run.sent[adapter] = Some(Sent {
                    slot,
                    width,
                    height,
                });
                port.borrow_mut().send(&Event::Frame {
                    slot,
                    width: w,
                    height: h,
                });
            }
        }
    }

    fn emit_audio(&mut self) {
        let Self { port, run, .. } = self;
        let board = run.machine.board_mut();
        board.render_audio();
        loop {
            let frames = board.take_audio(&mut run.audio);
            if frames == 0 {
                break;
            }
            to_pcm(&run.audio[..frames * 2], &mut run.pcm);
            port.borrow_mut().send(&Event::Audio(&run.pcm));
            if frames < MAX_AUDIO_FRAMES {
                break;
            }
        }
    }

    fn emit_formats(&mut self) {
        for drive in 0..2 {
            let now = self.run.machine.board().fixed_format(drive);
            if now == self.run.formats[drive].as_ref() {
                continue;
            }
            let Some(format) = now.cloned() else { continue };
            self.send(&Event::Formatted {
                drive: u8::try_from(drive).unwrap_or(0),
                format: format.clone(),
            });
            self.run.formats[drive] = Some(format);
        }
    }

    /// Stop the machine and send it whole, compressed, a piece at a time.
    fn suspend(&mut self) {
        self.run.running = false;
        let outcome = self.kept().and_then(|kept| {
            for piece in kept.chunks(KEPT_PIECE) {
                self.send(&Event::Kept(piece));
                self.port.borrow_mut().flush();
            }
            u32::try_from(kept.len()).map_err(|_| Unkept::Size)
        });
        self.send(&Event::Suspended(outcome));
    }

    /// The machine's snapshot, compressed.
    fn kept(&mut self) -> Result<Vec<u8>, Unkept> {
        let print = fingerprint(&self.run.config, &self.run.fixed).ok_or(Unkept::Memory)?;
        let raw = self.run.machine.snapshot(&print).ok_or(Unkept::Memory)?;
        if raw.len() > MAX_SNAPSHOT {
            return Err(Unkept::Size);
        }
        let bound = tairix_compress::max_compressed_len(raw.len());
        let mut kept = Vec::new();
        kept.try_reserve_exact(bound).map_err(|_| Unkept::Memory)?;
        kept.resize(bound, 0);
        let len = tairix_compress::compress(&raw, &mut kept).map_err(|_| Unkept::Size)?;
        kept.truncate(len);
        Ok(kept)
    }

    /// Hold the cache to its share of the media named now.
    fn rebudget(&mut self) {
        let mut port = self.port.borrow_mut();
        let bytes = port.media_bytes();
        port.cache().rebudget(budget(bytes, self.run.room));
    }
}

/// What names the build a snapshot is of: the configuration, and the fixed
/// disks' drives its firmware is built for.
fn fingerprint(config: &BoardConfig, fixed: &[Option<Drive>; 2]) -> Option<Vec<u8>> {
    let mut w = Writer::refusable();
    config::put_config(&mut w, config);
    for drive in fixed {
        match drive {
            None => w.u8(0),
            Some(drive) => {
                w.u8(1);
                w.u16(drive.kind.cylinders);
                w.u8(drive.kind.heads);
                w.u64(drive.kind.track_to_track);
                w.u64(drive.kind.average_seek);
            }
        }
    }
    w.finish()
}

/// The `len` bytes of the snapshot being resumed, recalled a piece at a
/// time; none once the session ends or the heap will not hold them.
fn recalled<L: Link>(port: &Rc<RefCell<Port<L>>>, len: u32) -> Option<Vec<u8>> {
    let mut kept = Vec::new();
    kept.try_reserve_exact(usize::try_from(len).ok()?).ok()?;
    kept.resize(usize::try_from(len).ok()?, 0);
    for (offset, piece) in (0u32..)
        .step_by(KEPT_PIECE)
        .zip(kept.chunks_mut(KEPT_PIECE))
    {
        port.borrow_mut().recall(offset, piece).ok()?;
    }
    Some(kept)
}

/// Whether `kept`, a compressed snapshot of a machine built as `print`
/// says, was taken back into `machine`, which is otherwise left partly
/// restored.
fn restored(machine: &mut Machine, kept: &[u8], print: &[u8]) -> bool {
    let Ok(len) = tairix_compress::decompressed_len(kept) else {
        return false;
    };
    if len > MAX_SNAPSHOT {
        return false;
    }
    let mut raw = Vec::new();
    if raw.try_reserve_exact(len).is_err() {
        return false;
    }
    raw.resize(len, 0);
    tairix_compress::decompress(kept, &mut raw) == Ok(len) && machine.restore(&raw, print).is_ok()
}

/// The machine `config` describes, its fixed disks attached through `port`.
fn build<L: Link + 'static>(
    port: &Rc<RefCell<Port<L>>>,
    config: &BoardConfig,
    fixed: &[Option<Drive>; 2],
    formats: &[Option<LowLevel>; 2],
) -> Result<Machine, Refusal> {
    let kinds = fixed.map(|disk| disk.map(|disk| disk.kind));
    let mut machine = crate::bios::machine(config, kinds).map_err(|error| match error {
        crate::bios::PowerError::Board(error) => Refusal::Board(error),
        _ => Refusal::Firmware,
    })?;
    for (n, (disk, format)) in fixed.iter().zip(formats).enumerate() {
        let (Some(disk), Some(format)) = (disk, format) else {
            continue;
        };
        let device = RemoteDisk::new(Rc::clone(port), disk.medium, disk.sectors, disk.read_only);
        machine
            .board_mut()
            .attach_fixed(n, disk.kind, Box::new(device), format.clone())
            .map_err(|error| Refusal::Fixed(u8::try_from(n).unwrap_or(0), error))?;
    }
    Ok(machine)
}

/// A [`Link`] over the sandbox's timed worker end, measured by `clock`.
pub struct SandboxLink<C> {
    link: tairix_sandbox::WorkerLink<C>,
    clock: fn() -> Nanos,
}

impl<C> SandboxLink<C> {
    /// The link over `link`, its waits measured by `clock`.
    pub const fn new(link: tairix_sandbox::WorkerLink<C>, clock: fn() -> Nanos) -> Self {
        Self { link, clock }
    }
}

impl<C: tairix_sandbox::TimedChannel> Link for SandboxLink<C> {
    fn recv<R>(
        &mut self,
        wait: Option<Nanos>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, tairix_sandbox::ProtoError> {
        self.link.recv(wait, take)
    }

    fn queue(&mut self, payload: &[u8]) -> Result<(), tairix_sandbox::ProtoError> {
        self.link.queue(payload)
    }

    fn flush(&mut self) -> Result<(), tairix_sandbox::ProtoError> {
        self.link.flush()
    }

    fn now(&self) -> Nanos {
        (self.clock)()
    }
}

/// What a test that runs the worker in its own process stands in for it
/// with.
#[cfg(test)]
pub(crate) mod testing {
    use super::Memory;
    use alloc::boxed::Box;
    use tairix_log::DiscardSink;
    use tairix_reclaim::{PressureBand, PressureGauge, ReportedPressure};

    /// Where the worker's cache's refusals go: nowhere, as in the sandbox.
    pub(crate) static DISCARD: DiscardSink = DiscardSink;

    /// A pressure gauge of the test's own.
    pub(crate) struct TestMemory(&'static ReportedPressure);

    impl Memory for TestMemory {
        fn gauge(&self) -> &'static (dyn PressureGauge + 'static) {
            self.0
        }

        fn report(&self, band: PressureBand) {
            self.0.report(band);
        }
    }

    /// A fresh gauge, so no two tests share one.
    pub(crate) fn memory() -> TestMemory {
        TestMemory(Box::leak(Box::new(ReportedPressure::unknown())))
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;

//! The session's worker (`plans/RISCOS-EMULATOR.md` §15.1): the kernel and
//! every context of a session, with nothing but its port to the front end
//! and the damage ring in its share.
//!
//! - **The start.** The front end's first command is the session's
//!   [`Begin`](crate::protocol::Begin); the kernel is made from it through
//!   the [`Session`] the worker's process gives, and the keyboard it names
//!   selected.
//! - **Scheduling.** Each context that can run has a turn of [`TURN`]
//!   instructions in turn, the guest's measure of its time. One waiting on
//!   input or the clock runs again once that comes or its own timed work falls
//!   due, one parked on a relay once its reply comes; the session's timed work
//!   wakes one waiting context to take the clock's interrupt, not every one.
//!   One the front end pauses has no turn until it is let go on. While the
//!   current context cannot be set aside it alone may run. With none to run
//!   the worker waits on its port until the soonest deadline of a context
//!   that may, so an idle session costs nothing. While the front end holds
//!   the session nothing runs, not even a context waiting in place.
//! - **Time.** The guest's clock is the port's since the session began; the
//!   kernel makes up a second of a late clock's work at most and gives up the
//!   rest, never racing it.
//! - **Input** for a context waits in its mailbox until the kernel reads it.
//! - **Screens.** After each turn, and before the kernel waits in place for
//!   input or the clock, the current context's screen is set beside what the
//!   front end was told, and the lines drawn since go into the ring as its
//!   room allows, then the text cursor and the pointer drawn over it. Lines it
//!   had no room for go once the front end gives room back, each context with
//!   lines waiting visited in turn while others run.
//! - **Memory.** After each turn what the context holds of its own, and what
//!   the shared areas hold, are told where they changed.
//! - **Relays** go out as events: one the kernel lets park waits for its
//!   reply with its context parked, unless the context is parked on another,
//!   and any other in place.
//! - **Launches** make a context, its slot and its mailbox, tell the front
//!   end, and run the command line, or the boot sequence, there. A context
//!   ends when its program does, when its command needs no program, or on a
//!   quit, and the front end is told once its end has run, so nothing of it
//!   is shown after; a boot's failures are told before its end.
//! - **Containment.** A command the protocol does not allow, or a session
//!   that fails, ends the worker with why.

mod port;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_collections::{BitVec, OrderedMap};
use tairix_reclaim::PressureBand;
use tairix_riscos_arm::cpu::Unmodelled;
use tairix_riscos_filing::boot::{self, Report};
use tairix_riscos_kernel::context::{ContextError, ContextId};
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::error;
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::{layout, Bits, Own, Shared};
use tairix_riscos_kernel::os::{
    len32, BootError, Config, Ended, Fail, Host, Kernel, Os, Outcome, Regs, Unwind, Wait,
};
use tairix_riscos_kernel::swi::os::{environment, CHANGE_ENVIRONMENT, CLI};
use tairix_riscos_kernel::swi::X;
use tairix_riscos_vdu::display;
use tairix_sandbox::session::ShareWrite;
use tairix_sandbox::{Waited, WorkerPort};

use crate::protocol::{
    Command, Context, Cursor, Ending, Error, Event, Screen, Shape, BOOT, SCREEN_MOST,
};
use crate::ring::{Ring, RECORD_MOST, RING_BYTES};
pub use port::End;
use port::{Kept, Port, Reply};

/// The instructions a context runs in a turn before another may.
pub const TURN: u64 = 1 << 20;

/// What the front end started a session with, as the worker keeps it for the
/// kernel's making.
#[derive(Debug)]
pub struct Started {
    /// The guest's RAM.
    pub ceiling: Ceiling,
    /// The decoded pages each processor keeps.
    pub memo_pages: usize,
    /// The session's zone: the store, read for the session's life, and the
    /// zone's name in it; none for UTC.
    pub zone: Option<(Vec<u8>, String)>,
    /// The TAIRiX desktop's interface text.
    pub font: DesktopFont,
    /// The machine's memory pressure.
    pub pressure: PressureBand,
}

/// Why a session's kernel could not be configured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unconfigured {
    /// What the front end started it with is no session's: why.
    Refused(&'static str),
    /// There was no memory for it.
    NoMemory,
}

/// What a worker's process gives its session beside its port and share.
pub trait Session {
    /// The kernel's configuration for a session started as `started`, its
    /// host `host`.
    ///
    /// # Errors
    ///
    /// [`Unconfigured`]: a zone store the zone reader refuses, or one
    /// without the zone it names; or no memory.
    fn config(&mut self, host: Box<dyn Host>, started: Started) -> Result<Config, Unconfigured>;

    /// The machine's memory pressure moved to `band`, which the session's
    /// caches are then held to.
    fn pressure(&mut self, band: PressureBand);
}

/// How a context stands between its turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    Ready,
    /// Its SWI waits for input, or the clock reaching `until`.
    Input {
        until: Option<u64>,
    },
    /// Its SWI waits for the clock to reach `until`.
    Clock {
        until: u64,
    },
    /// Its SWI waits for a relay's reply.
    Relay,
}

/// A context the worker runs.
struct Live {
    kernel: ContextId,
    standing: Standing,
    /// When its own timed work falls due, as it stood after its last turn.
    due: Option<u64>,
    /// What the front end was last told it holds of its own.
    own: Option<Own>,
    /// It runs the boot sequence, whose failures are owed at its end.
    boot: bool,
}

impl Live {
    /// Whether a turn would take the clock's interrupt for the session and go
    /// back to its wait: not one parked on a relay, whose SWI performed again
    /// would ask again.
    const fn waits_on_the_clock(&self) -> bool {
        matches!(
            self.standing,
            Standing::Input { .. } | Standing::Clock { .. }
        )
    }

    /// When the clock alone lets it run: its wait's end or its own timed
    /// work, for one waiting on the clock.
    fn falls_due(&self) -> Option<u64> {
        let until = match self.standing {
            Standing::Input { until } => until,
            Standing::Clock { until } => Some(until),
            Standing::Ready | Standing::Relay => return None,
        };
        [until, self.due].into_iter().flatten().min()
    }
}

/// Which contexts may have a turn now.
#[derive(Clone, Copy)]
enum Eligible {
    Any,
    /// Only the current, which cannot be set aside.
    Only(ContextId),
}

impl Eligible {
    fn admits(self, live: &Live) -> bool {
        match self {
            Self::Any => true,
            Self::Only(current) => live.kernel == current,
        }
    }
}

/// What the front end has been shown of one context's screen and what is
/// drawn over it.
struct Seen {
    screen: Screen,
    shown: Option<display::Shown>,
    /// The lines drawn and not yet sent.
    pending: BitVec,
    cursor: Option<Cursor>,
    pointer: Option<Pointer>,
    at: Option<(i32, i32)>,
    /// The guest's settings of its pointer, as last counted.
    sets: u32,
    /// Whether its hourglass shows, as last told.
    hourglass: bool,
}

/// A pointer shape as last told.
#[derive(Clone, PartialEq, Eq)]
struct Pointer {
    width: u32,
    height: u32,
    active: (u8, u8),
    colours: [u32; 3],
    pixels: Vec<u8>,
}

/// What the front end has been shown of each context's screen, and the ring
/// the lines cross in: the session's, held for the kernel's waits to reach.
struct Display<P, S> {
    port: Rc<RefCell<Port<P>>>,
    ring: Ring,
    seen: OrderedMap<Context, Seen>,
    /// The context whose waiting lines were last visited.
    visited: Context,
    share: S,
    /// A line's colours as the drivers read them.
    line: Vec<u32>,
    /// A pointer shape's pixels as the drivers read them.
    shape: Vec<u8>,
}

/// The current context's screen shown, as after a turn and before the
/// kernel waits in place for input or the clock.
fn show<P: WorkerPort + 'static, S: ShareWrite + 'static>(os: &mut Os<'_>) {
    os.with_extension::<Display<P, S>, _>(|display, os| display.show(os));
}

/// The text of the error block at `block`: none where it cannot be read.
fn text_of(os: &mut Os<'_>, block: u32) -> Vec<u8> {
    os.read_zstring(block.wrapping_add(4), error::TEXT)
        .unwrap_or_default()
}

/// What the front end is told of a screen, where the protocol can describe
/// it.
fn described(view: &display::View) -> Option<Screen> {
    let sized = |side: u32| (1..=SCREEN_MOST).contains(&side);
    (sized(view.width) && sized(view.height)).then_some(Screen {
        width: view.width,
        height: view.height,
        x_eig: view.x_eig,
        y_eig: view.y_eig,
        border: view.border,
        blanked: view.blanked,
    })
}

/// The text cursor as the front end is told it: none where the protocol
/// cannot place it on `screen`.
fn cursor_on(screen: &Screen, cursor: &display::Cursor) -> Option<Cursor> {
    let (top, bottom) = (*cursor.lines.start(), *cursor.lines.end());
    let fits = cursor.x < screen.width
        && cursor
            .y
            .checked_add(bottom)
            .is_some_and(|y| y < screen.height)
        && cursor.width >= 1
        && cursor.width <= screen.width - cursor.x;
    fits.then_some(Cursor {
        x: cursor.x,
        y: cursor.y,
        width: cursor.width,
        top,
        bottom,
        flash: cursor.flash,
    })
}

impl<P: WorkerPort, S: ShareWrite> Display<P, S> {
    /// The ring's space the front end gave back taken.
    fn take_released(&mut self) {
        let mut port = self.port.borrow_mut();
        let released = core::mem::take(&mut port.released);
        let fits = u32::try_from(released).ok();
        if fits.is_none_or(|bytes| self.ring.release(bytes).is_err()) {
            port.finish(End::Violation("more of the ring given back than was held"));
        }
    }

    fn show(&mut self, os: &mut Os<'_>) {
        self.take_released();
        let context = self.port.borrow().current;
        // One the front end has not been told of, or has been told ended.
        if !self.port.borrow().knows(context) {
            return;
        }
        let Some(screen) = display::view(os).as_ref().and_then(described) else {
            return;
        };
        let shown = display::shown(os);
        let seen = match self.seen.get_mut(&context) {
            Some(seen) if seen.screen == screen => seen,
            _ => {
                let Ok(pending) = BitVec::try_with_len(screen.height as usize, true) else {
                    return;
                };
                // A context's count starts at nought with its mouse, and
                // neither it nor its hourglass is the screen's.
                let (sets, hourglass) = self
                    .seen
                    .get(&context)
                    .map_or((0, false), |seen| (seen.sets, seen.hourglass));
                let fresh = Seen {
                    screen,
                    shown: shown.clone(),
                    pending,
                    cursor: None,
                    pointer: None,
                    at: None,
                    sets,
                    hourglass,
                };
                if self.seen.try_insert(context, fresh).is_err() {
                    return;
                }
                self.port
                    .borrow_mut()
                    .send(&Event::Screen { context, screen });
                let Some(seen) = self.seen.get_mut(&context) else {
                    return;
                };
                seen
            }
        };
        if seen.shown != shown {
            seen.shown = shown;
            seen.pending.fill(true);
        }
        display::damage(os, |first, last| {
            seen.pending
                .set_range(first as usize..last as usize + 1, true);
        });
        let line_bytes = screen.width * 4;
        let per_record = (RECORD_MOST / line_bytes).max(1);
        let mut from = 0;
        while let Some(first) = seen.pending.next_one(from) {
            let end = seen.pending.next_zero(first).unwrap_or(seen.pending.len());
            let lines = len32(end - first).min(per_record);
            let Some(placed) = self.ring.place(lines * line_bytes) else {
                break;
            };
            let first = len32(first);
            let record = Record {
                lines: first..first + lines,
                width: screen.width,
                offset: placed.offset,
            };
            if !write_lines(&mut self.share, &mut self.line, os, &record) {
                break;
            }
            self.ring.take(placed);
            seen.pending
                .set_range(first as usize..(first + lines) as usize, false);
            self.port.borrow_mut().send(&Event::Damage {
                context,
                first,
                lines,
                offset: placed.offset,
            });
            from = (first + lines) as usize;
        }
        show_over(seen, &mut self.shape, &self.port, os, context);
        self.port.borrow_mut().flush();
    }

    /// The next context after the last visited whose lines wait, while the
    /// ring has room for them.
    fn next_waiting(&mut self) -> Option<Context> {
        self.take_released();
        // A visit costs a switch, so it waits for room enough to send much.
        if self.ring.held() >= RING_BYTES / 2 {
            return None;
        }
        let after = self.visited.wrapping_add(1);
        let next = self
            .seen
            .range(after..)
            .chain(self.seen.range(..after))
            .find(|(_, seen)| seen.pending.next_one(0).is_some())
            .map(|(&context, _)| context)?;
        self.visited = next;
        Some(next)
    }
}

/// What is drawn over `context`'s screen told where it changed: the text
/// cursor, the pointer's shape and place, the guest setting its pointer
/// itself, and its hourglass.
fn show_over<P: WorkerPort>(
    seen: &mut Seen,
    shape: &mut Vec<u8>,
    port: &Rc<RefCell<Port<P>>>,
    os: &mut Os<'_>,
    context: Context,
) {
    let cursor = display::cursor(os).and_then(|cursor| cursor_on(&seen.screen, &cursor));
    if seen.cursor != cursor {
        seen.cursor = cursor;
        port.borrow_mut().send(&Event::Cursor { context, cursor });
    }
    if let Ok(view) = display::pointer(os, shape) {
        let pointer = view.map(|view| Pointer {
            width: view.width,
            height: view.height,
            active: (
                u8::try_from(view.active.0).unwrap_or(u8::MAX),
                u8::try_from(view.active.1).unwrap_or(u8::MAX),
            ),
            colours: view.colours,
            pixels: core::mem::take(shape),
        });
        if seen.pointer != pointer {
            let told = pointer.as_ref().map(|p| Shape {
                width: p.width,
                height: p.height,
                active: p.active,
                colours: p.colours,
                pixels: &p.pixels,
            });
            port.borrow_mut().send(&Event::Pointer {
                context,
                shape: told,
            });
            seen.pointer = pointer;
        } else if let Some(kept) = pointer {
            *shape = kept.pixels;
        }
        let at = view.map(|view| view.at);
        if let Some((x, y)) = at.filter(|&at| seen.at != Some(at)) {
            port.borrow_mut().send(&Event::PointerAt { context, x, y });
        }
        seen.at = at;
    }
    let sets = os.pointer_sets();
    if seen.sets != sets {
        seen.sets = sets;
        port.borrow_mut().send(&Event::PointerSet { context });
    }
    let hourglass = display::hourglass(os);
    if seen.hourglass != hourglass {
        seen.hourglass = hourglass;
        port.borrow_mut().send(&Event::Hourglass {
            context,
            shown: hourglass,
        });
    }
}

/// A damage record: its lines of the current screen, how wide they are, and
/// where in the share it goes.
struct Record {
    lines: core::ops::Range<u32>,
    width: u32,
    offset: u32,
}

/// `record`'s lines, each its pixels' colours, written into `share`: false
/// where one could not be read, or is not the width the record was placed
/// for, so that nothing is written past it.
fn write_lines<S: ShareWrite>(
    share: &mut S,
    line: &mut Vec<u32>,
    os: &mut Os<'_>,
    record: &Record,
) -> bool {
    let bytes = record.width as usize * 4;
    share.write_with(&mut |region| {
        let mut at = record.offset as usize;
        for row in record.lines.clone() {
            if display::line(os, row, line).is_err() || line.len() != record.width as usize {
                return false;
            }
            let Some(place) = region.get_mut(at..at + bytes) else {
                return false;
            };
            for (word, colour) in place.as_chunks_mut::<4>().0.iter_mut().zip(line.iter()) {
                *word = colour.to_le_bytes();
            }
            at += bytes;
        }
        true
    })
}

/// What a launch runs once its context is made.
enum Launching<'a> {
    Line(&'a [u8]),
    Boot,
}

/// The worker.
struct Worker<P: WorkerPort, S> {
    kernel: Kernel,
    port: Rc<RefCell<Port<P>>>,
    session: Box<dyn Session>,
    /// The contexts launched and not yet ended.
    contexts: OrderedMap<Context, Live>,
    /// The boot's context, which every ended one gives way to.
    boot: ContextId,
    /// The number the next context made is given.
    next: Context,
    /// The context that had the last turn.
    last: Context,
    /// When the session's timed work falls due, as it stood after the last
    /// turn or launch.
    session_due: Option<u64>,
    slot_bytes: u32,
    /// The pages in use when the shared areas were last looked at, and what
    /// the front end was last told they hold.
    in_use: u32,
    shared: Option<Shared>,
    share: core::marker::PhantomData<S>,
}

/// What the first command started the session with: the start itself, and
/// what of it the worker uses beside the kernel's making.
struct Begun {
    started: Started,
    slot_bytes: u32,
    real_time: u64,
    keyboard: u32,
}

/// The session's start, which the front end's first command must be.
fn begun<P: WorkerPort>(port: &mut P) -> Result<Begun, End> {
    let taken = port.recv(None, |payload| match Command::decode(payload) {
        Ok(Command::Begin(begin)) => {
            let zone = match begin.zone {
                Some(zone) => match (copied(zone.store), copied(zone.name.as_bytes())) {
                    (Some(store), Some(name)) => match String::from_utf8(name) {
                        Ok(name) => Some((store, name)),
                        Err(_) => return Err(End::Violation("a zone named in no text")),
                    },
                    _ => return Err(End::NoMemory),
                },
                None => None,
            };
            let ceiling = Ceiling::of_pages(begin.ram_pages)
                .ok_or(End::Violation("more RAM than a guest can be shown"))?;
            Ok(Begun {
                started: Started {
                    ceiling,
                    memo_pages: begin.memo_pages as usize,
                    zone,
                    font: begin.font,
                    pressure: begin.pressure,
                },
                slot_bytes: begin.slot_bytes,
                real_time: begin.real_time,
                keyboard: begin.keyboard,
            })
        }
        Ok(_) => Err(End::Violation("a session not started first")),
        Err(_) => Err(End::Violation("a command that does not decode")),
    });
    match taken {
        Ok(Waited::Frame(begun)) => begun,
        Ok(Waited::Nothing | Waited::Closed) => Err(End::Closed),
        Err(error) => Err(End::Failed(error)),
    }
}

/// `bytes` copied into memory of the worker's own: none where there is no
/// room for them.
fn copied(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).ok()?;
    owned.extend_from_slice(bytes);
    Some(owned)
}

/// The session served over `port`, its screens' lines in `share`: the
/// kernel made through `session` once the front end has started it, then
/// every command answered until the session ends.
pub fn serve<P, S>(mut port: P, share: S, mut session: impl Session + 'static) -> End
where
    P: WorkerPort + 'static,
    S: ShareWrite + 'static,
{
    let origin = port.now();
    let begun = match begun(&mut port) {
        Ok(begun) => begun,
        Err(end) => return end,
    };
    let Some(port) = Port::new(port, origin, begun.real_time) else {
        return End::Refused(BootError::NoMemory);
    };
    let port = Rc::new(RefCell::new(port));
    let mut made = match session.config(port::host(&port), begun.started) {
        Ok(made) => made,
        Err(Unconfigured::Refused(why)) => return End::Violation(why),
        Err(Unconfigured::NoMemory) => return End::NoMemory,
    };
    made.show = Some(show::<P, S>);
    let mut kernel = match Kernel::new(made) {
        Ok(kernel) => kernel,
        Err(error) => return End::Refused(error),
    };
    if kernel.os().select_keyboard(begun.keyboard).is_err() {
        return End::Fault;
    }
    let display = Display {
        port: Rc::clone(&port),
        ring: Ring::new(),
        seen: OrderedMap::new(),
        visited: BOOT,
        share,
        line: Vec::new(),
        shape: Vec::new(),
    };
    if kernel.os().hold_extension(display).is_err() {
        return End::Refused(BootError::NoMemory);
    }
    let boot = kernel.os().context();
    let session_due = kernel.os().deadlines().session;
    let worker = Worker {
        kernel,
        port,
        session: Box::new(session),
        contexts: OrderedMap::new(),
        boot,
        next: BOOT + 1,
        last: BOOT,
        session_due,
        slot_bytes: begun.slot_bytes,
        in_use: u32::MAX,
        shared: None,
        share: core::marker::PhantomData::<S>,
    };
    worker.serve()
}

impl<P: WorkerPort + 'static, S: ShareWrite + 'static> Worker<P, S> {
    fn serve(mut self) -> End {
        loop {
            if let Some(end) = self.port.borrow().end {
                return end;
            }
            self.port.borrow_mut().take_commands(Some(0));
            self.take_moved();
            self.apply_kept();
            let shown = self.show_waiting();
            match self.next_turn() {
                Some(context) => self.turn(context),
                None if !shown => self.wait(),
                None => {}
            }
            self.port.borrow_mut().flush();
        }
    }

    fn end(&self, why: End) {
        self.port.borrow_mut().finish(why);
    }

    fn running(&self) -> bool {
        self.port.borrow().running
    }

    /// The machine's memory pressure and the desktop's text taken where they
    /// moved: the session's caches held to the band, a `Desktop` font found
    /// from now on set in the text.
    fn take_moved(&mut self) {
        let (pressure, font) = {
            let mut port = self.port.borrow_mut();
            (port.pressure.take(), port.font.take())
        };
        if let Some(band) = pressure {
            self.session.pressure(band);
            self.kernel.os().pressure_moved();
        }
        if let Some(font) = font {
            self.kernel.os().set_desktop_font(font);
        }
    }

    /// The commands kept applied, as far as the session allows: a launch
    /// waits while the session is held, and a launch or a quit while the
    /// current context cannot be set aside. Those that wait keep their
    /// order.
    fn apply_kept(&mut self) {
        let kept = self.port.borrow().kept.len();
        for _ in 0..kept {
            if self.port.borrow().end.is_some() {
                return;
            }
            let Some(command) = self.port.borrow_mut().kept.pop_front() else {
                return;
            };
            let running = self.running();
            let applied = match command {
                Kept::Launch { serial, ref line } => {
                    running && self.launch(serial, &Launching::Line(line))
                }
                Kept::Boot { serial } => running && self.launch(serial, &Launching::Boot),
                Kept::Quit { context } => self.quit(context),
            };
            if !applied {
                self.port.borrow_mut().kept.push_back(command);
            }
        }
    }

    /// Whether `live` can run now, the clock at `now`, for what it waits on
    /// itself.
    fn can_run(&self, context: Context, live: &Live, now: u64) -> bool {
        let port = self.port.borrow();
        match live.standing {
            Standing::Ready => true,
            Standing::Relay => port.answered(context),
            Standing::Input { .. } if port.input_waits(context) => true,
            Standing::Input { .. } | Standing::Clock { .. } => {
                live.falls_due().is_some_and(|at| at <= now)
            }
        }
    }

    /// Which contexts may have a turn now: every one, or the current alone
    /// while it cannot be set aside.
    fn eligible(&mut self) -> Eligible {
        let mut os = self.kernel.os();
        if os.switchable() {
            Eligible::Any
        } else {
            Eligible::Only(os.context())
        }
    }

    /// The contexts that may have a turn, in turn after the one that had the
    /// last: none the front end pauses.
    fn in_turn(&self, eligible: Eligible) -> impl Iterator<Item = (&Context, &Live)> {
        let after = self.last.wrapping_add(1);
        let port = Rc::clone(&self.port);
        self.contexts
            .range(after..)
            .chain(self.contexts.range(..after))
            .filter(move |(&context, live)| eligible.admits(live) && !port.borrow().paused(context))
    }

    /// The next context to have a turn: one that can run, after the last in
    /// turn, else one to take the clock's interrupt where the session's
    /// timed work is due, the current one where it can, saving a switch.
    fn next_turn(&mut self) -> Option<Context> {
        if !self.running() {
            return None;
        }
        let now = self.port.borrow().monotonic();
        let eligible = self.eligible();
        if let Some((&context, _)) = self
            .in_turn(eligible)
            .find(|(&context, live)| self.can_run(context, live, now))
        {
            return Some(context);
        }
        if self.session_due.is_none_or(|at| at > now) {
            return None;
        }
        let current = self.kernel.os().context();
        self.in_turn(eligible)
            .filter(|(_, live)| live.waits_on_the_clock())
            .min_by_key(|(_, live)| live.kernel != current)
            .map(|(&context, _)| context)
    }

    /// The session ended for the kernel's refusal, `error`, to switch or end
    /// a context — unless it is only busy, which passes.
    fn refused_switch(&self, error: ContextError) {
        match error {
            ContextError::Busy => {}
            ContextError::NoMemory => self.end(End::NoMemory),
            _ => self.end(End::Fault),
        }
    }

    /// `context` made current, the reply it is parked on given to the kernel
    /// where it has come: it is ready to run then, whatever it was entered
    /// for.
    fn enter(&mut self, context: Context) -> bool {
        let Some(kernel) = self.contexts.get(&context).map(|live| live.kernel) else {
            return false;
        };
        if self.kernel.os().context() != kernel {
            if let Err(error) = self.kernel.os().switch_context(kernel) {
                self.refused_switch(error);
                return false;
            }
        }
        self.port.borrow_mut().current = context;
        let reply = self.port.borrow_mut().reply(context);
        let settled = match reply {
            Some(Reply::Answered(reply)) => self.kernel.os().relay_answered(&reply),
            Some(Reply::Failed) => self.kernel.os().relay_failed(),
            None => false,
        };
        if let Some(live) = self.contexts.get_mut(&context).filter(|_| settled) {
            live.standing = Standing::Ready;
        }
        true
    }

    fn turn(&mut self, context: Context) {
        if !self.enter(context) {
            return;
        }
        self.last = context;
        let outcome = self.kernel.run(TURN);
        let parked = self.port.borrow_mut().parked.take();
        if let Some(serial) = parked.filter(|_| !matches!(outcome, Outcome::Waiting(Wait::Relay))) {
            // The kernel went on without parking on it, so its answer is not
            // the one a later park waits for.
            self.port.borrow_mut().abandon(serial);
        }
        let standing = match outcome {
            Outcome::Budget => Standing::Ready,
            Outcome::Waiting(Wait::Input { until }) => Standing::Input { until },
            Outcome::Waiting(Wait::Clock { until }) => Standing::Clock { until },
            Outcome::Waiting(Wait::Relay) => Standing::Relay,
            Outcome::Stopped => return,
            Outcome::Ended(ended) => {
                self.program_ended(context, ended);
                return;
            }
            Outcome::Unmodelled(unmodelled) => {
                self.unmodelled(context, unmodelled);
                return;
            }
        };
        let deadlines = self.kernel.os().deadlines();
        self.session_due = deadlines.session;
        if let Some(live) = self.contexts.get_mut(&context) {
            live.standing = standing;
            live.due = deadlines.own;
        }
        show::<P, S>(&mut self.kernel.os());
        self.tell_memory(context);
    }

    /// What the current context, `context`, holds of its own and what the
    /// shared areas hold, each told where it changed since last told.
    fn tell_memory(&mut self, context: Context) {
        let memory = self.kernel.memory();
        let own = memory.own();
        let in_use = memory.pages_in_use();
        let shared = (in_use != self.in_use).then(|| memory.shared());
        if let Some(live) = self.contexts.get_mut(&context) {
            if live.own != Some(own) {
                live.own = Some(own);
                self.port.borrow_mut().send(&Event::Own {
                    context,
                    slot: own.slot,
                    screen: own.screen,
                });
            }
        }
        self.in_use = in_use;
        if let Some(shared) = shared.filter(|&shared| self.shared != Some(shared)) {
            self.shared = Some(shared);
            self.port.borrow_mut().send(&Event::Shared(shared));
        }
    }

    /// The current context, `context`, whose program `ended`: its screen
    /// shown a last time and its error's text read while they are there,
    /// then ended.
    fn program_ended(&mut self, context: Context, ended: Ended) {
        show::<P, S>(&mut self.kernel.os());
        let text = match ended {
            Ended::Error { block, .. } => text_of(&mut self.kernel.os(), block),
            _ => Vec::new(),
        };
        self.ended(context, Ending::of(ended, &text));
    }

    /// The current context, `context`, stopped at what the processor does
    /// not model: its screen shown a last time, then ended.
    fn unmodelled(&mut self, context: Context, unmodelled: Unmodelled) {
        show::<P, S>(&mut self.kernel.os());
        self.ended(context, Ending::Unmodelled(unmodelled));
    }

    /// The failures `report` kept, and the count it did not keep, told as
    /// the boot `context`'s.
    fn tell_report(&self, context: Context, report: &Report) {
        let mut port = self.port.borrow_mut();
        for failure in &report.failures {
            port.send(&Event::BootFailed {
                context,
                line: &failure.line,
                error: Error {
                    number: failure.number,
                    text: &failure.text,
                },
            });
        }
        if report.omitted > 0 {
            port.send(&Event::BootOmitted {
                context,
                count: report.omitted,
            });
        }
    }

    /// The current context, `context`, ended: a boot's failures told first,
    /// then its end run, then the front end told, so that nothing the end
    /// does is shown after it.
    fn ended(&mut self, context: Context, ending: Ending<'_>) {
        if self.contexts.get(&context).is_some_and(|live| live.boot) {
            let report = boot::reported(&mut self.kernel.os());
            self.tell_report(context, &report);
        }
        if let Err(error) = self.kernel.os().end_context(self.boot) {
            self.refused_switch(error);
            return;
        }
        self.session_due = self.kernel.os().deadlines().session;
        self.port
            .borrow_mut()
            .send(&Event::Ended { context, ending });
        self.forget(context);
    }

    /// `context`'s mailbox, screen and standing let go.
    fn forget(&mut self, context: Context) {
        let mut port = self.port.borrow_mut();
        port.close(context);
        if port.current == context {
            port.current = BOOT;
        }
        drop(port);
        self.kernel
            .os()
            .with_extension::<Display<P, S>, _>(|display, _| display.seen.remove(&context));
        self.contexts.remove(&context);
    }

    /// Launch `serial` made, running what `launching` names: false while the
    /// current context cannot be set aside for it.
    fn launch(&mut self, serial: u32, launching: &Launching<'_>) -> bool {
        let refused = |worker: &Self, error: Option<Error<'_>>| {
            worker
                .port
                .borrow_mut()
                .send(&Event::Refused { serial, error });
        };
        let context = self.next;
        let Some(next) = context.checked_add(1) else {
            refused(self, None);
            return true;
        };
        let kernel = match self.kernel.os().launch_context() {
            Ok(kernel) => kernel,
            Err(ContextError::Busy) => return false,
            Err(ContextError::Start(number)) => {
                refused(self, number.map(|number| Error { number, text: b"" }));
                return true;
            }
            Err(ContextError::NoMemory) => {
                refused(self, None);
                return true;
            }
            Err(_) => {
                self.end(End::Fault);
                return true;
            }
        };
        self.next = next;
        // Until the front end is told of it, what its making does is the
        // session's own.
        self.port.borrow_mut().current = BOOT;
        let ready = Live {
            kernel,
            standing: Standing::Ready,
            due: None,
            own: None,
            boot: matches!(launching, Launching::Boot),
        };
        // Its mailbox last: a context the front end has not been told of has
        // none, so nothing of it is shown.
        let made = self.contexts.try_insert(context, ready).is_ok()
            && self.give_slot()
            && self.port.borrow_mut().open(context);
        if !made {
            refused(self, None);
            self.forget(context);
            if let Err(error) = self.kernel.os().end_context(self.boot) {
                self.refused_switch(error);
            }
            return true;
        }
        self.port.borrow_mut().current = context;
        self.port
            .borrow_mut()
            .send(&Event::Launched { serial, context });
        let ran = match *launching {
            Launching::Line(line) => self.command(line),
            Launching::Boot => self.boot(context),
        };
        self.session_due = self.kernel.os().deadlines().session;
        match ran {
            Ok(()) => self.program_ended(context, Ended::Exit { code: 0 }),
            Err(Fail::Error(block)) => {
                let number = self.kernel.os().read_word(block).unwrap_or(0);
                self.program_ended(context, Ended::Error { number, block });
            }
            Err(Fail::Unwind(Unwind::Ended(ended))) => self.program_ended(context, ended),
            Err(Fail::Unwind(Unwind::Unmodelled(unmodelled))) => {
                self.unmodelled(context, unmodelled);
            }
            // A command line run from here is never left to be performed again.
            Err(Fail::Unwind(Unwind::Wait(_))) => self.program_ended(context, Ended::Fault),
            Err(Fail::Unwind(Unwind::Transfer | Unwind::Stopped)) => {}
        }
        true
    }

    /// The boot sequence run in the current context, `context`: where it
    /// finishes at once, its failures told then, none owed at its end.
    fn boot(&mut self, context: Context) -> Result<(), Fail> {
        let report = boot::start(&mut self.kernel.os())?;
        self.tell_report(context, &report);
        if let Some(live) = self.contexts.get_mut(&context) {
            live.boot = false;
        }
        Ok(())
    }

    /// The current context given a slot of the worker's size, its memory
    /// limit at its top: false where there is no memory for one.
    fn give_slot(&mut self) -> bool {
        let memory = self.kernel.memory();
        let Ok(slot) = memory.create_slot(Bits::Bit32) else {
            return false;
        };
        if memory.set_slot_size(slot, self.slot_bytes).is_err() || memory.switch_slot(slot).is_err()
        {
            // Never current, so the context's end would not find it, and its
            // destruction cannot be refused.
            let _ = memory.destroy_slot(slot);
            return false;
        }
        let mut regs = Regs::default();
        regs.r[0] = environment::MEMORY_LIMIT;
        regs.r[1] = layout::APPLICATION + self.slot_bytes;
        self.kernel
            .os()
            .perform(X | CHANGE_ENVIRONMENT, &mut regs)
            .is_ok()
    }

    /// `line` run as a command in the current context.
    fn command(&mut self, line: &[u8]) -> Result<(), Fail> {
        let mut os = self.kernel.os();
        let at = os.rma_claim(len32(line.len()) + 1)?;
        let ran = os
            .write_terminated(at, line, b'\r', line.len() + 1)
            .and_then(|()| {
                let mut regs = Regs::default();
                regs.r[0] = at;
                os.perform(X | CLI, &mut regs)
            });
        os.free_rma_quietly(at);
        ran
    }

    /// `context`, which the port knows was made and has not ended, quit:
    /// false while the current context cannot be set aside for it.
    fn quit(&mut self, context: Context) -> bool {
        if !self.enter(context) {
            return self.port.borrow().end.is_some() || !self.contexts.contains_key(&context);
        }
        self.ended(context, Ending::Quit);
        true
    }

    /// The next context whose lines wait to be sent, visited and shown while
    /// the ring has room: whether any of its lines went.
    fn show_waiting(&mut self) -> bool {
        let waiting = self
            .kernel
            .os()
            .with_extension::<Display<P, S>, _>(|display, _| display.next_waiting());
        let Some(Some(context)) = waiting else {
            return false;
        };
        if !self.enter(context) {
            return false;
        }
        self.kernel
            .os()
            .with_extension::<Display<P, S>, _>(|display, os| {
                let had = display.ring.held();
                display.show(os);
                display.ring.held() != had
            })
            .unwrap_or(false)
    }

    /// Nothing to run: the port waited on until the soonest time the clock
    /// alone would let a context have a turn — its wait's end, its own timed
    /// work, the session's where it may take it — or a command. The same
    /// contexts are counted as [`Self::next_turn`] would choose from, so a
    /// deadline passing always gives one a turn.
    fn wait(&mut self) {
        let soonest = if self.running() {
            let eligible = self.eligible();
            let mut takers = false;
            let mut soonest = None;
            for (_, live) in self.in_turn(eligible) {
                takers |= live.waits_on_the_clock();
                soonest = [soonest, live.falls_due()].into_iter().flatten().min();
            }
            [soonest, self.session_due.filter(|_| takers)]
                .into_iter()
                .flatten()
                .min()
        } else {
            None
        };
        let mut port = self.port.borrow_mut();
        let until = soonest.map(|at| port.instant(at));
        port.flush();
        port.take_commands(until);
    }
}

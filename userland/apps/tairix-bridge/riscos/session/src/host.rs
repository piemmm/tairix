//! The front end's side of one session's worker (`plans/RISCOS-EMULATOR.md`
//! §15.3): the session started, every command sent only where the model
//! lets it go, every event believed through [`Front`], each damage record
//! copied out of the ring into the host's own memory and the ring given back
//! at once.
//!
//! The host never blocks. Its owner registers [`Host::descriptors`] on its
//! wait-set as [`Host::wants_read`] and [`Host::wants_write`] say, calls
//! [`Host::on_readable`] and [`Host::on_writable`] when they fire, and drains
//! [`Host::told`]. A command the session's queue has no room for waits here,
//! in order, until it has.
//!
//! Input past a context's credit waits here too, in order: motion run
//! together, a pointer's place moved on, and past [`HELD_MOST`] a key pressed
//! dropped with its release, as a keyboard's own buffer overruns.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::OrderedMap;
use tairix_log::Sink;
use tairix_reclaim::PressureBand;
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::input::InputEvent;
use tairix_sandbox::host::WorkerEnd;
use tairix_sandbox::session::{
    SandboxSession, SessionBounds, SessionDescriptors, SessionError, SessionTransport, Share,
};
use tairix_sandbox::wire::Writer;

use crate::front::{Front, Unbelievable, Why};
use crate::protocol::{Begin, Command, Context, Event, Screen, Stopped, FRAME_MOST};
use crate::ring::{RECORD_MOST, RING_BYTES};

/// The most input events held for a context beyond its credit.
pub const HELD_MOST: usize = 256;

/// What the worker told, believed: the event, and for damage its pixels,
/// copied out of the ring, each `&RRGGBB` as four little-endian bytes.
#[derive(Debug)]
pub struct Told<'a> {
    /// The event.
    pub event: Event<'a>,
    /// A damage record's pixels; empty for any other event.
    pub pixels: &'a [u8],
}

/// Why a session could not be started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unstarted {
    /// The sandbox would not admit its worker.
    Session(SessionError),
    /// The worker was handed no damage ring of the ring's size.
    Share,
    /// There was no memory for what the session needs.
    NoMemory,
}

/// Input held for one context past its credit, and the keys whose press it
/// dropped, whose release it drops too.
#[derive(Default)]
struct Held {
    events: VecDeque<InputEvent>,
    dropped: u128,
}

impl Held {
    /// `event` held behind the rest: whether it is held, or run together
    /// with the one before, rather than dropped.
    fn hold(&mut self, event: InputEvent) -> bool {
        match (self.events.back_mut(), event) {
            (
                Some(InputEvent::Motion { dx, dy, buttons }),
                InputEvent::Motion {
                    dx: more_x,
                    dy: more_y,
                    buttons: now,
                },
            ) if *buttons == now => {
                *dx = dx.saturating_add(more_x);
                *dy = dy.saturating_add(more_y);
                return true;
            }
            (Some(held), InputEvent::Pointer { buttons: now, .. }) if matches!(*held, InputEvent::Pointer { buttons, .. } if buttons == now) =>
            {
                *held = event;
                return true;
            }
            _ => {}
        }
        let bit = |key: u32| 1u128.checked_shl(key);
        if let InputEvent::Transition { key, down: false } = event {
            if let Some(bit) = bit(key).filter(|&bit| self.dropped & bit != 0) {
                self.dropped &= !bit;
                return false;
            }
        }
        if self.events.len() >= HELD_MOST || self.events.try_reserve(1).is_err() {
            if let InputEvent::Transition { key, down: true } = event {
                self.dropped |= bit(key).unwrap_or(0);
            }
            return false;
        }
        self.events.push_back(event);
        true
    }
}

/// The host's sending half: the session, the commands waiting for room in
/// its queue, and why it ended, once it has.
struct Out<T: SessionTransport, S: Sink> {
    session: SandboxSession<T, S>,
    /// Each command encoded before it is queued.
    scratch: Vec<u8>,
    /// Commands the session's queue had no room for, in order.
    pending: VecDeque<Vec<u8>>,
    why: Option<Why>,
}

impl<T: SessionTransport, S: Sink> Out<T, S> {
    /// `command` queued, behind any waiting for room.
    fn send(&mut self, command: &Command<'_>) {
        if self.why.is_some() {
            return;
        }
        let mut w = Writer::reusing(core::mem::take(&mut self.scratch));
        command.encode(&mut w);
        self.scratch = w.finish();
        if self.pending.is_empty() {
            match self.session.send(&self.scratch) {
                Ok(()) => return,
                Err(SessionError::OutboundFull) => {}
                Err(_) => {
                    self.failed();
                    return;
                }
            }
        }
        let mut kept = Vec::new();
        if kept.try_reserve_exact(self.scratch.len()).is_err()
            || self.pending.try_reserve(1).is_err()
        {
            self.end_for(Why::NoRoom);
            return;
        }
        kept.extend_from_slice(&self.scratch);
        self.pending.push_back(kept);
    }

    /// What waited for room queued, as far as there is room.
    fn send_pending(&mut self) {
        while let Some(command) = self.pending.front() {
            match self.session.send(command) {
                Ok(()) => {
                    self.pending.pop_front();
                }
                Err(SessionError::OutboundFull) => return,
                Err(_) => {
                    self.failed();
                    return;
                }
            }
        }
    }

    /// The session ended for `why`, its worker contained where it still
    /// runs.
    fn end_for(&mut self, why: Why) -> Why {
        if self.why.is_none() {
            self.session.condemn(why.reason());
            self.why = Some(why);
        }
        self.why.unwrap_or(why)
    }

    /// The worker sent what it could not have: contained.
    fn condemn(&mut self, why: &'static str) -> Why {
        self.end_for(Why::Unbelievable(Unbelievable(why)))
    }

    /// The session failed, its worker already contained.
    fn failed(&mut self) -> Why {
        let end = self.session.worker_end().unwrap_or(WorkerEnd::Unknown);
        *self.why.get_or_insert(Why::Worker(end))
    }

    /// The worker closed its side of the session: why it stopped, as its
    /// exit status says.
    fn finished(&mut self) -> Why {
        if self.why.is_none() {
            self.session.condemn("its worker stopped");
        }
        let end = self.session.worker_end().unwrap_or(WorkerEnd::Unknown);
        let why = match self.session.exit_status().and_then(Stopped::of_code) {
            Some(stopped) => Why::Stopped(stopped),
            None => Why::Worker(end),
        };
        *self.why.get_or_insert(why)
    }
}

/// The front end's side of one session.
pub struct Host<T: SessionTransport, S: Sink> {
    out: Out<T, S>,
    front: Front,
    share: Arc<dyn Share>,
    /// The frame last taken, which the event lent borrows.
    frame: Vec<u8>,
    /// The damage record last copied out of the ring.
    pixels: Vec<u8>,
    /// Input past each context's credit.
    held: OrderedMap<Context, Held>,
}

impl<T: SessionTransport, S: Sink> Host<T, S> {
    /// The session over `transport`, its failures recorded to `sink`, started
    /// with `begin`.
    ///
    /// # Errors
    ///
    /// [`Unstarted`]: the worker is ended again whatever stopped it.
    pub fn start(transport: T, sink: S, begin: &Begin<'_>) -> Result<Self, Unstarted> {
        let bounds =
            SessionBounds::new(2 * FRAME_MOST, 2 * FRAME_MOST).map_err(Unstarted::Session)?;
        let mut session =
            SandboxSession::new(transport, bounds, sink).map_err(Unstarted::Session)?;
        let share = session
            .share()
            .filter(|share| share.byte_len() == RING_BYTES as usize)
            .cloned();
        let Some(share) = share else {
            session.condemn("a worker handed no damage ring");
            return Err(Unstarted::Share);
        };
        let mut pixels = Vec::new();
        let room = pixels.try_reserve_exact(RECORD_MOST as usize).is_ok();
        let Some(front) = Front::new().filter(|_| room) else {
            session.condemn("no memory for the session's front end");
            return Err(Unstarted::NoMemory);
        };
        let mut host = Self {
            out: Out {
                session,
                scratch: Vec::new(),
                pending: VecDeque::new(),
                why: None,
            },
            front,
            share,
            frame: Vec::new(),
            pixels,
            held: OrderedMap::new(),
        };
        host.out.send(&Command::Begin(*begin));
        match host.out.why {
            Some(_) => Err(Unstarted::NoMemory),
            None => Ok(host),
        }
    }

    /// The descriptors to register on a wait-set: none once the session has
    /// ended.
    #[must_use]
    pub fn descriptors(&self) -> Option<SessionDescriptors> {
        self.out.session.descriptors()
    }

    /// Whether read readiness should be armed.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.out.why.is_none() && self.out.session.wants_read()
    }

    /// Whether write readiness should be armed.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        self.out.why.is_none() && (self.out.session.wants_write() || !self.out.pending.is_empty())
    }

    /// Read readiness fired.
    pub fn on_readable(&mut self) {
        if self.out.why.is_none() && self.out.session.on_readable().is_err() {
            self.out.failed();
        }
    }

    /// Write readiness fired: what the queue holds written, and what waited
    /// for room in it queued.
    pub fn on_writable(&mut self) {
        if self.out.why.is_some() {
            return;
        }
        if self.out.session.on_writable().is_err() {
            self.out.failed();
            return;
        }
        self.out.send_pending();
    }

    /// Why the session ended, once it has.
    #[must_use]
    pub const fn why(&self) -> Option<Why> {
        self.out.why
    }

    /// The contexts running, in order, each with the command line it was
    /// launched with.
    pub fn running(&self) -> impl Iterator<Item = (Context, &[u8])> {
        self.front.running()
    }

    /// The screen `context` was last told of.
    #[must_use]
    pub fn screen(&self, context: Context) -> Option<Screen> {
        self.front.screen(context)
    }

    /// The next event the worker sent, believed: none until one has come
    /// whole. For a credit, the input held for its context is sent first.
    ///
    /// # Errors
    ///
    /// [`Why`] the session ended: a worker that failed, stopped by itself or
    /// sent what it could not have, which is then contained.
    pub fn told(&mut self) -> Result<Option<Told<'_>>, Why> {
        if let Some(why) = self.out.why {
            return Err(why);
        }
        let frame = &mut self.frame;
        let taken = self.out.session.recv(|payload| {
            frame.clear();
            frame
                .try_reserve(payload.len())
                .map(|()| frame.extend_from_slice(payload))
        });
        match taken {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(_))) => return Err(self.out.end_for(Why::NoRoom)),
            Ok(None) if self.out.session.peer_finished() => return Err(self.out.finished()),
            Ok(None) => return Ok(None),
            Err(_) => return Err(self.out.failed()),
        }
        let Self {
            out,
            front,
            share,
            frame,
            pixels,
            held,
        } = self;
        let Ok(event) = Event::decode(frame) else {
            return Err(out.condemn("an event that does not decode"));
        };
        let placed = match front.believe(&event) {
            Ok(placed) => placed,
            Err(Unbelievable(why)) => return Err(out.condemn(why)),
        };
        match event {
            Event::Credit { context, .. } => send_held(out, front, held, context),
            Event::Ended { context, .. } => {
                held.remove(&context);
            }
            _ => {}
        }
        let pixels: &[u8] = match placed {
            Some(placed) => {
                pixels.clear();
                pixels.resize(placed.bytes as usize, 0);
                if !share.copy_out(placed.offset as usize, pixels) {
                    return Err(out.condemn("damage beyond the ring"));
                }
                front.release(placed.released());
                out.send(&Command::Released {
                    bytes: placed.released(),
                });
                pixels
            }
            None => &[],
        };
        Ok(Some(Told { event, pixels }))
    }

    /// The session's contexts run, or every one holds where it stands.
    pub fn run(&mut self, running: bool) {
        self.out.send(if running {
            &Command::Run
        } else {
            &Command::Hold
        });
    }

    /// A launch of `line`: the serial its answer names, none where the
    /// model would not ask it.
    pub fn launch(&mut self, line: &[u8]) -> Option<u32> {
        let serial = self.front.launch(line)?;
        self.out.send(&Command::Launch { serial, line });
        Some(serial)
    }

    /// The session's boot: the serial its answer names, none where it was
    /// asked before or the model would not ask it.
    pub fn boot(&mut self) -> Option<u32> {
        let serial = self.front.boot()?;
        self.out.send(&Command::Boot { serial });
        Some(serial)
    }

    /// A quit of `context`: whether it was asked.
    pub fn quit(&mut self, context: Context) -> bool {
        let asked = self.front.quit(context);
        if asked {
            self.out.send(&Command::Quit { context });
        }
        asked
    }

    /// `context` held where it stands, or let go on: whether it was asked.
    pub fn pause(&mut self, context: Context, paused: bool) -> bool {
        let asked = self.front.runs(context);
        if asked {
            self.out.send(&Command::Pause { context, paused });
        }
        asked
    }

    /// `event` for `context`: sent within its credit, else held behind the
    /// rest; whether it was sent or held rather than dropped.
    pub fn input(&mut self, context: Context, event: InputEvent) -> bool {
        if !self.front.runs(context) {
            return false;
        }
        let unheld = self
            .held
            .get(&context)
            .is_none_or(|held| held.events.is_empty());
        if unheld && self.front.input(context) {
            self.out.send(&Command::Input { context, event });
            return true;
        }
        if !self.held.contains_key(&context)
            && self.held.try_insert(context, Held::default()).is_err()
        {
            return false;
        }
        self.held
            .get_mut(&context)
            .is_some_and(|held| held.hold(event))
    }

    /// Relay `serial` answered with `reply`: whether it was asked.
    pub fn answer(&mut self, serial: u32, reply: &[u8]) -> bool {
        let asked = self.front.answer(serial);
        if asked {
            self.out.send(&Command::Answer { serial, reply });
        }
        asked
    }

    /// Relay `serial` could not be carried: whether it was asked.
    pub fn unanswered(&mut self, serial: u32) -> bool {
        let asked = self.front.answer(serial);
        if asked {
            self.out.send(&Command::Unanswered { serial });
        }
        asked
    }

    /// The machine's memory pressure is now `band`.
    pub fn pressure(&mut self, band: PressureBand) {
        self.out.send(&Command::Pressure(band));
    }

    /// The desktop's text is now `font`.
    pub fn font(&mut self, font: DesktopFont) {
        self.out.send(&Command::Font(font));
    }
}

/// The input held for `context` sent, as far as its credit lets it.
fn send_held<T: SessionTransport, S: Sink>(
    out: &mut Out<T, S>,
    front: &mut Front,
    held: &mut OrderedMap<Context, Held>,
    context: Context,
) {
    let Some(waiting) = held.get_mut(&context) else {
        return;
    };
    while !waiting.events.is_empty() && front.input(context) {
        let Some(event) = waiting.events.pop_front() else {
            return;
        };
        out.send(&Command::Input { context, event });
    }
}

#[cfg(test)]
mod tests;

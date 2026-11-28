//! The worker's end of the session, shared by the run loop and the kernel's
//! host: the port and its clock, each context's mailbox — its inbox, the
//! credit it owes and the reply its parked relay waits for — the relays
//! outstanding, and the commands that arrived while the kernel waited in
//! place.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_collections::OrderedMap;
use tairix_reclaim::PressureBand;
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::os::{BootError, Ending, Host, Pulse};
use tairix_riscos_kernel::relay::{Relayed, Side};
use tairix_sandbox::wire::Writer;
use tairix_sandbox::worker::WorkerExit;
use tairix_sandbox::{ProtoError, Waited, WorkerPort};

use crate::credit::Inbox;
use crate::protocol::{Command, Context, Event, Stopped, BOOT, RELAY_MOST, WAITING_MOST};

/// Why the worker stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The front end closed the session.
    Closed,
    /// The session failed.
    Failed(ProtoError),
    /// The front end sent what the protocol does not allow.
    Violation(&'static str),
    /// The kernel would not boot.
    Refused(BootError),
    /// The kernel faulted switching contexts, or selecting the keyboard the
    /// session was started with.
    Fault,
    /// There was no memory to switch contexts with, or to keep what the
    /// session was started with.
    NoMemory,
}

impl End {
    /// The exit status a worker that ends for this carries: the sandbox's own
    /// where one names it, the session's [`Stopped`] otherwise.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Closed => WorkerExit::Finished.code(),
            Self::Failed(ProtoError::PeerClosed) => WorkerExit::PeerClosed.code(),
            Self::Failed(ProtoError::Oversize) => WorkerExit::Oversize.code(),
            Self::Failed(ProtoError::OutOfMemory) | Self::NoMemory => {
                WorkerExit::OutOfMemory.code()
            }
            Self::Failed(ProtoError::Channel(_)) => WorkerExit::Transport.code(),
            Self::Violation(_) => Stopped::Violation.code(),
            Self::Refused(_) => Stopped::Refused.code(),
            Self::Fault => Stopped::Fault.code(),
        }
    }
}

/// A command the run loop applies, kept from where it arrived.
#[derive(Debug)]
pub(super) enum Kept {
    Launch { serial: u32, line: Vec<u8> },
    Boot { serial: u32 },
    Quit { context: Context },
}

/// Nanoseconds in a centisecond, the guest clock's unit.
const CENTISECOND: u64 = 10_000_000;

/// A relay outstanding: the context it was made for, whether that context
/// is parked on it rather than the kernel waiting in place, and whether its
/// answer, which may cross the context's end, is to be dropped.
#[derive(Clone, Copy, Debug)]
struct Outstanding {
    context: Context,
    parked: bool,
    abandoned: bool,
}

/// What a relay came to.
#[derive(Debug)]
pub(super) enum Reply {
    Answered(Vec<u8>),
    Failed,
}

/// What came for the relay the kernel waits on in place.
enum InPlace {
    Waiting(u32),
    Came(Reply),
}

/// A context's input, whether what it has taken in is still to be told,
/// the reply its parked relay waits for, and whether the front end holds it:
/// room for each held from its start, so that nothing that arrives needs
/// more.
struct Mailbox {
    inbox: Inbox,
    owes_credit: bool,
    reply: Option<Reply>,
    paused: bool,
}

/// The worker's end of the session.
pub(super) struct Port<P> {
    link: P,
    /// The bytes each event is encoded in, kept from one to the next.
    scratch: Vec<u8>,
    /// The port's clock when the session began, and the real time then.
    origin: u64,
    real_time: u64,
    /// The context the kernel reads input for.
    pub(super) current: Context,
    mailboxes: OrderedMap<Context, Mailbox>,
    /// The mailboxes that owe a credit.
    owing: usize,
    outstanding: OrderedMap<u32, Outstanding>,
    next_serial: u32,
    in_place: Option<InPlace>,
    /// The relay the kernel left pending in the turn under way.
    pub(super) parked: Option<u32>,
    /// Whether the front end lets the session run.
    pub(super) running: bool,
    /// The last context given a mailbox: a quit of one above it, or of the
    /// boot's, names a context never made.
    made: Context,
    /// Room for [`WAITING_MOST`] from the start, and for the one being
    /// applied, which may be kept again, so that no command is lost for want
    /// of it.
    pub(super) kept: VecDeque<Kept>,
    /// The damage ring's bytes given back since the run loop last took them.
    pub(super) released: u64,
    /// The machine's memory pressure, where it moved since the run loop last
    /// took it.
    pub(super) pressure: Option<PressureBand>,
    /// The desktop's text, where it changed since the run loop last took it.
    pub(super) font: Option<DesktopFont>,
    pub(super) end: Option<End>,
}

/// `event` encoded in `scratch` and queued on `link`.
fn queue<P: WorkerPort>(
    link: &mut P,
    scratch: &mut Vec<u8>,
    event: &Event<'_>,
) -> Result<(), ProtoError> {
    let mut w = Writer::reusing(core::mem::take(scratch));
    event.encode(&mut w);
    *scratch = w.finish();
    link.queue(scratch)
}

impl<P: WorkerPort> Port<P> {
    /// The port over `link`, its clock reading `origin` at `real_time`, its
    /// kept commands' room held from the start: none where that is not had.
    pub(super) fn new(link: P, origin: u64, real_time: u64) -> Option<Self> {
        let mut kept = VecDeque::new();
        kept.try_reserve_exact(WAITING_MOST + 1).ok()?;
        Some(Self {
            link,
            scratch: Vec::new(),
            origin,
            real_time,
            current: BOOT,
            mailboxes: OrderedMap::new(),
            owing: 0,
            outstanding: OrderedMap::new(),
            next_serial: 0,
            in_place: None,
            parked: None,
            running: false,
            made: BOOT,
            kept,
            released: 0,
            pressure: None,
            font: None,
            end: None,
        })
    }

    /// The guest's monotonic clock: centiseconds since the session began.
    pub(super) fn monotonic(&self) -> u64 {
        self.link.now().saturating_sub(self.origin) / CENTISECOND
    }

    /// The port's clock at monotonic centisecond `at`.
    pub(super) const fn instant(&self, at: u64) -> u64 {
        self.origin.saturating_add(at.saturating_mul(CENTISECOND))
    }

    /// The session ends, for `why`, unless it has already.
    pub(super) fn finish(&mut self, why: End) {
        if self.end.is_none() {
            self.end = Some(why);
        }
    }

    /// A mailbox made for `context`, which has none.
    pub(super) fn open(&mut self, context: Context) -> bool {
        let Some(inbox) = Inbox::new() else {
            return false;
        };
        let mailbox = Mailbox {
            inbox,
            owes_credit: false,
            reply: None,
            paused: false,
        };
        let made = !self.mailboxes.contains_key(&context)
            && self.mailboxes.try_insert(context, mailbox).is_ok();
        if made {
            self.made = self.made.max(context);
        }
        made
    }

    /// `context`'s mailbox let go, every relay it made abandoned and a quit
    /// of it waiting dropped.
    pub(super) fn close(&mut self, context: Context) {
        if self
            .mailboxes
            .remove(&context)
            .is_some_and(|mailbox| mailbox.owes_credit)
        {
            self.owing -= 1;
        }
        for outstanding in self.outstanding.values_mut() {
            if outstanding.context == context {
                outstanding.abandoned = true;
            }
        }
        self.kept.retain(
            |kept| !matches!(*kept, Kept::Quit { context: quitting } if quitting == context),
        );
    }

    /// Relay `serial`'s answer, when it comes, dropped.
    pub(super) fn abandon(&mut self, serial: u32) {
        if let Some(outstanding) = self.outstanding.get_mut(&serial) {
            outstanding.abandoned = true;
        }
    }

    /// Whether the front end has been told of `context` and not of its end.
    pub(super) fn knows(&self, context: Context) -> bool {
        self.mailboxes.contains_key(&context)
    }

    /// Whether the front end holds `context` where it stands.
    pub(super) fn paused(&self, context: Context) -> bool {
        self.mailboxes
            .get(&context)
            .is_some_and(|mailbox| mailbox.paused)
    }

    /// Whether input waits for `context`.
    pub(super) fn input_waits(&self, context: Context) -> bool {
        self.mailboxes
            .get(&context)
            .is_some_and(|mailbox| mailbox.inbox.waiting())
    }

    /// The reply `context` is parked on, taken where it has come.
    pub(super) fn reply(&mut self, context: Context) -> Option<Reply> {
        self.mailboxes.get_mut(&context)?.reply.take()
    }

    /// Whether the reply `context` is parked on has come.
    pub(super) fn answered(&self, context: Context) -> bool {
        self.mailboxes
            .get(&context)
            .is_some_and(|mailbox| mailbox.reply.is_some())
    }

    /// `event` queued to go with the next flush.
    pub(super) fn send(&mut self, event: &Event<'_>) {
        if let Err(error) = queue(&mut self.link, &mut self.scratch, event) {
            self.finish(End::Failed(error));
        }
    }

    /// Every event queued written, each context's input taken in told first.
    pub(super) fn flush(&mut self) {
        let mut failed = None;
        if self.owing > 0 {
            for (&context, mailbox) in &mut self.mailboxes {
                if !core::mem::take(&mut mailbox.owes_credit) {
                    continue;
                }
                self.owing -= 1;
                let credit = Event::Credit {
                    context,
                    applied: mailbox.inbox.applied(),
                };
                failed = queue(&mut self.link, &mut self.scratch, &credit).err();
                if failed.is_some() || self.owing == 0 {
                    break;
                }
            }
        }
        if let Some(error) = failed.or_else(|| self.link.flush().err()) {
            self.finish(End::Failed(error));
        }
    }

    /// The commands that have come taken, waiting at most until the port's
    /// clock reads `until` for the first — for ever where it is `None` —
    /// and not at all for the rest.
    pub(super) fn take_commands(&mut self, until: Option<u64>) {
        let mut wait = until.map(|until| until.saturating_sub(self.link.now()));
        while self.end.is_none() {
            let taken = self.link.recv(wait, |payload| {
                Command::decode(payload).map_or(Taken::Malformed, Taken::of)
            });
            match taken {
                Ok(Waited::Frame(taken)) => self.apply(taken),
                Ok(Waited::Nothing) => return,
                Ok(Waited::Closed) => self.finish(End::Closed),
                Err(error) => self.finish(End::Failed(error)),
            }
            wait = Some(0);
        }
    }

    fn apply(&mut self, taken: Taken) {
        match taken {
            Taken::Malformed => self.finish(End::Violation("a command that does not decode")),
            Taken::Begin => self.finish(End::Violation("a session started twice")),
            Taken::Input { context, event } => self.input(context, event),
            Taken::Released { bytes } => self.released += u64::from(bytes),
            Taken::Answer { serial, reply } => self.answer(serial, reply),
            Taken::Running(running) => self.running = running,
            Taken::Pressure(band) => self.pressure = Some(band),
            Taken::Font(font) => self.font = Some(font),
            Taken::Unheld { serial } => self.send(&Event::Refused {
                serial,
                error: None,
            }),
            Taken::Pause { context, .. } if context == BOOT || context > self.made => {
                self.finish(End::Violation("a pause of a context never made"));
            }
            // One that has ended crossed its end.
            Taken::Pause { context, paused } => {
                if let Some(mailbox) = self.mailboxes.get_mut(&context) {
                    mailbox.paused = paused;
                }
            }
            Taken::Kept(Kept::Quit { context }) if context == BOOT || context > self.made => {
                self.finish(End::Violation("a quit of a context never made"));
            }
            // One that has ended: the quit crossed its end.
            Taken::Kept(Kept::Quit { context }) if !self.mailboxes.contains_key(&context) => {}
            Taken::Kept(kept) => {
                if self.kept.len() >= WAITING_MOST {
                    self.finish(End::Violation(
                        "more launches and quits waiting than are kept",
                    ));
                } else {
                    self.kept.push_back(kept);
                }
            }
        }
    }

    /// `event` for `context`, held until the kernel reads it. One for a
    /// context that has ended crossed its end and is dropped.
    fn input(&mut self, context: Context, event: InputEvent) {
        let Some(mailbox) = self.mailboxes.get_mut(&context) else {
            return;
        };
        if mailbox.inbox.receive(event).is_err() {
            self.finish(End::Violation("input beyond its credit"));
        }
    }

    fn answer(&mut self, serial: u32, reply: Reply) {
        let Some(outstanding) = self.outstanding.remove(&serial) else {
            self.finish(End::Violation("an answer to no relay"));
            return;
        };
        if outstanding.abandoned {
            return;
        }
        if let Some(InPlace::Waiting(waiting)) = self.in_place {
            if waiting == serial {
                self.in_place = Some(InPlace::Came(reply));
                return;
            }
        }
        let parked_on = outstanding
            .parked
            .then(|| self.mailboxes.get_mut(&outstanding.context))
            .flatten();
        match parked_on {
            Some(mailbox) if mailbox.reply.is_none() => mailbox.reply = Some(reply),
            _ => self.finish(End::Violation("an answer to a relay waited on elsewhere")),
        }
    }

    /// The next input event for the current context, taken in.
    fn take_input(&mut self) -> Option<InputEvent> {
        let mailbox = self.mailboxes.get_mut(&self.current)?;
        let event = mailbox.inbox.take()?;
        if !mailbox.owes_credit {
            mailbox.owes_credit = true;
            self.owing += 1;
        }
        Some(event)
    }

    /// A relay of `request` to `side` for the current context, which is
    /// parked on it where `parking` lets it be and it is parked on no other:
    /// its serial, and whether it is parked on.
    fn relay(&mut self, side: Side, request: &[u8], parking: bool) -> Option<(u32, bool)> {
        let context = self.current;
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1);
        // A serial still outstanding once they wrap is not given again.
        if request.len() > RELAY_MOST || self.outstanding.contains_key(&serial) {
            return None;
        }
        // One parked on and unanswered, abandoned or not, is the front end's
        // to answer still: another waits in place.
        let parked = parking
            && !self
                .outstanding
                .values()
                .any(|outstanding| outstanding.parked && outstanding.context == context);
        let outstanding = Outstanding {
            context,
            parked,
            abandoned: false,
        };
        if self.outstanding.try_insert(serial, outstanding).is_err() {
            return None;
        }
        self.send(&Event::Relay {
            serial,
            context,
            parked,
            side,
            request,
        });
        self.flush();
        Some((serial, parked))
    }

    /// The commands that come taken while the front end holds the session,
    /// until it runs it again or the session ends: a context waiting in
    /// place or part way through its turn holds where it stands.
    fn hold_while_held(&mut self) {
        while !self.running && self.end.is_none() {
            self.flush();
            self.take_commands(None);
        }
    }

    /// The reply to relay `serial` waited for in place, every other command
    /// that comes meanwhile taken: failed where the session ends first.
    fn wait_in_place(&mut self, serial: u32) -> Reply {
        self.in_place = Some(InPlace::Waiting(serial));
        loop {
            if let Some(InPlace::Came(reply)) = self
                .in_place
                .take_if(|in_place| matches!(in_place, InPlace::Came(_)))
            {
                return reply;
            }
            if self.end.is_some() {
                self.in_place = None;
                return Reply::Failed;
            }
            self.take_commands(None);
        }
    }
}

/// A command as the port takes it: its bytes copied out of the frame.
enum Taken {
    Malformed,
    /// A start, which only the first command may be.
    Begin,
    Input {
        context: Context,
        event: InputEvent,
    },
    Released {
        bytes: u32,
    },
    Answer {
        serial: u32,
        reply: Reply,
    },
    /// Run, or hold.
    Running(bool),
    Pause {
        context: Context,
        paused: bool,
    },
    Pressure(PressureBand),
    Font(DesktopFont),
    /// A launch whose line there was no room to keep.
    Unheld {
        serial: u32,
    },
    Kept(Kept),
}

impl Taken {
    fn of(command: Command<'_>) -> Self {
        let owned = |bytes: &[u8]| {
            let mut owned = Vec::new();
            owned.try_reserve_exact(bytes.len()).ok()?;
            owned.extend_from_slice(bytes);
            Some(owned)
        };
        match command {
            Command::Begin(_) => Self::Begin,
            Command::Run => Self::Running(true),
            Command::Hold => Self::Running(false),
            Command::Pause { context, paused } => Self::Pause { context, paused },
            Command::Pressure(band) => Self::Pressure(band),
            Command::Font(font) => Self::Font(font),
            Command::Boot { serial } => Self::Kept(Kept::Boot { serial }),
            Command::Input { context, event } => Self::Input { context, event },
            Command::Released { bytes } => Self::Released { bytes },
            // An answer there is no room to keep fails the relay.
            Command::Answer { serial, reply } => Self::Answer {
                serial,
                reply: owned(reply).map_or(Reply::Failed, Reply::Answered),
            },
            Command::Unanswered { serial } => Self::Answer {
                serial,
                reply: Reply::Failed,
            },
            Command::Launch { serial, line } => match owned(line) {
                Some(line) => Self::Kept(Kept::Launch { serial, line }),
                None => Self::Unheld { serial },
            },
            Command::Quit { context } => Self::Kept(Kept::Quit { context }),
        }
    }
}

/// The kernel's host: the session's clock, the current context's input, and
/// the relays, over the shared port.
pub(super) struct WorkerHost<P>(pub(super) Rc<RefCell<Port<P>>>);

impl<P: WorkerPort> Host for WorkerHost<P> {
    fn monotonic(&mut self) -> u64 {
        self.0.borrow().monotonic()
    }

    fn real_time(&mut self) -> u64 {
        let port = self.0.borrow();
        port.real_time.saturating_add(port.monotonic())
    }

    fn pulse(&mut self) -> Pulse {
        let mut port = self.0.borrow_mut();
        port.take_commands(Some(0));
        port.flush();
        port.hold_while_held();
        if port.end.is_some() {
            Pulse::Stop
        } else {
            Pulse::Continue
        }
    }

    fn wait(&mut self, until: Option<u64>, _: Ending) -> Pulse {
        let mut port = self.0.borrow_mut();
        port.flush();
        let deadline = until.map(|at| port.instant(at));
        port.take_commands(deadline);
        port.hold_while_held();
        if port.end.is_some() {
            Pulse::Stop
        } else {
            Pulse::Continue
        }
    }

    fn input(&mut self) -> Option<InputEvent> {
        self.0.borrow_mut().take_input()
    }

    fn relay(&mut self, side: Side, request: &[u8], reply: &mut Vec<u8>, parking: bool) -> Relayed {
        let mut port = self.0.borrow_mut();
        let Some((serial, parked)) = port.relay(side, request, parking) else {
            return Relayed::Failed;
        };
        if parked {
            port.parked = Some(serial);
            return Relayed::Pending;
        }
        match port.wait_in_place(serial) {
            Reply::Answered(answer) => appended(reply, answer),
            Reply::Failed => Relayed::Failed,
        }
    }
}

/// `answer` appended to `reply`, moved where `reply` holds nothing yet.
fn appended(reply: &mut Vec<u8>, answer: Vec<u8>) -> Relayed {
    if reply.is_empty() {
        *reply = answer;
    } else if reply.try_reserve(answer.len()).is_ok() {
        reply.extend_from_slice(&answer);
    } else {
        return Relayed::Failed;
    }
    Relayed::Answered
}

/// The host for `port`, boxed for the kernel.
pub(super) fn host<P: WorkerPort + 'static>(port: &Rc<RefCell<Port<P>>>) -> Box<dyn Host> {
    Box::new(WorkerHost(Rc::clone(port)))
}

#[cfg(test)]
mod tests;

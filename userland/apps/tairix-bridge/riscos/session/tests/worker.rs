//! The worker over the real kernel, VDU drivers, filing system and BASIC,
//! driven by a front end the test scripts that believes every event only
//! through the front end's model of the session: launches made, waiting
//! while the session is held, and ended; input credited to the context it is
//! for; screens and their damage in the ring where the front end finds them,
//! a waiting context's shown while another runs; relays carried and answered,
//! and those a context's end makes told before its end; the session's timed
//! work done while every context waits; a launch the machine has no room for
//! refused; and a front end that breaks the protocol ending the session.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::LazyLock;

use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fscopy::tree::Roots;
use tairix_riscos_filing::hostfs::server::{Given, Server};
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::layout::RISC_OS_5;
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Config, Host};
use tairix_riscos_kernel::relay::Side;
use tairix_riscos_session::front::Front;
use tairix_riscos_session::protocol::{Command, Context, Ending, Event, Screen, WAITING_MOST};
use tairix_riscos_session::ring::{Placed, RING_BYTES};
use tairix_riscos_session::worker::{serve, End, Setup};
use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::session::Share;
use tairix_sandbox::wire::Writer;
use tairix_sandbox::{ProtoError, Waited, WorkerPort};

static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    [
        &tairix_riscos_filing::MODULES[..],
        &tairix_riscos_vdu::MODULES[..],
        &tairix_riscos_basic::MODULES[..],
    ]
    .concat()
});

/// Nanoseconds in a centisecond, the guest clock's unit.
const CENTISECOND: u64 = 10_000_000;
/// The most reads of its port a test's worker makes. A worker reads once or
/// twice a turn, and a turn is a million instructions, so one that reads
/// more is spinning: the port closes the session on it.
const RECV_MOST: u64 = 100_000;

/// An event as the front end keeps it.
#[derive(Clone, PartialEq, Eq)]
enum Seen {
    Credit(Context, u32),
    Launched(u32, Context),
    Refused(u32, Option<u32>),
    Screen(Context, Screen),
    /// A damage record: its context, first line, lines and pixels.
    Damage(Context, u32, u32, Vec<u32>),
    /// A relay to a side, and whether its context is parked on it.
    Relay(Side, bool),
    Ended(Context, String),
    /// The front end's own: the ring's bytes it gave back at last.
    Withheld(u32),
}

/// A damage record's pixels counted rather than listed, so that a failure
/// reads.
impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credit(context, applied) => write!(f, "Credit({context}, {applied})"),
            Self::Launched(serial, context) => write!(f, "Launched({serial}, {context})"),
            Self::Refused(serial, error) => write!(f, "Refused({serial}, {error:?})"),
            Self::Screen(context, screen) => write!(f, "Screen({context}, {screen:?})"),
            Self::Damage(context, first, lines, pixels) => {
                write!(
                    f,
                    "Damage({context}, {first}, {lines}, {} pixels)",
                    pixels.len()
                )
            }
            Self::Relay(side, parked) => write!(f, "Relay({side:?}, {parked})"),
            Self::Ended(context, ending) => write!(f, "Ended({context}, {ending})"),
            Self::Withheld(bytes) => write!(f, "Withheld({bytes})"),
        }
    }
}

fn ending(ending: &Ending<'_>) -> String {
    match ending {
        Ending::Exit { code } => format!("exit {code}"),
        Ending::Error(error) => format!("error {:X}", error.number),
        Ending::Fault => "fault".into(),
        Ending::Reset { .. } => "reset".into(),
        Ending::Unmodelled(_) => "unmodelled".into(),
        Ending::Quit => "quit".into(),
    }
}

fn encoded(command: &Command<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

/// What a script has the front end do.
#[derive(Clone, Copy, Debug)]
enum Act {
    /// A command sent as a front end sends it: a launch numbered by the
    /// model, input within its context's credit.
    Send(Command<'static>),
    /// A command sent whatever the front end was told.
    Breach(Command<'static>),
}

/// How a front end gives the ring's space back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Giving {
    /// At once.
    AtOnce,
    /// Not until a relay whose request names `trigger` comes, then all of
    /// it: with the session held as the relay's answer goes, and run again
    /// once more lines come, where `holding`.
    Withholding {
        kept: u32,
        trigger: &'static [u8],
        holding: bool,
    },
    Held,
    Resumed,
}

/// A front end: the commands it sends, every event it was sent, its clock,
/// its model of the session, how it gives the ring back, the keys it types
/// into the first context launched within that context's credit, and what
/// it does on each event it sees.
struct Driver {
    commands: VecDeque<Vec<u8>>,
    /// Commands it sends once its clock reads their time, in time order.
    later: VecDeque<(u64, Vec<u8>)>,
    /// How long after a relay comes the front end answers it, where its
    /// context is parked on it; a relay waited for in place is answered at
    /// once.
    parked_answer_after: Option<u64>,
    seen: Vec<Seen>,
    clock: u64,
    /// When it closes the session, where nothing more would end it.
    close_at: Option<u64>,
    /// The worker's reads of its port, and whether there were too many.
    recvs: u64,
    spun: bool,
    /// The clock when the last damage came.
    damaged_at: u64,
    model: Front,
    giving: Giving,
    share: HeapShare,
    server: Option<Server<Handle>>,
    keys: VecDeque<u8>,
    typing: Option<Context>,
    react: fn(&Seen) -> Vec<Act>,
}

impl Driver {
    fn queue(&mut self, command: &Command<'_>) {
        self.commands.push_back(encoded(command));
    }

    fn act(&mut self, act: Act) {
        match act {
            // One the model will not have asked is not sent.
            Act::Send(Command::Launch { line, .. }) => {
                if let Some(serial) = self.model.launch(line) {
                    self.queue(&Command::Launch { serial, line });
                }
            }
            Act::Send(Command::Quit { context }) => {
                if self.model.quit(context) {
                    self.queue(&Command::Quit { context });
                }
            }
            Act::Send(command @ Command::Input { context, .. }) => {
                assert!(self.model.input(context), "within its credit");
                self.queue(&command);
            }
            Act::Breach(command @ Command::Input { context, .. }) => {
                // Sent as far as the credit counts it; past that the worker
                // ends the session.
                let _ = self.model.input(context);
                self.queue(&command);
            }
            Act::Send(command) | Act::Breach(command) => self.queue(&command),
        }
    }

    fn type_keys(&mut self) {
        let Some(context) = self.typing else {
            return;
        };
        while let Some(&key) = self.keys.front() {
            if !self.model.input(context) {
                return;
            }
            self.keys.pop_front();
            self.queue(&Command::Input {
                context,
                event: InputEvent::Key(key),
            });
        }
    }

    /// The ring's oldest `bytes` given back.
    fn release(&mut self, bytes: u32) {
        assert!(self.model.release(bytes), "held");
        self.queue(&Command::Released { bytes });
    }

    fn take(&mut self, event: Event<'_>) {
        let placed = match self.model.believe(&event) {
            Ok(placed) => placed,
            Err(why) => panic!("{event:?} could not have been sent: {}", why.0),
        };
        let seen = match event {
            Event::Credit { context, applied } => {
                self.type_keys();
                Seen::Credit(context, applied)
            }
            Event::Launched { serial, context } => {
                if self.typing.is_none() && !self.keys.is_empty() {
                    self.typing = Some(context);
                    self.type_keys();
                }
                Seen::Launched(serial, context)
            }
            Event::Refused { serial, error } => {
                Seen::Refused(serial, error.map(|error| error.number))
            }
            Event::Screen { context, screen } => Seen::Screen(context, screen),
            Event::Damage {
                context,
                first,
                lines,
                ..
            } => {
                let pixels = self.copied(placed.expect("damage is placed"));
                Seen::Damage(context, first, lines, pixels)
            }
            Event::Relay {
                serial,
                parked,
                side,
                request,
                ..
            } => {
                self.answer(serial, side, parked, request);
                Seen::Relay(side, parked)
            }
            Event::Ended { context, ending: e } => Seen::Ended(context, ending(&e)),
        };
        for act in (self.react)(&seen) {
            self.act(act);
        }
        self.seen.push(seen);
    }

    /// The record `placed` copied out of the ring, its space given back as
    /// the front end gives it: its pixels.
    fn copied(&mut self, placed: Placed) -> Vec<u32> {
        let mut out = vec![0; placed.bytes as usize];
        assert!(self.share.copy_out(placed.offset as usize, &mut out));
        self.damaged_at = self.clock;
        match self.giving {
            Giving::Withholding {
                kept,
                trigger,
                holding,
            } => {
                self.giving = Giving::Withholding {
                    kept: kept + placed.released(),
                    trigger,
                    holding,
                };
            }
            Giving::Held => {
                self.release(placed.released());
                self.queue(&Command::Run);
                self.giving = Giving::Resumed;
            }
            Giving::AtOnce | Giving::Resumed => self.release(placed.released()),
        }
        out.as_chunks::<4>()
            .0
            .iter()
            .map(|&word| u32::from_le_bytes(word))
            .collect()
    }

    /// Relay `serial` of `request` to `side` answered, ahead of everything
    /// else the front end has to send, or later where its context is
    /// `parked` on it and the front end delays such answers; where it names
    /// what the front end withholds the ring for, the ring given back.
    fn answer(&mut self, serial: u32, side: Side, parked: bool, request: &[u8]) {
        let answer = match (side, self.server.as_mut()) {
            (Side::Files, Some(server)) => {
                let mut reply = Vec::new();
                server.serve(request, &mut reply);
                encoded(&Command::Answer {
                    serial,
                    reply: &reply,
                })
            }
            _ => encoded(&Command::Unanswered { serial }),
        };
        assert!(self.model.answer(serial));
        match self.parked_answer_after.filter(|_| parked) {
            Some(after) => self.later.push_back((self.clock + after, answer)),
            None => self.commands.push_front(answer),
        }
        let Giving::Withholding {
            kept,
            trigger,
            holding,
        } = self.giving
        else {
            return;
        };
        if request.windows(trigger.len()).any(|named| named == trigger) {
            if holding {
                self.commands.push_front(encoded(&Command::Hold));
            }
            self.release(kept);
            self.giving = if holding {
                Giving::Held
            } else {
                Giving::AtOnce
            };
            self.seen.push(Seen::Withheld(kept));
        }
    }
}

struct TestPort(Rc<RefCell<Driver>>);

impl WorkerPort for TestPort {
    fn recv<R>(
        &mut self,
        wait_ns: Option<u64>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError> {
        let mut front = self.0.borrow_mut();
        front.recvs += 1;
        if front.recvs > RECV_MOST {
            front.spun = true;
            return Ok(Waited::Closed);
        }
        if let Some(command) = front.commands.pop_front() {
            return Ok(Waited::Frame(take(&command)));
        }
        let clock = front.clock;
        let due = front
            .later
            .front()
            .is_some_and(|&(at, _)| wait_ns.is_none_or(|wait| clock.saturating_add(wait) >= at));
        if due {
            if let Some((at, command)) = front.later.pop_front() {
                front.clock = clock.max(at);
                return Ok(Waited::Frame(take(&command)));
            }
        }
        let closing = front.close_at.is_some_and(|at| front.clock >= at);
        match wait_ns {
            // Nothing will ever come: the test is over.
            None => Ok(Waited::Closed),
            Some(_) if closing => Ok(Waited::Closed),
            Some(wait) => {
                front.clock += wait.max(1);
                Ok(Waited::Nothing)
            }
        }
    }

    fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        let event = Event::decode(payload).expect("an event that decodes");
        self.0.borrow_mut().take(event);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ProtoError> {
        Ok(())
    }

    fn now(&self) -> u64 {
        self.0.borrow().clock
    }
}

fn config(host: Box<dyn Host>) -> Config {
    Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(64 << 20),
        memo_pages: 16,
        host,
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: &MODULES,
        zone: tairix_tz::Zone::UTC,
        desktop_font: tairix_riscos_kernel::desktop::DesktopFont::SHIPPED,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
        cache_key: tairix_hash::HashSeed::UNKEYED,
        show: None,
    }
}

const fn nothing(_: &Seen) -> Vec<Act> {
    Vec::new()
}

/// What a session came to: how it ended, what the front end saw, and when
/// the last damage came.
struct Served {
    end: End,
    seen: Vec<Seen>,
    damaged_at: u64,
}

/// How a test's front end is set up beside its script.
#[derive(Clone, Copy)]
struct Fronting<'t> {
    keys: &'t [u8],
    disc: Option<&'t Tree>,
    react: fn(&Seen) -> Vec<Act>,
    giving: Giving,
    close_at: Option<u64>,
    parked_answer_after: Option<u64>,
    /// Commands sent before the script whatever the front end was told.
    breaches: &'t [Command<'static>],
    /// The slot each launch is given.
    slot_bytes: u32,
}

impl Default for Fronting<'_> {
    fn default() -> Self {
        Self {
            keys: b"",
            disc: None,
            react: nothing,
            giving: Giving::AtOnce,
            close_at: None,
            parked_answer_after: None,
            breaches: &[],
            slot_bytes: 0x8_0000,
        }
    }
}

/// A session served from `script`, its front end set up as `fronting`.
fn served(script: &[Command<'static>], fronting: Fronting<'_>) -> Served {
    let share = HeapShare::new(RING_BYTES as usize).expect("the ring");
    let server = fronting.disc.map(|tree| {
        let given = Given {
            dir: tree.open_dir("/disc", false).expect("the disc"),
            name: "Disc".to_string(),
            writable: true,
        };
        Server::new(None, vec![given], [3; 16], 1)
    });
    let front = Rc::new(RefCell::new(Driver {
        commands: VecDeque::new(),
        later: VecDeque::new(),
        parked_answer_after: fronting.parked_answer_after,
        seen: Vec::new(),
        clock: 0,
        close_at: fronting.close_at,
        recvs: 0,
        spun: false,
        damaged_at: 0,
        model: Front::new().expect("room"),
        giving: fronting.giving,
        share: share.clone(),
        server,
        keys: fronting.keys.iter().copied().collect(),
        typing: None,
        react: fronting.react,
    }));
    for &command in fronting.breaches {
        front.borrow_mut().act(Act::Breach(command));
    }
    for &command in script {
        front.borrow_mut().act(Act::Send(command));
    }
    let setup = Setup {
        slot_bytes: fronting.slot_bytes,
        real_time: 0x58_0000_0000,
    };
    let end = serve(TestPort(Rc::clone(&front)), share, setup, config);
    let mut front = front.borrow_mut();
    assert!(
        !front.spun,
        "the worker read its port {RECV_MOST} times: it spun"
    );
    Served {
        end,
        seen: std::mem::take(&mut front.seen),
        damaged_at: front.damaged_at,
    }
}

/// A session served from `script`, the keys `keys` typed into its first
/// launch, its disc `disc` where there is one, `react` done on each event:
/// how it ended, and what the front end saw.
fn session(
    script: &[Command<'static>],
    keys: &[u8],
    disc: Option<&Tree>,
    react: fn(&Seen) -> Vec<Act>,
) -> (End, Vec<Seen>) {
    let fronting = Fronting {
        keys,
        disc,
        react,
        ..Fronting::default()
    };
    let served = served(script, fronting);
    (served.end, served.seen)
}

/// The session run, and `line` launched in it.
const fn launch(line: &'static [u8]) -> [Command<'static>; 2] {
    [Command::Run, Command::Launch { serial: 0, line }]
}

/// The events of one kind, in order.
fn of<T>(seen: &[Seen], pick: impl Fn(&Seen) -> Option<T>) -> Vec<T> {
    seen.iter().filter_map(pick).collect()
}

/// The events but the relays, which HostFS's start makes whatever runs.
fn unrelayed(seen: &[Seen]) -> Vec<Seen> {
    of(seen, |s| (!matches!(s, Seen::Relay(..))).then(|| s.clone()))
}

/// Whether `seen` holds a relay to the files.
fn relayed_files(seen: &[Seen]) -> bool {
    seen.iter()
        .any(|s| matches!(s, Seen::Relay(Side::Files, _)))
}

/// `words` as an absolute program, `name` at `&8100`.
fn program(words: &[u32], name: &[u8]) -> Vec<u8> {
    assembled(words, &[(0x100, name)])
}

/// `words` as an absolute program, each of `strings` at its offset.
fn assembled(words: &[u32], strings: &[(usize, &[u8])]) -> Vec<u8> {
    let mut image: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    for &(at, bytes) in strings {
        image.resize(at, 0);
        image.extend_from_slice(bytes);
    }
    image
}

#[test]
fn a_launch_runs_its_command_shows_its_screen_and_ends_once_it_is_done() {
    let (end, seen) = session(&launch(b"Echo Hello"), b"", None, nothing);
    assert_eq!(end, End::Closed);
    assert_eq!(unrelayed(&seen).first(), Some(&Seen::Launched(0, 1)));
    let screens = of(&seen, |s| matches!(s, Seen::Screen(1, _)).then_some(()));
    assert_eq!(screens.len(), 1, "its screen described once");
    let pixels = of(&seen, |s| match s {
        Seen::Damage(1, _, _, pixels) => Some(pixels.clone()),
        _ => None,
    });
    assert!(!pixels.is_empty(), "its lines sent");
    assert!(
        pixels.iter().flatten().any(|&pixel| pixel != 0),
        "the text it drew among them"
    );
    assert_eq!(seen.last(), Some(&Seen::Ended(1, "exit 0".into())));
}

#[test]
fn a_command_that_fails_ends_its_context_with_its_error() {
    let (end, seen) = session(&launch(b"NoSuchCommandAnywhere"), b"", None, nothing);
    assert_eq!(end, End::Closed);
    let seen = unrelayed(&seen);
    assert_eq!(seen.first(), Some(&Seen::Launched(0, 1)), "{seen:?}");
    assert!(
        matches!(seen.last(), Some(Seen::Ended(1, ending)) if ending.starts_with("error ")),
        "{seen:?}"
    );
    assert!(!seen.iter().any(|s| matches!(s, Seen::Refused(..))));
}

#[test]
fn a_launch_waits_until_the_session_runs() {
    let launch = Command::Launch {
        serial: 0,
        line: b"Echo Hello",
    };
    let (end, seen) = session(&[launch], b"", None, nothing);
    assert_eq!(end, End::Closed);
    assert!(
        unrelayed(&seen).is_empty(),
        "nothing made while held: {seen:?}"
    );
    let (end, seen) = session(&[launch, Command::Run], b"", None, nothing);
    assert_eq!(end, End::Closed);
    assert_eq!(unrelayed(&seen).first(), Some(&Seen::Launched(0, 1)));
    assert_eq!(seen.last(), Some(&Seen::Ended(1, "exit 0".into())));
}

#[test]
fn typed_keys_reach_basic_within_its_credit_and_its_quit_ends_it() {
    let mut keys = b"PRINT 6*7\r".repeat(10);
    keys.extend_from_slice(b"QUIT\r");
    let (end, seen) = session(&launch(b"BASIC"), &keys, None, nothing);
    assert_eq!(end, End::Closed);
    let credits = of(&seen, |s| match s {
        Seen::Credit(1, applied) => Some(*applied),
        _ => None,
    });
    assert!(
        credits.windows(2).all(|pair| pair[0] < pair[1]),
        "{credits:?}"
    );
    assert_eq!(
        credits.last().copied(),
        Some(u32::try_from(keys.len()).expect("few"))
    );
    assert_eq!(seen.last(), Some(&Seen::Ended(1, "exit 0".into())));
}

#[test]
fn what_basic_draws_is_shown_while_it_waits_for_a_key() {
    let (end, seen) = session(&launch(b"BASIC"), b"", None, nothing);
    assert_eq!(end, End::Closed, "the front end gone while BASIC waited");
    let drawn = of(&seen, |s| match s {
        Seen::Damage(1, _, _, pixels) => Some(pixels.iter().any(|&pixel| pixel != 0)),
        _ => None,
    });
    assert!(drawn.contains(&true), "its banner and prompt shown");
}

/// An absolute program that reads the catalogue information of `note` on
/// the disc with its own `OS_File`, then exits with the object type.
fn catalogue_program() -> Vec<u8> {
    program(
        &[
            0xE3A0_0005, // MOV R0, #5
            0xE28F_10F4, // ADR R1, the name at &8100
            0xEF02_0008, // SWI XOS_File
            0xE1A0_2000, // MOV R2, R0
            0xE59F_1008, // LDR R1, "ABEX"
            0xE3A0_0000, // MOV R0, #0
            0xEF00_0011, // SWI OS_Exit
            0,
            0x5845_4241, // "ABEX"
        ],
        b"HostFS::Disc.$.note\0",
    )
}

#[test]
fn a_program_s_own_filing_swi_is_answered_across_the_relay() {
    let tree = Tree::new()
        .dir("/disc")
        .file("/disc/note,fff", b"Contents")
        .file("/disc/prog,ff8", &catalogue_program());
    let (end, seen) = session(
        &launch(b"Run HostFS::Disc.$.prog"),
        b"",
        Some(&tree),
        nothing,
    );
    assert_eq!(end, End::Closed);
    assert_eq!(
        seen.last(),
        Some(&Seen::Ended(1, "exit 1".into())),
        "a file found: {seen:?}"
    );
}

/// An absolute program that clears the screen eight times over, each in a
/// turn of its own, then exits with the object type of `note` on the disc.
fn clearing_catalogue_program() -> Vec<u8> {
    program(
        &[
            0xE3A0_4008, // MOV R4, #8
            0xE3A0_000C, // MOV R0, #12
            0xEF00_0000, // SWI OS_WriteC
            0xE3A0_3601, // MOV R3, #&100000
            0xE253_3001, // SUBS R3, R3, #1
            0x1AFF_FFFD, // BNE the SUBS
            0xE254_4001, // SUBS R4, R4, #1
            0x1AFF_FFF8, // BNE the MOV R0
            0xE3A0_0005, // MOV R0, #5
            0xE28F_10D4, // ADR R1, the name at &8100
            0xEF02_0008, // SWI XOS_File
            0xE1A0_2000, // MOV R2, R0
            0xE59F_1008, // LDR R1, "ABEX"
            0xE3A0_0000, // MOV R0, #0
            0xEF00_0011, // SWI OS_Exit
            0,
            0x5845_4241, // "ABEX"
        ],
        b"HostFS::Disc.$.note\0",
    )
}

#[test]
fn a_context_shown_while_the_session_is_held_keeps_the_reply_it_is_parked_on() {
    let tree = Tree::new()
        .dir("/disc")
        .file("/disc/note,fff", b"Contents")
        .file("/disc/prog,ff8", &clearing_catalogue_program());
    let fronting = Fronting {
        disc: Some(&tree),
        giving: Giving::Withholding {
            kept: 0,
            trigger: b"note",
            holding: true,
        },
        ..Fronting::default()
    };
    let Served { end, seen, .. } = served(&launch(b"Run HostFS::Disc.$.prog"), fronting);
    assert_eq!(end, End::Closed);
    let screen = of(&seen, |s| match s {
        Seen::Screen(1, screen) => Some(*screen),
        _ => None,
    });
    let held = seen.iter().position(|s| matches!(s, Seen::Withheld(_)));
    let (Some(screen), Some(held)) = (screen.first(), held) else {
        panic!("its screen, and the session held as it parked: {seen:?}");
    };
    let Seen::Withheld(kept) = seen[held] else {
        unreachable!("found as one");
    };
    assert!(
        RING_BYTES - kept < screen.width * screen.height * 4,
        "no room for its cleared screen when it parked"
    );
    assert!(
        seen[held..]
            .iter()
            .any(|s| matches!(s, Seen::Damage(1, ..))),
        "the lines it drew before it parked shown while the session was held"
    );
    assert_eq!(
        seen.last(),
        Some(&Seen::Ended(1, "exit 1".into())),
        "run again with the reply once the session runs"
    );
}

/// An absolute program that clears the screen eight times over, each in a
/// turn of its own, then waits for a key it is never given.
fn clearing_waiting_program() -> Vec<u8> {
    program(
        &[
            0xE3A0_4008, // MOV R4, #8
            0xE3A0_000C, // MOV R0, #12
            0xEF00_0000, // SWI OS_WriteC
            0xE3A0_3601, // MOV R3, #&100000
            0xE253_3001, // SUBS R3, R3, #1
            0x1AFF_FFFD, // BNE the SUBS
            0xE254_4001, // SUBS R4, R4, #1
            0x1AFF_FFF8, // BNE the MOV R0
            0xEF00_0004, // SWI OS_ReadC
            0xEF00_0011, // SWI OS_Exit
        ],
        b"",
    )
}

/// An absolute program that runs on for longer than the clearing one takes
/// to clear its screens, reads the catalogue information of `trigger` on the
/// disc, runs on a little more, then exits.
fn busy_program() -> Vec<u8> {
    program(
        &[
            0xE3A0_3503, // MOV R3, #&C00000
            0xE253_3001, // SUBS R3, R3, #1
            0x1AFF_FFFD, // BNE the SUBS
            0xE3A0_0005, // MOV R0, #5
            0xE28F_10E8, // ADR R1, the name at &8100
            0xEF02_0008, // SWI XOS_File
            0xE3A0_3501, // MOV R3, #&400000
            0xE253_3001, // SUBS R3, R3, #1
            0x1AFF_FFFD, // BNE the SUBS
            0xE3A0_0000, // MOV R0, #0
            0xE59F_1004, // LDR R1, "ABEX"
            0xE3A0_2000, // MOV R2, #0
            0xEF00_0011, // SWI OS_Exit
            0x5845_4241, // "ABEX"
        ],
        b"HostFS::Disc.$.trigger\0",
    )
}

#[test]
fn a_waiting_context_s_lines_are_shown_while_another_runs() {
    let tree = Tree::new()
        .dir("/disc")
        .file("/disc/trigger,fff", b"")
        .file("/disc/clear,ff8", &clearing_waiting_program())
        .file("/disc/busy,ff8", &busy_program());
    let busy = |seen: &Seen| match seen {
        Seen::Launched(_, 1) => vec![Act::Send(Command::Launch {
            serial: 0,
            line: b"Run HostFS::Disc.$.busy",
        })],
        _ => Vec::new(),
    };
    let fronting = Fronting {
        disc: Some(&tree),
        react: busy,
        giving: Giving::Withholding {
            kept: 0,
            trigger: b"trigger",
            holding: false,
        },
        ..Fronting::default()
    };
    let Served { end, seen, .. } = served(&launch(b"Run HostFS::Disc.$.clear"), fronting);
    assert_eq!(end, End::Closed);
    let given = seen.iter().position(|s| matches!(s, Seen::Withheld(_)));
    let busy_ended = seen
        .iter()
        .position(|s| *s == Seen::Ended(2, "exit 0".into()));
    let (Some(given), Some(busy_ended)) = (given, busy_ended) else {
        panic!("the ring given back, and the busy program's end: {seen:?}");
    };
    let shown = seen[given..]
        .iter()
        .position(|s| matches!(s, Seen::Damage(1, ..)))
        .map(|at| given + at);
    assert!(
        shown.is_some_and(|shown| shown < busy_ended),
        "the waiting screen's lines sent while the busy one ran: {seen:?}"
    );
}

/// BASIC told to spool to a file on the disc, then to print.
const SPOOLING: &[u8] = b"*Spool HostFS::Disc.$.log\rPRINT 6*7\r";

#[test]
fn what_a_context_s_end_relays_is_told_before_its_end() {
    let tree = Tree::new().dir("/disc");
    let quit_once_typed = |seen: &Seen| match *seen {
        Seen::Credit(context, applied) if applied as usize == SPOOLING.len() => {
            vec![Act::Send(Command::Quit { context })]
        }
        _ => Vec::new(),
    };
    let (end, seen) = session(&launch(b"BASIC"), SPOOLING, Some(&tree), quit_once_typed);
    assert_eq!(end, End::Closed);
    let quit = seen
        .iter()
        .position(|s| *s == Seen::Credit(1, len32(SPOOLING)));
    let ended = seen
        .iter()
        .position(|s| *s == Seen::Ended(1, "quit".into()));
    let (Some(quit), Some(ended)) = (quit, ended) else {
        panic!("quit once its keys were taken in, and ended: {seen:?}");
    };
    assert!(
        relayed_files(&seen[quit..ended]),
        "the spool file its end closed: {seen:?}"
    );
}

fn len32(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).expect("few")
}

#[test]
fn the_session_s_timed_work_is_done_while_every_context_waits() {
    // A timed call half a second on that types an A into the keyboard
    // buffer, while BASIC waits at its prompt.
    let keys = b"DIM C% 24:C%!0=&E92D4007:C%!4=&E3A0008A:C%!8=&E3A01000:\
C%!12=&E3A02041:C%!16=&EF020006:C%!20=&E8BD8007:SYS &3B,50,C%\r";
    let fronting = Fronting {
        keys,
        close_at: Some(1000 * CENTISECOND),
        ..Fronting::default()
    };
    let served = served(&launch(b"BASIC"), fronting);
    assert_eq!(served.end, End::Closed);
    assert!(
        served.damaged_at >= 50 * CENTISECOND,
        "the A it typed echoed once the call fell due: {:?}",
        served.seen
    );
}

#[test]
fn a_quit_ends_its_context() {
    let quit = |seen: &Seen| match *seen {
        Seen::Launched(_, context) => vec![Act::Send(Command::Quit { context })],
        _ => Vec::new(),
    };
    let (end, seen) = session(&launch(b"BASIC"), b"", None, quit);
    assert_eq!(end, End::Closed);
    assert_eq!(seen.last(), Some(&Seen::Ended(1, "quit".into())));
}

#[test]
fn relays_cross_to_the_disc_and_are_answered() {
    let tree = Tree::new().dir("/disc").file("/disc/note,fff", b"Contents");
    let (end, seen) = session(
        &launch(b"Type HostFS::Disc.$.note"),
        b"",
        Some(&tree),
        nothing,
    );
    assert_eq!(end, End::Closed);
    assert!(relayed_files(&seen), "the file read across the relay");
    assert_eq!(seen.last(), Some(&Seen::Ended(1, "exit 0".into())));
}

#[test]
fn two_contexts_run_side_by_side_each_with_its_own_input() {
    let second = |seen: &Seen| match *seen {
        Seen::Launched(_, 1) => vec![Act::Send(Command::Launch {
            serial: 0,
            line: b"BASIC",
        })],
        Seen::Launched(_, context) => vec![
            Act::Send(Command::Input {
                context,
                event: InputEvent::Key(b'Q'),
            }),
            Act::Send(Command::Quit { context }),
        ],
        _ => Vec::new(),
    };
    let (end, seen) = session(&launch(b"BASIC"), b"QUIT\r", None, second);
    assert_eq!(end, End::Closed);
    assert!(seen.contains(&Seen::Launched(1, 2)));
    assert!(seen.contains(&Seen::Ended(1, "exit 0".into())), "{seen:?}");
    assert!(seen.contains(&Seen::Ended(2, "quit".into())));
    let screened = of(&seen, |s| match s {
        Seen::Screen(context, _) => Some(*context),
        _ => None,
    });
    assert!(
        screened.contains(&1) && screened.contains(&2),
        "each its own screen"
    );
}

#[test]
fn a_front_end_that_breaks_the_protocol_ends_the_session() {
    for (breach, why) in [
        (
            Command::Released { bytes: 1 },
            "more of the ring given back than was held",
        ),
        (
            Command::Answer {
                serial: 99,
                reply: b"",
            },
            "an answer to no relay",
        ),
        (
            Command::Quit { context: 9 },
            "a quit of a context never made",
        ),
        (
            Command::Quit { context: 0 },
            "a quit of a context never made",
        ),
    ] {
        let fronting = Fronting {
            breaches: &[breach],
            ..Fronting::default()
        };
        assert_eq!(served(&[], fronting).end, End::Violation(why));
    }
}

#[test]
fn input_beyond_a_context_s_credit_ends_the_session() {
    let flood = |seen: &Seen| match *seen {
        Seen::Launched(_, context) => (0..=64)
            .map(|_| {
                Act::Breach(Command::Input {
                    context,
                    event: InputEvent::Key(b'x'),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    let (end, _) = session(&launch(b"BASIC"), b"", None, flood);
    assert_eq!(end, End::Violation("input beyond its credit"));
}

/// How long the front end takes to answer a relay its context is parked on
/// in the tests that delay them.
const SLOW_ANSWER: u64 = 100 * CENTISECOND;

/// An absolute program that enables the vertical sync event, so that its
/// screen's next frame is always its own timed work, then reads the
/// catalogue information of `note` on the disc and exits with its type.
fn framing_catalogue_program() -> Vec<u8> {
    program(
        &[
            0xE3A0_000E, // MOV R0, #14
            0xE3A0_1004, // MOV R1, #4
            0xEF02_0006, // SWI XOS_Byte
            0xE3A0_0005, // MOV R0, #5
            0xE28F_10E8, // ADR R1, the name at &8100
            0xEF02_0008, // SWI XOS_File
            0xE1A0_2000, // MOV R2, R0
            0xE59F_1008, // LDR R1, "ABEX"
            0xE3A0_0000, // MOV R0, #0
            0xEF00_0011, // SWI OS_Exit
            0,
            0x5845_4241, // "ABEX"
        ],
        b"HostFS::Disc.$.note\0",
    )
}

#[test]
fn a_context_parked_on_a_relay_waits_its_reply_out_though_its_own_work_falls_due() {
    let tree = Tree::new()
        .dir("/disc")
        .file("/disc/note,fff", b"Contents")
        .file("/disc/prog,ff8", &framing_catalogue_program());
    let fronting = Fronting {
        disc: Some(&tree),
        parked_answer_after: Some(SLOW_ANSWER),
        ..Fronting::default()
    };
    let Served { end, seen, .. } = served(&launch(b"Run HostFS::Disc.$.prog"), fronting);
    assert_eq!(end, End::Closed);
    assert!(seen.contains(&Seen::Relay(Side::Files, true)), "{seen:?}");
    assert_eq!(seen.last(), Some(&Seen::Ended(1, "exit 1".into())));
}

#[test]
fn a_context_that_cannot_be_set_aside_waits_without_another_s_deadline_spinning_the_worker() {
    // The first waits for a key for half a second; the second waits for
    // one in Supervisor mode, where it cannot be set aside.
    let tree = Tree::new()
        .dir("/disc")
        .file(
            "/disc/timed,ff8",
            &program(
                &[
                    0xE3A0_0081, // MOV R0, #129
                    0xE3A0_1032, // MOV R1, #50
                    0xE3A0_2000, // MOV R2, #0
                    0xEF02_0006, // SWI XOS_Byte
                    0xEF00_0011, // SWI OS_Exit
                ],
                b"",
            ),
        )
        .file(
            "/disc/held,ff8",
            &program(
                &[
                    0xEF02_0016, // SWI XOS_EnterOS
                    0xEF02_0004, // SWI XOS_ReadC
                    0xEAFF_FFFE, // B the B
                ],
                b"",
            ),
        );
    let second = |seen: &Seen| match *seen {
        Seen::Launched(_, 1) => vec![Act::Send(Command::Launch {
            serial: 0,
            line: b"Run HostFS::Disc.$.held",
        })],
        _ => Vec::new(),
    };
    let fronting = Fronting {
        disc: Some(&tree),
        react: second,
        ..Fronting::default()
    };
    let Served { end, seen, .. } = served(&launch(b"Run HostFS::Disc.$.timed"), fronting);
    assert_eq!(end, End::Closed, "{seen:?}");
    assert!(seen.contains(&Seen::Launched(1, 2)), "{seen:?}");
}

#[test]
fn a_context_quit_while_parked_on_a_relay_closes_its_files_as_it_ends() {
    let words = [
        0xE28F_0038, // ADR R0, the command at &8040
        0xEF02_0005, // SWI XOS_CLI
        0xE3A0_0005, // MOV R0, #5
        0xE28F_106C, // ADR R1, the name at &8080
        0xEF02_0008, // SWI XOS_File
        0xEF00_0011, // SWI OS_Exit
    ];
    let image = assembled(
        &words,
        &[
            (0x40, b"Spool HostFS::Disc.$.log\0"),
            (0x80, b"HostFS::Disc.$.note\0"),
        ],
    );
    let tree = Tree::new()
        .dir("/disc")
        .file("/disc/note,fff", b"Contents")
        .file("/disc/prog,ff8", &image);
    let quit_once_parked = |seen: &Seen| match *seen {
        Seen::Relay(_, true) => vec![Act::Send(Command::Quit { context: 1 })],
        _ => Vec::new(),
    };
    let fronting = Fronting {
        disc: Some(&tree),
        react: quit_once_parked,
        parked_answer_after: Some(SLOW_ANSWER),
        ..Fronting::default()
    };
    let Served { end, seen, .. } = served(&launch(b"Run HostFS::Disc.$.prog"), fronting);
    assert_eq!(end, End::Closed);
    let parked = seen
        .iter()
        .position(|s| *s == Seen::Relay(Side::Files, true));
    let ended = seen
        .iter()
        .position(|s| *s == Seen::Ended(1, "quit".into()));
    let (Some(parked), Some(ended)) = (parked, ended) else {
        panic!("parked on its read, then quit: {seen:?}");
    };
    assert!(
        seen[parked..ended].contains(&Seen::Relay(Side::Files, false)),
        "its spool file closed in place as it ended: {seen:?}"
    );
}

#[test]
fn an_honest_front_end_never_has_more_waiting_than_the_worker_keeps() {
    let mut script = vec![
        Command::Launch {
            serial: 0,
            line: b"Echo x",
        };
        WAITING_MOST + 6
    ];
    script.push(Command::Run);
    let Served { end, seen, .. } = served(&script, Fronting::default());
    assert_eq!(end, End::Closed, "no launch beyond what the worker keeps");
    let launched = of(&seen, |s| matches!(s, Seen::Launched(..)).then_some(()));
    assert_eq!(
        launched.len(),
        WAITING_MOST,
        "those the model asked, each made"
    );
}

#[test]
fn what_a_launch_s_making_relays_is_the_session_s_own() {
    let tree = Tree::new().dir("/disc").file("/disc/note,fff", b"Contents");
    let launch_once_claimed = |seen: &Seen| match *seen {
        Seen::Credit(1, applied) if applied as usize == CLAIMING.len() => {
            vec![Act::Send(Command::Launch {
                serial: 0,
                line: b"Echo x",
            })]
        }
        _ => Vec::new(),
    };
    let fronting = Fronting {
        keys: CLAIMING,
        disc: Some(&tree),
        react: launch_once_claimed,
        ..Fronting::default()
    };
    let Served { end, seen, .. } = served(&launch(b"BASIC"), fronting);
    assert_eq!(end, End::Closed);
    let asked = seen
        .iter()
        .position(|s| *s == Seen::Credit(1, len32(CLAIMING)));
    let made = seen.iter().position(|s| *s == Seen::Launched(1, 2));
    let (Some(asked), Some(made)) = (asked, made) else {
        panic!("the claim made, then the second launched: {seen:?}");
    };
    assert!(
        relayed_files(&seen[asked..made]),
        "the claimant's read as the second's slot was made: {seen:?}"
    );
    assert!(seen.contains(&Seen::Ended(2, "exit 0".into())), "{seen:?}");
}

#[test]
fn a_launch_the_machine_has_no_room_for_is_refused_and_the_session_goes_on() {
    let beyond_the_machine = Fronting {
        slot_bytes: 128 << 20,
        ..Fronting::default()
    };
    let mut script = launch(b"Echo x").to_vec();
    script.push(Command::Launch {
        serial: 0,
        line: b"Echo y",
    });
    let Served { end, seen, .. } = served(&script, beyond_the_machine);
    assert_eq!(end, End::Closed, "the session went on");
    assert_eq!(
        unrelayed(&seen),
        [Seen::Refused(0, None), Seen::Refused(1, None)],
        "each refused with no error of the guest's"
    );
}

/// BASIC told to put a `ChangeEnvironmentV` claimant in the module area that
/// reads the catalogue information of `note` on the disc: the making of
/// every context's slot changes its environment.
const CLAIMING: &[u8] = b"SYS &1E,6,,,64 TO ,,C%:C%!0=&E92D403F:C%!4=&E3A00005:C%!8=&E28F1008:\
C%!12=&EF020008:C%!16=&E8BD403F:C%!20=&E1A0F00E:$(C%+24)=\"HostFS::Disc.$.note\":\
SYS &1F,&1E,C%,0\r";

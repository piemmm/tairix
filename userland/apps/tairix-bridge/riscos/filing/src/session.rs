//! Test support for the crates above the kernel: a session made for an
//! integration test, its host's clock moving on as the guest waits, its
//! keyboard typing what the test gives, a wait with nothing left to type
//! stopping the run, and HostFS serving `$` over a test's tree, its relays
//! left pending where a test asks; and the commands run in it. Test support
//! panics where a fixture names nothing.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fscopy::tree::Roots;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::layout::{APPLICATION, RISC_OS_5};
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{
    Config, Ending, Fail, Host, Kernel, Os, Outcome, Pulse, Regs, Wait, MILLENNIUM,
};
use tairix_riscos_kernel::output::VduDrivers;
use tairix_riscos_kernel::relay::{Relayed, Side};
use tairix_riscos_kernel::swi::os::{CHANGE_ENVIRONMENT, CHANGE_REDIRECTION, CLI, FIND};
use tairix_riscos_kernel::swi::X;

use crate::hostfs::server::{Given, Server};

/// The folder of a test's tree that HostFS serves as `$`.
pub const DISC: &str = "/disc";

/// The application slot of a test's session that needs no particular size.
pub const SLOT_BYTES: u32 = 0x10_0000;

/// A host whose clock moves on as the guest waits, whose keyboard types the
/// keys a test gives, which stops a wait with nothing left to type — unless
/// the test lets the wait go on to the timed work due next — which stops
/// work once the words a test allows are spent, and which serves HostFS
/// where a test gives it a disc, leaving each relay that may park pending
/// once the test asks it to.
struct ScriptHost {
    clock: Rc<Cell<u64>>,
    keys: Rc<RefCell<VecDeque<u8>>>,
    words: Rc<Cell<Option<u32>>>,
    timed: Rc<Cell<bool>>,
    disc: Disc,
    parking: Rc<Cell<bool>>,
    parked: Parked,
    fonts: Fonts,
}

/// The server of a test's disc, which the session answers parked relays by.
type Disc = Rc<RefCell<Option<Server<Handle>>>>;

/// The request a relay left pending.
type Parked = Rc<RefCell<Option<Vec<u8>>>>;

/// The font service a test's session serves font requests through, if it
/// has been given one.
type Fonts = Rc<RefCell<Option<FontService>>>;

/// A font service: a request, and the reply it appends; whether it answered.
pub type FontService = Box<dyn FnMut(&[u8], &mut Vec<u8>) -> bool>;

impl Host for ScriptHost {
    fn monotonic(&mut self) -> u64 {
        self.clock.get()
    }

    fn real_time(&mut self) -> u64 {
        MILLENNIUM + self.clock.get()
    }

    fn pulse(&mut self) -> Pulse {
        match self.words.get() {
            None => Pulse::Continue,
            Some(0) => Pulse::Stop,
            Some(left) => {
                self.words.set(Some(left - 1));
                Pulse::Continue
            }
        }
    }

    fn wait(&mut self, until: Option<u64>, ending: Ending) -> Pulse {
        if !self.keys.borrow().is_empty() {
            return Pulse::Continue;
        }
        match (until, ending) {
            (Some(at), Ending::Clock) => {
                self.clock.set(self.clock.get().max(at));
                Pulse::Continue
            }
            (Some(at), Ending::Input) if self.timed.get() => {
                self.clock.set(self.clock.get().max(at));
                Pulse::Continue
            }
            _ => Pulse::Stop,
        }
    }

    fn input(&mut self) -> Option<InputEvent> {
        self.keys.borrow_mut().pop_front().map(InputEvent::Key)
    }

    fn relay(&mut self, side: Side, request: &[u8], reply: &mut Vec<u8>, parking: bool) -> Relayed {
        match side {
            Side::Files => {
                let mut disc = self.disc.borrow_mut();
                let Some(server) = disc.as_mut() else {
                    return Relayed::Failed;
                };
                if parking && self.parking.get() {
                    *self.parked.borrow_mut() = Some(request.to_vec());
                    return Relayed::Pending;
                }
                server.serve(request, reply);
                Relayed::Answered
            }
            Side::Fonts => {
                let answered = self
                    .fonts
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(|service| service(request, reply));
                if answered {
                    Relayed::Answered
                } else {
                    Relayed::Failed
                }
            }
        }
    }
}

/// What a test's session is made with.
pub struct Setup<'a> {
    /// The VDU drivers, if it has any.
    pub vdu: Option<VduDrivers>,
    /// Its built-in modules.
    pub modules: &'static [&'static Builtin],
    /// Its application slot's bytes.
    pub bytes: u32,
    /// What its keyboard types.
    pub keys: &'a [u8],
    /// The tree whose [`DISC`] folder HostFS serves as `$`, writable.
    pub disc: Option<&'a Tree>,
}

/// A test's session: its kernel, and the clock, words, timed waking, disc,
/// parked relay and font service its host keeps.
pub struct Session {
    /// Its kernel.
    pub kernel: Kernel,
    clock: Rc<Cell<u64>>,
    words: Rc<Cell<Option<u32>>>,
    timed: Rc<Cell<bool>>,
    disc: Disc,
    parking: Rc<Cell<bool>>,
    parked: Parked,
    relays_parked: u32,
    fonts: Fonts,
}

impl Session {
    /// How many relays [`Self::run`] has answered that a SWI parked on.
    #[must_use]
    pub const fn relays_parked(&self) -> u32 {
        self.relays_parked
    }

    /// Lets a wait for input go on, from now on, to the timed work due next
    /// — a ticker, an event — where there is some, as a real host's would,
    /// rather than stopping the run.
    pub fn wake_for_timed_work(&self, wake: bool) {
        self.timed.set(wake);
    }

    /// Serves the session's font requests through `service` from now on.
    pub fn serve_fonts(&self, service: FontService) {
        *self.fonts.borrow_mut() = Some(service);
    }

    /// Lets work go on for `words` more of the host's words, each after a
    /// slice of work, before the host stops it; `None` never stops it.
    pub fn allow_words(&self, words: Option<u32>) {
        self.words.set(words);
    }

    /// Leaves each relay the kernel lets park pending from now on, as a
    /// worker leaves one whose reply is on its way, or answers every relay
    /// at once again.
    pub fn park_relays(&self, park: bool) {
        self.parking.set(park);
    }

    /// Runs the session for at most `budget` instructions a turn, a turn
    /// that leaves it waiting for the clock — or, while
    /// [`Self::wake_for_timed_work`] lets it, for input until a time — taken
    /// again once the clock is moved on to then, and one that leaves it
    /// waiting on a relay taken again once the disc has answered it: its
    /// outcome otherwise.
    pub fn run(&mut self, budget: u64) -> Outcome {
        loop {
            match self.kernel.run(budget) {
                Outcome::Waiting(Wait::Clock { until }) => {
                    self.clock.set(self.clock.get().max(until));
                }
                Outcome::Waiting(Wait::Input { until: Some(until) }) if self.timed.get() => {
                    self.clock.set(self.clock.get().max(until));
                }
                Outcome::Waiting(Wait::Relay) => {
                    let request = self.parked.borrow_mut().take().expect("a parked relay");
                    let mut reply = Vec::new();
                    self.disc
                        .borrow_mut()
                        .as_mut()
                        .expect("a disc")
                        .serve(&request, &mut reply);
                    assert!(self.kernel.os().relay_answered(&reply), "its reply taken");
                    self.relays_parked += 1;
                }
                outcome => return outcome,
            }
        }
    }
}

/// A session made as `setup` says, its application slot current.
#[must_use]
pub fn session(setup: &Setup<'_>) -> Session {
    let clock = Rc::new(Cell::new(0));
    let words = Rc::new(Cell::new(None));
    let timed = Rc::new(Cell::new(false));
    let fonts: Fonts = Rc::new(RefCell::new(None));
    let keys = Rc::new(RefCell::new(setup.keys.iter().copied().collect()));
    let disc: Disc = Rc::new(RefCell::new(setup.disc.map(|tree| {
        let given = Given {
            dir: tree.open_dir(DISC, false).expect("the disc's folder"),
            name: "Disc".to_string(),
            writable: true,
        };
        Server::new(None, vec![given], [9; 16], 1)
    })));
    let parking = Rc::new(Cell::new(false));
    let parked: Parked = Rc::new(RefCell::new(None));
    let config = Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(256 * 1024 * 1024),
        memo_pages: 16,
        host: Box::new(ScriptHost {
            clock: Rc::clone(&clock),
            keys,
            words: Rc::clone(&words),
            timed: Rc::clone(&timed),
            disc: Rc::clone(&disc),
            parking: Rc::clone(&parking),
            parked: Rc::clone(&parked),
            fonts: Rc::clone(&fonts),
        }),
        vdu: setup.vdu,
        modules: setup.modules,
        zone: tairix_tz::Zone::UTC,
        desktop_font: tairix_riscos_kernel::desktop::DesktopFont::SHIPPED,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
        cache_key: tairix_hash::HashSeed::UNKEYED,
        show: None,
    };
    let mut kernel = Kernel::new(config).expect("a kernel");
    application_slot(&mut kernel, setup.bytes);
    Session {
        kernel,
        clock,
        words,
        timed,
        disc,
        parking,
        parked,
        relays_parked: 0,
        fonts,
    }
}

/// An application slot of `bytes` made current in `kernel`, the memory
/// limit at its end, as the front end gives a program one.
pub fn application_slot(kernel: &mut Kernel, bytes: u32) {
    let memory = kernel.memory();
    let slot = memory.create_slot(Bits::Bit32).expect("a slot");
    memory.set_slot_size(slot, bytes).expect("its memory");
    memory.switch_slot(slot).expect("made current");
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[0, APPLICATION + bytes]);
    kernel
        .os()
        .perform(X | CHANGE_ENVIRONMENT, &mut regs)
        .expect("its limit");
}

/// `line` run as a command from a block of the RMA: the transfer to an
/// application it started, or the first refusal met claiming, writing,
/// running or freeing that block.
pub fn command(os: &mut Os<'_>, line: &str) -> Result<(), Fail> {
    let length = u32::try_from(line.len() + 1).expect("short");
    let at = os.rma_claim(length)?;
    let ran = os
        .write_terminated(at, line.as_bytes(), 13, line.len() + 1)
        .and_then(|()| {
            let mut regs = Regs::default();
            regs.r[0] = at;
            os.perform(X | CLI, &mut regs)
        });
    ran.and(os.rma_free(at))
}

/// The redirection a command line set up ended and its files closed, as
/// the command line that ran it ends them.
pub fn end_redirection(os: &mut Os<'_>) {
    let mut ended = Regs::default();
    os.perform(X | CHANGE_REDIRECTION, &mut ended)
        .expect("redirection ended");
    for handle in [ended.r[0], ended.r[1]] {
        if handle != 0 {
            let mut close = Regs::default();
            close.r[1] = handle;
            os.perform(X | FIND, &mut close).expect("its file closed");
        }
    }
}

/// The bytes of the file `tree`'s disc holds as `name`, whatever type its
/// host name's suffix gives it.
#[must_use]
pub fn disc_file(tree: &Tree, name: &str) -> Option<Vec<u8>> {
    let host = tree
        .names(DISC)
        .into_iter()
        .find(|host| host.split(',').next() == Some(name))?;
    tree.bytes(&format!("{DISC}/{host}"))
}

/// The filing system's modules, then each of `others`' in turn: what a
/// session of a crate above the filing system starts with.
#[must_use]
pub fn modules_with(others: &[&[&'static Builtin]]) -> Vec<&'static Builtin> {
    let mut modules = crate::MODULES.to_vec();
    for more in others {
        modules.extend(more.iter().copied());
    }
    modules
}

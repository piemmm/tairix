//! Test support for the crates above the kernel: a session made for an
//! integration test, its host's clock moving on as the guest waits, its
//! keyboard typing what the test gives, a wait with nothing left to type
//! stopping the run, and HostFS serving `$` over a test's tree; and the
//! commands run in it. Test support panics where a fixture names nothing.

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
use tairix_riscos_kernel::swi::os::{CHANGE_ENVIRONMENT, CHANGE_REDIRECTION, CLI, FIND};
use tairix_riscos_kernel::swi::X;

use crate::hostfs::server::{Given, Server};

/// The folder of a test's tree that HostFS serves as `$`.
pub const DISC: &str = "/disc";

/// The application slot of a test's session that needs no particular size.
pub const SLOT_BYTES: u32 = 0x10_0000;

/// A host whose clock moves on as the guest waits, whose keyboard types the
/// keys a test gives, which stops a wait with nothing left to type, and
/// which serves HostFS where a test gives it a disc.
struct ScriptHost {
    clock: Rc<Cell<u64>>,
    keys: Rc<RefCell<VecDeque<u8>>>,
    disc: Option<Server<Handle>>,
}

impl Host for ScriptHost {
    fn monotonic(&mut self) -> u64 {
        self.clock.get()
    }

    fn real_time(&mut self) -> u64 {
        MILLENNIUM + self.clock.get()
    }

    fn pulse(&mut self) -> Pulse {
        Pulse::Continue
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
            _ => Pulse::Stop,
        }
    }

    fn input(&mut self) -> Option<InputEvent> {
        self.keys.borrow_mut().pop_front().map(InputEvent::Key)
    }

    fn relay(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool {
        match &mut self.disc {
            Some(server) => {
                server.serve(request, reply);
                true
            }
            None => false,
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

/// A test's session: its kernel, and the clock its host keeps.
pub struct Session {
    /// Its kernel.
    pub kernel: Kernel,
    clock: Rc<Cell<u64>>,
}

impl Session {
    /// Runs the session for at most `budget` instructions a turn, a turn
    /// that leaves it waiting for the clock taken again once the clock is
    /// moved on to then: its outcome otherwise.
    pub fn run(&mut self, budget: u64) -> Outcome {
        loop {
            match self.kernel.run(budget) {
                Outcome::Waiting(Wait::Clock { until }) => {
                    self.clock.set(self.clock.get().max(until));
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
    let keys = Rc::new(RefCell::new(setup.keys.iter().copied().collect()));
    let disc = setup.disc.map(|tree| {
        let given = Given {
            dir: tree.open_dir(DISC, false).expect("the disc's folder"),
            name: "Disc".to_string(),
            writable: true,
        };
        Server::new(None, vec![given], [9; 16], 1)
    });
    let config = Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(256 * 1024 * 1024),
        memo_pages: 16,
        host: Box::new(ScriptHost {
            clock: Rc::clone(&clock),
            keys,
            disc,
        }),
        vdu: setup.vdu,
        modules: setup.modules,
        zone: tairix_tz::Zone::UTC,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
    };
    let mut kernel = Kernel::new(config).expect("a kernel");
    application_slot(&mut kernel, setup.bytes);
    Session { kernel, clock }
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

/// `line` run as a command: the transfer to an application it started.
pub fn command(os: &mut Os<'_>, line: &str) -> Result<(), Fail> {
    let length = u32::try_from(line.len() + 1).expect("short");
    let at = os.rma_claim(length).expect("room");
    os.write_terminated(at, line.as_bytes(), 13, line.len() + 1)
        .expect("written");
    let mut regs = Regs::default();
    regs.r[0] = at;
    os.perform(X | CLI, &mut regs)
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

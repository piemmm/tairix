//! What the BASIC crate's integration tests share: a session with the
//! filing and BASIC modules, whose VDU stream is captured, whose keyboard
//! types what a test scripts and whose HostFS disc a test's tree may be;
//! programs run in it; and the reading of a recording's quoted text.

#![allow(
    dead_code,
    reason = "each test crate including this uses the helpers its own tests need"
)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fscopy::tree::Roots;
use tairix_riscos_filing::hostfs::server::{Given, Server};
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::layout::RISC_OS_5;
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Config, Fail, Host, Kernel, Os, Outcome, Pulse, Regs, Unwind};
use tairix_riscos_kernel::output::VduDrivers;
use tairix_riscos_kernel::swi::os::{CHANGE_ENVIRONMENT, CLI, FIND};
use tairix_riscos_kernel::swi::X;

/// The bytes the VDU drivers were given.
pub struct Screen(pub Vec<u8>);

/// The session's modules: the filing system's and BASIC's.
static MODULES: std::sync::LazyLock<Vec<&'static Builtin>> = std::sync::LazyLock::new(|| {
    let mut modules: Vec<&'static Builtin> = tairix_riscos_filing::MODULES.to_vec();
    modules.extend(tairix_riscos_basic::MODULES.iter().copied());
    modules
});

/// A host whose clock moves on as the guest waits, whose keyboard types the
/// keys a test gives, which stops a wait with nothing left to type, and
/// which serves HostFS where a test gives it a disc.
struct ScriptHost {
    clock: Rc<Cell<u64>>,
    keys: Rc<RefCell<VecDeque<u8>>>,
    disc: Option<Server<Handle>>,
}

/// 1 January 2000, in centiseconds since 1900.
const MILLENNIUM: u64 = 3_155_673_600 * 100;

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

    fn wait(&mut self, until: Option<u64>) -> Pulse {
        if !self.keys.borrow().is_empty() {
            return Pulse::Continue;
        }
        match until {
            Some(at) => {
                self.clock.set(self.clock.get().max(at));
                Pulse::Continue
            }
            None => Pulse::Stop,
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

#[expect(clippy::unnecessary_wraps, reason = "the VDU drivers' signature")]
fn capture(os: &mut Os<'_>, b: u8) -> Result<(), Fail> {
    if let Some(screen) = os.extension::<Screen>() {
        screen.0.push(b);
    }
    Ok(())
}

/// The VDU drivers a test's screen is: every byte kept.
const VDU: VduDrivers = VduDrivers {
    write: capture,
    queued: |_| false,
    printing: |_| false,
    swi: None,
    byte: None,
    word: None,
};

/// A session, an application slot of `bytes`, with `keys` to type.
pub fn kernel(bytes: u32, keys: &[u8]) -> Kernel {
    session(bytes, keys, None)
}

/// As [`kernel`], with no keys, its HostFS disc `$` served over `tree`'s
/// [`DISC`] folder, which it may write.
pub fn hosted(bytes: u32, tree: &Tree) -> Kernel {
    let given = Given {
        dir: tree.open_dir(DISC, false).expect("the disc's folder"),
        name: "Disc".to_string(),
        writable: true,
    };
    session(bytes, &[], Some(Server::new(None, vec![given], [9; 16], 1)))
}

/// The folder of a test's tree that HostFS serves as `$`.
pub const DISC: &str = "/disc";

fn session(bytes: u32, keys: &[u8], disc: Option<Server<Handle>>) -> Kernel {
    let clock = Rc::new(Cell::new(0));
    let keys = Rc::new(RefCell::new(keys.iter().copied().collect()));
    let config = Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(256 * 1024 * 1024),
        memo_pages: 16,
        host: Box::new(ScriptHost { clock, keys, disc }),
        vdu: Some(VDU),
        modules: MODULES.as_slice(),
        zone: tairix_tz::Zone::UTC,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
    };
    let mut kernel = Kernel::new(config).expect("a kernel");
    let memory = kernel.memory();
    let slot = memory.create_slot(Bits::Bit32).expect("a slot");
    memory.set_slot_size(slot, bytes).expect("its memory");
    memory.switch_slot(slot).expect("made current");
    let mut os = kernel.os();
    os.hold_extension(Screen(Vec::new())).expect("room");
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[0, 0x8000 + bytes]);
    os.perform(X | CHANGE_ENVIRONMENT, &mut regs)
        .expect("its limit");
    kernel
}

/// `line` run as a command: the transfer to an application it started.
pub fn command(os: &mut Os<'_>, line: &str) -> Result<(), Fail> {
    let at = os
        .rma_claim(u32::try_from(line.len() + 1).expect("short"))
        .expect("room");
    os.write_terminated(at, line.as_bytes(), 13, line.len() + 1)
        .expect("written");
    let mut regs = Regs::default();
    regs.r[0] = at;
    os.perform(X | CLI, &mut regs)
}

/// What a run of a program left.
pub struct Run {
    /// How the session's run ended.
    pub outcome: Outcome,
    /// The bytes written to the screen.
    pub screen: Vec<u8>,
}

impl Run {
    /// The screen as text, each LF CR a newline.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.screen).replace("\n\r", "\n")
    }
}

/// `program`'s text run by `*BASIC -quit` (or `*BASIC64`), `keys` typed.
pub fn run_program(basic: &str, program: &str, keys: &[u8]) -> Run {
    run_within(basic, program, keys, 1 << 32)
}

/// As [`run_program`], the run spending at most `budget` instructions.
pub fn run_within(basic: &str, program: &str, keys: &[u8], budget: u64) -> Run {
    let mut kernel = kernel(0x10_0000, keys);
    let mut os = kernel.os();
    let length = u32::try_from(program.len()).expect("short");
    let at = os.rma_claim(length.max(4)).expect("room");
    os.write_bytes(at, program.as_bytes()).expect("written");
    let line = format!("{basic} -quit @{at:08X},{:08X}", at + length);
    let entered = command(&mut os, &line);
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "BASIC entered as the application, not {entered:?}"
    );
    let outcome = kernel.run(budget);
    let screen = kernel
        .os()
        .extension::<Screen>()
        .map(|s| std::mem::take(&mut s.0))
        .unwrap_or_default();
    Run { outcome, screen }
}

/// BASIC V's output for `program`.
pub fn basic(program: &str) -> String {
    let run = run_program("BASIC", program, &[]);
    run.text()
}

/// `line` run as the reference harness's `!Boot` runs it, in a session whose
/// HostFS disc is `tree`'s, to its end; its redirection then ended and its
/// files closed, as the command line that ran it ends them.
pub fn run_hosted(tree: &Tree, line: &str) -> Outcome {
    let mut kernel = hosted(0x10_0000, tree);
    let entered = command(&mut kernel.os(), line);
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "{line}: an application entered, not {entered:?}"
    );
    let outcome = kernel.run(1 << 32);
    let mut os = kernel.os();
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
    outcome
}

/// The bytes of the file `tree`'s disc holds as `name`, whatever type its
/// host name's suffix gives it.
pub fn disc_file(tree: &Tree, name: &str) -> Option<Vec<u8>> {
    let host = tree
        .names(DISC)
        .into_iter()
        .find(|host| host.split(',').next() == Some(name))?;
    tree.bytes(&format!("{DISC}/{host}"))
}

/// A recording's quoted text: printable ASCII but for `\xHH`.
pub fn unquote(token: &str) -> Vec<u8> {
    tairix_riscos_basic::corpus::unquoted(token).expect("one quoted text")
}

/// `OS_ChangeRedirection`.
const CHANGE_REDIRECTION: u32 = 0x5E;

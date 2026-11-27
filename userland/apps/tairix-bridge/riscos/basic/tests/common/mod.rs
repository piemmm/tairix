//! What the BASIC crate's integration tests share: a session with the
//! filing and BASIC modules, whose VDU stream is captured, whose keyboard
//! types what a test scripts and whose HostFS disc a test's tree may be;
//! programs run in it; and the reading of a recording's quoted text.

#![allow(
    dead_code,
    reason = "each test crate including this uses the helpers its own tests need"
)]

use tairix_fscopy::fixture::Tree;
pub use tairix_riscos_filing::session::command;
use tairix_riscos_filing::session::{end_redirection, Setup, SLOT_BYTES};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Kernel, Os, Outcome, Unwind};
use tairix_riscos_kernel::output::VduDrivers;

/// The bytes the VDU drivers were given.
pub struct Screen(pub Vec<u8>);

/// The session's modules: the filing system's and BASIC's.
static MODULES: std::sync::LazyLock<Vec<&'static Builtin>> = std::sync::LazyLock::new(|| {
    tairix_riscos_filing::session::modules_with(&[&tairix_riscos_basic::MODULES])
});

#[expect(clippy::unnecessary_wraps, reason = "the VDU drivers' signature")]
fn capture(os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
    if let Some(screen) = os.extension::<Screen>() {
        screen.0.extend_from_slice(bytes);
    }
    Ok(())
}

/// The VDU drivers a test's screen is: every byte kept, as a recording made
/// with output redirected to a file keeps it, and nothing else answered.
const VDU: VduDrivers = VduDrivers {
    start: |_| Ok(()),
    screen: |_| Ok(()),
    write: capture,
    queued: |_| false,
    swi: None,
    byte: None,
    word: None,
    frames: |_| Ok(()),
    deadline: |_| None,
    edit: |_, _| Ok(None),
    monitor: |os| os.configured_display().map_or(0, |display| display[1]),
    set_monitor: |_, _| {},
    origin: |_| (0, 0),
    routines: &[],
    vectors: &[],
};

/// A session, an application slot of `bytes`, with `keys` to type.
pub fn kernel(bytes: u32, keys: &[u8]) -> Kernel {
    session(bytes, keys, None)
}

/// As [`kernel`], with no keys, its HostFS disc `$` served over `tree`'s
/// `DISC` folder, which it may write.
pub fn hosted(bytes: u32, tree: &Tree) -> Kernel {
    session(bytes, &[], Some(tree))
}

fn session(bytes: u32, keys: &[u8], disc: Option<&Tree>) -> Kernel {
    let mut kernel = tairix_riscos_filing::session::session(&Setup {
        vdu: Some(VDU),
        modules: MODULES.as_slice(),
        bytes,
        keys,
        disc,
    })
    .kernel;
    kernel
        .os()
        .hold_extension(Screen(Vec::new()))
        .expect("room");
    kernel
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
    let mut kernel = kernel(SLOT_BYTES, keys);
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
    let mut kernel = hosted(SLOT_BYTES, tree);
    let entered = command(&mut kernel.os(), line);
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "{line}: an application entered, not {entered:?}"
    );
    let outcome = kernel.run(1 << 32);
    end_redirection(&mut kernel.os());
    outcome
}

/// A recording's quoted text: printable ASCII but for `\xHH`.
pub fn unquote(token: &str) -> Vec<u8> {
    tairix_riscos_basic::corpus::unquoted(token).expect("one quoted text")
}

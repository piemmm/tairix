//! A BBC BASIC program run on a session of this crate's modules and BASIC's,
//! HostFS over an in-memory disc, as the probes these tests hold the devices
//! to ran on RISC OS 5: the lines it wrote to `Out`.

use std::fmt::Write as _;
use std::sync::LazyLock;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, disc_file, modules_with, session, Setup, DISC};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Unwind};

/// The modules: the filing system's, then BASIC's.
static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_basic::MODULES]));

/// The instructions a program may spend.
const BUDGET: u64 = 1 << 30;
/// Its application slot's bytes.
const SLOT: u32 = 2 << 20;

/// `body`, after the lines that open `Out` and before those that close it
/// and the procedures `procedures`, run by `*BASIC -quit` with `keys`
/// typed: the lines it wrote to `Out`. A program that stopped with an error
/// fails the test with it.
pub fn run(body: &[&str], procedures: &[&str], keys: &[u8]) -> Vec<String> {
    let mut lines = vec![
        "ON ERROR E%=OPENOUT\"HostFS:$.Err\":BPUT#E%,REPORT$+\" at \"+STR$ERL:CLOSE#E%:QUIT",
        "F%=OPENOUT\"HostFS:$.Out\"",
    ];
    lines.extend_from_slice(body);
    lines.extend_from_slice(&["CLOSE#F%", "QUIT", "DEF PROCw(S$):BPUT#F%,S$:ENDPROC"]);
    lines.push("DEF FNerr(r%,f%):IF f% AND 1 THEN =\"err &\"+STR$~(!r%) ELSE =\"ok\"");
    lines.extend_from_slice(procedures);
    let program = lines
        .iter()
        .enumerate()
        .fold(String::new(), |mut program, (n, line)| {
            let _ = writeln!(program, "{} {line}", 10 * (n + 1));
            program
        });
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/Prog,fff"), program.as_bytes());
    let mut session = session(&Setup {
        vdu: None,
        modules: MODULES.as_slice(),
        bytes: SLOT,
        keys,
        disc: Some(&tree),
    });
    session.wake_for_timed_work(true);
    let entered = command(&mut session.kernel.os(), "BASIC -quit HostFS:$.Prog");
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "BASIC entered, not {entered:?}"
    );
    session.run(BUDGET);
    if let Some(error) = disc_file(&tree, "Err") {
        panic!("the program stopped: {}", String::from_utf8_lossy(&error));
    }
    let out = disc_file(&tree, "Out").unwrap_or_default();
    String::from_utf8_lossy(&out)
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

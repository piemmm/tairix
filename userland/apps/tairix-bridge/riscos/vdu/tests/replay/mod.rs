//! The screen corpus replay the VDU drivers' and the Draw module's tests
//! share: every case framed and run in order in one session, as the
//! reference harness ran them, and what each read and drew held to what
//! genuine RISC OS recorded.

use std::sync::LazyLock;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{
    command, disc_file, modules_with, session, Setup, DISC, SLOT_BYTES,
};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Unwind};
use tairix_riscos_vdu::corpus::{self, Answer};

/// The session's modules: the filing system's, BASIC's, the VDU drivers'
/// and Draw's, as genuine RISC OS's machine has them all.
pub static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    modules_with(&[
        &tairix_riscos_basic::MODULES,
        &tairix_riscos_vdu::MODULES,
        &tairix_riscos_draw::MODULES,
    ])
});

/// How each case of `cases` ran in a session with [`MODULES`] differs from
/// `answers`, genuine RISC OS's recording of them.
pub fn differences(cases: &str, answers: &str) -> Vec<String> {
    let cases = corpus::cases(cases).expect("the cases");
    let answers: Vec<Answer> = corpus::parse_answers(answers).expect("the answers");
    assert_eq!(cases.len(), answers.len(), "a recording of every case");
    let mut tree = Tree::new().dir(DISC);
    for (n, case) in cases.iter().enumerate() {
        tree = tree.file(
            &format!("{DISC}/{},fff", corpus::program_name(n)),
            corpus::program(case, n).as_bytes(),
        );
    }
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: Some(&tree),
    });
    let mut wrong = Vec::new();
    for (n, (case, answer)) in cases.iter().zip(&answers).enumerate() {
        assert_eq!(case.name, answer.name, "the answers in the cases' order");
        let entered = command(
            &mut session.kernel.os(),
            &corpus::run_command(&corpus::program_name(n)),
        );
        assert!(
            matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
            "{}: BASIC entered, not {entered:?}",
            case.name
        );
        let outcome = session.run(1 << 34);
        let record = disc_file(&tree, &corpus::record_name(n));
        let dump = disc_file(&tree, &corpus::dump_name(n));
        let Some(record) = record else {
            wrong.push(format!("{}: no record, the run {outcome:?}", case.name));
            continue;
        };
        match corpus::recorded(&record, dump.as_deref()) {
            Ok(ours) => {
                if let Some(how) = corpus::difference(&ours, &answer.recorded) {
                    wrong.push(format!("{}: {how}", case.name));
                }
            }
            Err(e) => wrong.push(format!("{}: {e}", case.name)),
        }
    }
    wrong
}

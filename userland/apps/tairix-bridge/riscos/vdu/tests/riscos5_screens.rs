//! The replay: every case genuine RISC OS 5's BASIC V ran on the screen for
//! `cargo xtask riscos --reference` (`tests/riscos5/screens.txt`), framed
//! and run in order in one session as that harness ran them, and what each
//! read and drew held to what genuine RISC OS recorded
//! (`tests/riscos5/drawn.txt`), every reading and every byte of the screen.

use std::sync::LazyLock;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{
    command, disc_file, modules_with, session, Setup, DISC, SLOT_BYTES,
};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Unwind};
use tairix_riscos_vdu::corpus::{self, Answer, Recorded};
use tairix_vdu::mode::var::{LOG2_BPP, X_WIND_LIMIT, Y_WIND_LIMIT};

const CASES: &str = include_str!("riscos5/screens.txt");
const DRAWN: &str = include_str!("riscos5/drawn.txt");

/// The session's modules: the filing system's, BASIC's and the drivers'.
static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_basic::MODULES, &tairix_riscos_vdu::MODULES]));

/// How a case's recording differs from genuine RISC OS's, if it does.
fn differs(ours: &Recorded, genuine: &Recorded) -> Option<String> {
    if ours.lines != genuine.lines {
        let first = ours
            .lines
            .iter()
            .zip(&genuine.lines)
            .position(|(a, b)| a != b)
            .unwrap_or(ours.lines.len().min(genuine.lines.len()));
        return Some(format!(
            "reading {first}: {:?} where genuine RISC OS read {:?}",
            ours.lines.get(first),
            genuine.lines.get(first)
        ));
    }
    match (&ours.screen, &genuine.screen) {
        (Some(a), Some(b)) if a == b => None,
        (Some(a), Some(b)) => {
            if (a.width, a.height, a.log2_bpp, a.line_length)
                != (b.width, b.height, b.log2_bpp, b.line_length)
            {
                return Some(format!(
                    "a screen of {a:?} where genuine RISC OS had {:?}",
                    (b.width, b.height, b.log2_bpp, b.line_length)
                ));
            }
            let stride = b.line_length as usize;
            let rows: Vec<usize> = (0..b.height as usize)
                .filter(|&r| {
                    a.bytes[r * stride..(r + 1) * stride] != b.bytes[r * stride..(r + 1) * stride]
                })
                .collect();
            let first = rows[0];
            let unit = corpus::unit(b.log2_bpp);
            Some(format!(
                "{} lines differ, the first {first}:\n  ours    {}\n  genuine {}",
                rows.len(),
                corpus::encode_row(&a.bytes[first * stride..(first + 1) * stride], unit),
                corpus::encode_row(&b.bytes[first * stride..(first + 1) * stride], unit)
            ))
        }
        (a, b) => Some(format!(
            "a screen {} where genuine RISC OS {}",
            if a.is_some() { "saved" } else { "not saved" },
            if b.is_some() {
                "saved one"
            } else {
                "saved none"
            }
        )),
    }
}

#[test]
fn every_screen_is_read_and_drawn_as_genuine_risc_os_did() {
    let cases = corpus::cases(CASES).expect("the cases");
    let answers: Vec<Answer> = corpus::parse_answers(DRAWN).expect("the answers");
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
                if let Some(how) = differs(&ours, &answer.recorded) {
                    wrong.push(format!("{}: {how}", case.name));
                }
            }
            Err(e) => wrong.push(format!("{}: {e}", case.name)),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} cases differ from genuine RISC OS:\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n")
    );
}

#[test]
fn mode_given_a_string_selects_the_mode_it_describes_and_goes_on() {
    let program = "10 MODE \"X640 Y480 C16\"\n20 F%=OPENOUT \"HostFS:$.Done\"\n30 CLOSE#F%\n";
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/ModeStr,fff"), program.as_bytes());
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: Some(&tree),
    });
    let entered = command(&mut session.kernel.os(), &corpus::run_command("ModeStr"));
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "{entered:?}"
    );
    session.run(1 << 34);
    assert!(
        disc_file(&tree, "Done").is_some(),
        "the program went on past MODE"
    );
    let variable = |session: &mut tairix_riscos_filing::session::Session, number| {
        let mut regs = tairix_riscos_kernel::os::Regs::default();
        regs.r[..2].copy_from_slice(&[u32::MAX, number]);
        session
            .kernel
            .os()
            .perform(
                tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::READ_MODE_VARIABLE,
                &mut regs,
            )
            .expect("read");
        regs.r[2]
    };
    assert_eq!(
        [X_WIND_LIMIT, Y_WIND_LIMIT, LOG2_BPP].map(|number| variable(&mut session, number)),
        [639, 479, 2],
        "640 by 480 at four bits a pixel, which the session did not start in"
    );
}

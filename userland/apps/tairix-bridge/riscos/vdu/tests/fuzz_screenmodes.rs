//! Deterministic fuzz harness for ScreenModes: hostile monitor definition
//! files — a good file's lines dropped, repeated, reordered or garbled,
//! given keys no file has and numbers past any range, modes past the most
//! kept, or bytes at random — loaded with `*LoadModeFile`; then ScreenModes'
//! SWIs at every offset with hostile registers, the screen modes enumerated
//! and selected, and the mode extension service with hostile selectors.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

mod common;

use std::sync::LazyLock;

use common::{answers, SELECTOR_AT};
use tairix_fscopy::fixture::Tree;
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::{
    command, modules_with, session, Session, Setup, DISC, SLOT_BYTES,
};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::service::MODE_EXTENSION;
use tairix_riscos_kernel::swi::os::{SCREEN_MODE, SERVICE_CALL};
use tairix_riscos_kernel::swi::X;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 2;
/// Files on a run's disc, and the calls made in one run.
const FILES: usize = 8;
const CALLS: usize = 80;
/// ScreenModes' SWI chunk, every offset of which is called.
const CHUNK: u32 = 0x4_87C0;
/// A block in the slot an enumeration fills.
const BLOCK: u32 = 0x1_0000;
const BLOCK_BYTES: u32 = 0x1000;

/// A monitor definition file the drivers read, line by line.
const GOOD: [&str; 13] = [
    "file_format:1",
    "monitor_title:Fuzz",
    "DPMS_state:1",
    "lcd_support:0",
    "startmode",
    "mode_name:VGA",
    "x_res:640",
    "y_res:480",
    "pixel_rate:25175",
    "h_timings:96,40,8,640,8,8",
    "v_timings:2,25,8,480,8,2",
    "sync_pol:3",
    "endmode",
];

/// Values put in place of a line's own.
const HOSTILE_VALUES: [&str; 10] = [
    "",
    "0",
    "-1",
    "x",
    "12x",
    "4294967296",
    "99999999999999999999",
    "1,2,3",
    "96,40,8,640,8,8,9",
    "&280",
];

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES]));

/// A hostile monitor definition file.
fn hostile_file(rng: &mut Prng) -> Vec<u8> {
    if rng.below(5) == 0 {
        let mut bytes = vec![0u8; rng.below(600)];
        rng.fill(&mut bytes);
        for byte in &mut bytes {
            if *byte % 7 == 0 {
                *byte = b'\n';
            }
        }
        return bytes;
    }
    let mut lines: Vec<String> = GOOD.iter().map(|line| (*line).to_owned()).collect();
    if rng.below(4) == 0 {
        let mode: Vec<String> = lines[4..].to_vec();
        for _ in 0..rng.below(300) {
            lines.extend(mode.iter().cloned());
        }
    }
    for _ in 0..rng.below(6) {
        let at = rng.below(lines.len().max(1));
        match rng.below(6) {
            0 if !lines.is_empty() => {
                lines.remove(at);
            }
            1 if !lines.is_empty() => {
                let line = lines[at].clone();
                lines.insert(at, line);
            }
            2 if lines.len() > 1 => {
                let other = rng.below(lines.len());
                lines.swap(at, other);
            }
            3 if !lines.is_empty() => {
                let key = lines[at].split(':').next().unwrap_or("").to_owned();
                lines[at] = format!("{key}:{}", rng.pick(&HOSTILE_VALUES));
            }
            4 => lines.insert(
                at.min(lines.len()),
                format!("key{}:{}", rng.below(9), rng.below(9)),
            ),
            _ if !lines.is_empty() => {
                let mut garbled = lines[at].clone().into_bytes();
                if !garbled.is_empty() {
                    let place = rng.below(garbled.len());
                    garbled[place] = u8::try_from(rng.below(256)).unwrap_or(0);
                }
                lines[at] = String::from_utf8_lossy(&garbled).into_owned();
            }
            _ => {}
        }
    }
    let ending = if rng.below(2) == 0 { "\n" } else { "\r\n" };
    lines.join(ending).into_bytes()
}

/// A session whose disc holds a run's hostile files.
fn boot(rng: &mut Prng) -> Session {
    let mut tree = Tree::new().dir(DISC);
    for n in 0..FILES {
        tree = tree.file(&format!("{DISC}/M{n},fff"), &hostile_file(rng));
    }
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: b"",
        disc: Some(&tree),
    })
}

/// A hostile register value: small numbers, the ends of the range,
/// pointers into the slot or nowhere.
fn hostile(rng: &mut Prng) -> u32 {
    let small = u32::try_from(rng.below(8)).unwrap_or(0);
    let into = BLOCK + u32::try_from(rng.below(BLOCK_BYTES as usize)).unwrap_or(0);
    let anything = rng.next_u32();
    *rng.pick(&[0, 1, 2, 3, small, u32::MAX, 0x8000_0000, into, anything])
}

/// A selector at [`SELECTOR_AT`]: the file's size, or any.
fn selector(session: &mut Session, rng: &mut Prng) {
    let (width, height) = if rng.below(2) == 0 {
        (640, 480)
    } else {
        (rng.next_u32() % 4096, rng.next_u32() % 4096)
    };
    let words = [
        1,
        width,
        height,
        u32::try_from(rng.below(7)).unwrap_or(0),
        *rng.pick(&[u32::MAX, 60, 0]),
        u32::MAX,
    ];
    let os = &mut session.kernel.os();
    for (i, word) in (0u32..).zip(words) {
        let _ = os.write_word(SELECTOR_AT + 4 * i, word);
    }
}

/// One hostile call, its outcome a value.
fn call(session: &mut Session, rng: &mut Prng) {
    let mut regs = Regs::new([0; 13]);
    for register in &mut regs.r[..8] {
        *register = hostile(rng);
    }
    let result = match rng.below(5) {
        0 => {
            let line = format!("LoadModeFile HostFS:$.M{}", rng.below(FILES));
            command(&mut session.kernel.os(), &line)
        }
        1 => {
            let offset = u32::try_from(rng.below(64)).unwrap_or(0);
            session.kernel.os().perform(X | (CHUNK + offset), &mut regs)
        }
        2 => {
            regs.r[..8].copy_from_slice(&[2, 0, 0, 0, 0, 0, BLOCK, BLOCK_BYTES]);
            if rng.below(3) == 0 {
                regs.r[6] = 0;
            }
            session.kernel.os().perform(X | SCREEN_MODE, &mut regs)
        }
        3 => {
            selector(session, rng);
            regs.r[..4].copy_from_slice(&[0, MODE_EXTENSION, SELECTOR_AT, 7]);
            session.kernel.os().perform(X | SERVICE_CALL, &mut regs)
        }
        _ => {
            selector(session, rng);
            regs.r[..2].copy_from_slice(&[0, SELECTOR_AT]);
            session.kernel.os().perform(X | SCREEN_MODE, &mut regs)
        }
    };
    if let Err(Fail::Error(block)) = result {
        let _ = session.kernel.os().read_word(block);
    }
}

#[test]
fn hostile_monitor_files_and_screenmodes_calls_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_monitor_files_and_screenmodes_calls_leave_the_drivers_answering",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut session = boot(&mut rng);
        for step in 0..CALLS {
            call(&mut session, &mut rng);
            assert!(
                answers(&mut session),
                "OS_ReadModeVariable after step {step}"
            );
        }
        runs += 1;
    }
}

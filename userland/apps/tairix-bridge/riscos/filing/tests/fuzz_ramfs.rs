//! Deterministic fuzz harness for FileCore over the RAM disc: a disc a
//! privileged guest may scribble on, and filing calls on hostile names, sizes
//! and handles against it; and on a disc left whole, files run by `*Run` and
//! `*Obey` with hostile lines and parameters, an Obey file obeying itself.
//!
//! The invariants:
//!
//! * nothing panics or unwinds, and every refusal is a value;
//! * the free space FileCore reports never passes the disc's size;
//! * the session still answers after every call.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

mod common;

use common::{block, call, save, text};
use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::os::Os;
use tairix_riscos_kernel::swi::os::{
    fs_control, ARGS, CHANGE_DYNAMIC_AREA, DYNAMIC_AREA, FILE, FIND, FS_CONTROL, GBPB,
    READ_DYNAMIC_AREA, READ_MONOTONIC_TIME,
};

/// Runs a plain `cargo test` makes of the sweep.
const SMOKE_RUNS: u64 = 24;
/// Calls in one run.
const STEPS: usize = 60;
/// The RAM disc's size in a run: one zone, so its map is at the area's start.
const DISC: u32 = 0x4_0000;

/// The parts hostile names are built from: names there and not, signs
/// FileSwitch or FileCore keep, wildcards, the parent and the root.
const PARTS: [&[u8]; 14] = [
    b"A", b"B", b"Dir", b"Inner", b"aB", b"x@", b"@x", b"^", b"$", b"*", b"#x", b"&", b"a b", b"",
];

/// A name on the RAM disc of up to three parts.
fn name(rng: &mut Prng) -> Vec<u8> {
    let mut name = b"RAM:$".to_vec();
    for _ in 0..=rng.below(3) {
        name.push(b'.');
        name.extend_from_slice(PARTS[rng.below(PARTS.len())]);
    }
    name
}

/// Bytes of the disc's area overwritten as a privileged guest may: in its
/// map, its root, or any page the area shows, never past the page.
fn scribble(os: &mut Os<'_>, rng: &mut Prng) {
    let base = call(os, READ_DYNAMIC_AREA, &[5]).expect("area 5").r[0];
    let at = match rng.below(3) {
        0 => rng.below(0x400),
        1 => 0x800 + rng.below(0x800),
        _ => rng.below(DISC as usize),
    };
    let at = u32::try_from(at).expect("within the disc");
    let query = block(os, &[0xFF; 16]);
    os.write_word(query + 8, at / PAGE).expect("written");
    call(os, DYNAMIC_AREA, &[25, 5, query, 1]).expect("queried");
    if os.read_word(query).expect("read") == u32::MAX {
        return;
    }
    let room = (PAGE - at % PAGE) as usize;
    let junk: Vec<u8> = (0..=rng.below(32).min(room - 1))
        .map(|_| rng.edge_biased_u8())
        .collect();
    os.write_bytes(base + at, &junk)
        .expect("a page the area shows");
}

/// A page of the area.
const PAGE: u32 = 0x1000;

/// One filing call of a random kind, refused or not, its bytes moved through
/// `data`, which holds a whole disc.
fn act(os: &mut Os<'_>, rng: &mut Prng, data: u32, handles: &mut Vec<u32>) {
    let target = name(rng);
    let at = text(os, &target);
    let size: u32 = [0, 1, 0x3FF, 0x400, 0x801, 0x1388, DISC][rng.below(7)];
    let room = size.min(0x2000);
    let handle = handles
        .get(rng.below(handles.len().max(1)))
        .copied()
        .unwrap_or(0);
    let outcome = match rng.below(14) {
        0 => call(os, FILE, &[10, at, 0xFFD, 0, data, data + size]),
        1 => call(os, FILE, &[5, at]),
        2 => call(os, FILE, &[6, at]),
        3 => call(os, FILE, &[8, at]),
        4 => call(os, FILE, &[255, at, data, 0]),
        5 => {
            let opened = call(os, FIND, &[[0x40, 0x80, 0xC0][rng.below(3)], at]);
            if let Ok(regs) = &opened {
                handles.push(regs.r[0]);
            }
            opened
        }
        6 => call(os, ARGS, &[3, handle, size]),
        7 => call(
            os,
            GBPB,
            &[[1, 2, 3, 4][rng.below(4)], handle, data, room, 0],
        ),
        8 => {
            handles.retain(|&open| open != handle);
            call(os, FIND, &[0, handle])
        }
        9 => {
            let to = text(os, &name(rng));
            call(os, FS_CONTROL, &[fs_control::RENAME, at, to])
        }
        10 => call(os, FS_CONTROL, &[fs_control::WIPE, at, 0, 3]),
        11 => call(os, GBPB, &[11, at, data, 8, 0, 0x2000, 0]),
        12 => {
            let named = text(os, b"Scribbled");
            let ram = text(os, b"RAM:");
            call(os, FS_CONTROL, &[fs_control::NAME_DISC, ram, named])
        }
        _ => {
            scribble(os, rng);
            Ok(tairix_riscos_kernel::os::Regs::default())
        }
    };
    let _ = outcome;
}

/// The lines a hostile Obey file is made from: parameters of every form, a
/// condition, the file run again from itself, a run of another, a line too
/// long for a command, and nonsense.
fn obey_line(rng: &mut Prng, own: &[u8], other: &[u8]) -> Vec<u8> {
    let mut line = match rng.below(11) {
        0 => b"Set Probe$A %0".to_vec(),
        1 => b"Set Probe$B %*1 %9 %*9".to_vec(),
        2 => b"SetEval Probe$N 1+%2".to_vec(),
        3 => b"If %0=1 Then Set Probe$I 1 Else Set Probe$I 2".to_vec(),
        4 => [&b"Obey "[..], own, b" %*1"].concat(),
        5 => [&b"Run "[..], other, b" %0"].concat(),
        6 => b"| a comment %0".to_vec(),
        7 => Vec::new(),
        8 => vec![b'x'; 1100],
        9 => b"Unset Probe$*".to_vec(),
        _ => (0..rng.below(24))
            .map(|_| b"%*09x |<>"[rng.below(9)])
            .collect(),
    };
    line.push([b'\n', b'\r', 0][rng.below(3)]);
    line
}

/// The types a run may meet that do not leave the call as an application:
/// an Obey file, text, data, and types whose action names a command the
/// session lacks.
const RUN_TYPES: [u32; 6] = [0xFEB, 0xFEB, 0xFFF, 0xFFD, 0xFFB, 0xFD7];

#[test]
fn any_file_run_or_obeyed_is_answered() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_file_run_or_obeyed_is_answered",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let names: [&[u8]; 3] = [b"RAM:$.One", b"RAM:$.Two", b"RAM:$.!App.!Run"];
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut kernel = common::kernel();
        let mut os = kernel.os();
        call(&mut os, CHANGE_DYNAMIC_AREA, &[5, DISC]).expect("a RAM disc");
        let app = text(&mut os, b"RAM:$.!App");
        call(&mut os, FILE, &[8, app]).expect("an application directory");
        for _ in 0..STEPS / 4 {
            let own = names[rng.below(names.len())];
            let other = names[rng.below(names.len())];
            let mut file = Vec::new();
            for _ in 0..rng.below(6) {
                file.extend(obey_line(&mut rng, own, other));
            }
            let kind = RUN_TYPES[rng.below(RUN_TYPES.len())];
            let _ = save(&mut os, own, kind, &file);
            let mut command = match rng.below(4) {
                0 => b"Obey ".to_vec(),
                1 => b"Obey -c ".to_vec(),
                2 => b"Obey -v ".to_vec(),
                _ => b"Run ".to_vec(),
            };
            command.extend_from_slice([own, b"RAM:$.!App", b"RAM:$.Missing"][rng.below(3)]);
            for _ in 0..rng.below(4) {
                command.extend_from_slice(
                    [&b" 1"[..], b" two", b" %0", b" 3 4 5 6 7 8 9 10"][rng.below(4)],
                );
            }
            let line = text(&mut os, &command);
            let _ = call(&mut os, tairix_riscos_kernel::swi::os::CLI, &[line]);
            assert!(
                call(&mut os, READ_MONOTONIC_TIME, &[]).is_ok(),
                "the session answers"
            );
        }
        runs += 1;
    }
}

#[test]
fn any_call_on_a_scribbled_ram_disc_is_answered() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_call_on_a_scribbled_ram_disc_is_answered",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut kernel = common::kernel();
        let mut os = kernel.os();
        call(&mut os, CHANGE_DYNAMIC_AREA, &[5, DISC]).expect("a RAM disc");
        let data = block(&mut os, &vec![0xA5; DISC as usize]);
        let mut handles = Vec::new();
        for _ in 0..STEPS {
            act(&mut os, &mut rng, data, &mut handles);
            let ram = text(&mut os, b"RAM:");
            if let Ok(free) = call(&mut os, FS_CONTROL, &[fs_control::FREE_SPACE, ram]) {
                assert!(
                    free.r[0] <= DISC,
                    "free {:#X} on a disc of {DISC:#X}",
                    free.r[0]
                );
            }
            assert!(
                call(&mut os, READ_MONOTONIC_TIME, &[]).is_ok(),
                "the session answers"
            );
        }
        runs += 1;
    }
}

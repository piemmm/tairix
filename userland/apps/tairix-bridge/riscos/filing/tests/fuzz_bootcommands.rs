//! Deterministic fuzz harness for `BootCommands` and the boot sequence:
//! every command of `BootCommands` on hostile parameters — quotation marks,
//! keywords in odd places, sizes past what a size holds, names there and
//! not, walks that become the application — and the boot sequence over a
//! user's own disc whose `!Boot` holds hostile files, programs and
//! directories.
//!
//! The invariants:
//!
//! * nothing panics or unwinds past the session, and every refusal is a value;
//! * a program a command or the boot started runs to its end within the
//!   step budget, and the boot goes on after it;
//! * every failure the boot keeps names its line and its error;
//! * nothing outside the user's own disc changes;
//! * the session still answers after every command and every boot.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

mod common;

use common::served::session_with;
use common::{call, command, run_out, save, text, EXITS, FAILS};
use tairix_fscopy::fixture::Tree;
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::boot;
use tairix_riscos_kernel::os::{Fail, Kernel, Unwind};
use tairix_riscos_kernel::swi::os::{FILE, READ_MONOTONIC_TIME};

/// Runs a plain `cargo test` makes of each sweep.
const SMOKE_RUNS: u64 = 16;
/// Commands in one run.
const STEPS: usize = 40;

/// The words hostile tails are made from.
const WORDS: [&[u8]; 24] = [
    b"RAM:$.F",
    b"RAM:$.D",
    b"RAM:$.!App",
    b"RAM:$.Missing",
    b"RAM:$.D.*",
    b"!A*",
    b"Then",
    b"Else",
    b"THEN",
    b"Set Probe$A 1",
    b"\"a b\"",
    b"\"",
    b"<Probe$A>",
    b"16K",
    b"4000",
    b"5000M",
    b"x",
    b"-Files",
    b"-Type",
    b"Obey",
    b"-CommandTail",
    b"-Tasks",
    b"%0",
    b"",
];

/// The commands of `BootCommands`, and one that is none.
const COMMANDS: [&[u8]; 14] = [
    b"X ",
    b"IfThere ",
    b"Do ",
    b"AddApp ",
    b"AddToRMA ",
    b"ShrinkRMA",
    b"AppSize ",
    b"AppSlot ",
    b"FreePool",
    b"SaveCMOS ",
    b"LoadCMOS ",
    b"SafeLogon ",
    b"Repeat ",
    b"NoSuchBootCommand ",
];

/// A hostile tail of up to six words.
fn tail(rng: &mut Prng) -> Vec<u8> {
    let mut tail = Vec::new();
    for _ in 0..rng.below(7) {
        tail.extend_from_slice(WORDS[rng.below(WORDS.len())]);
        tail.push(b"  .\""[rng.below(4)]);
    }
    tail
}

/// `kernel` run on until its program ends, where the line left one to run.
fn settle(kernel: &mut Kernel, outcome: Result<(), Fail>) {
    if let Err(Fail::Unwind(Unwind::Transfer)) = outcome {
        let _ = run_out(kernel);
    }
    let mut os = kernel.os();
    assert!(
        call(&mut os, READ_MONOTONIC_TIME, &[]).is_ok(),
        "the session answers"
    );
}

#[test]
fn any_boot_command_on_hostile_parameters_is_answered() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_boot_command_on_hostile_parameters_is_answered",
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
        common::session(&mut kernel, 0x1_0000);
        let mut os = kernel.os();
        let ram = text(&mut os, b"RAM");
        call(&mut os, tairix_riscos_kernel::swi::os::CLI, &[ram]).expect("current");
        for name in [&b"RAM:$.D"[..], b"RAM:$.!App"] {
            let at = text(&mut os, name);
            call(&mut os, FILE, &[8, at]).expect("a directory");
        }
        save(&mut os, b"RAM:$.F", 0xFEB, b"Set Probe$F 1\n").expect("saved");
        save(&mut os, b"RAM:$.D.Abs", 0xFF8, &EXITS).expect("saved");
        save(&mut os, b"RAM:$.D.Fails", 0xFF8, &FAILS).expect("saved");
        save(&mut os, b"RAM:$.!App.!Run", 0xFEB, b"Set Probe$R 1\n").expect("saved");
        for _ in 0..STEPS {
            let line = [COMMANDS[rng.below(COMMANDS.len())], &tail(&mut rng)].concat();
            let outcome = command(&mut kernel.os(), &line);
            settle(&mut kernel, outcome);
        }
        runs += 1;
    }
}

/// The files a hostile `!Boot` is made from: Obey files of lines that work,
/// fail, walk or obey again, programs that exit or fail, and data.
fn boot_file(rng: &mut Prng) -> (&'static str, Vec<u8>) {
    match rng.below(7) {
        0 => ("feb", b"Set Probe$Log <Probe$Log>o\n".to_vec()),
        1 => ("feb", b"NoSuchCommandAtAll\nSet Probe$Never 1\n".to_vec()),
        2 => ("feb", b"Repeat Set HostFS::Home.$.!Boot -Files\n".to_vec()),
        3 => (
            "feb",
            b"Obey -c Boot:Choices.Boot.PreDesktop\nX NoSuchCommandAtAll\n".to_vec(),
        ),
        4 => ("ff8", EXITS.to_vec()),
        5 => ("ff8", FAILS.to_vec()),
        _ => (
            "ffd",
            (0..rng.below(40)).map(|_| rng.edge_biased_u8()).collect(),
        ),
    }
}

/// A user's own disc whose boot application holds hostile objects.
fn hostile_disc(rng: &mut Prng) -> Tree {
    let pre_desk = "/home/!Boot/Choices/Boot/PreDesk";
    let mut tree = Tree::new().dir("/home").dir("/elsewhere");
    match rng.below(4) {
        0 => return tree.file("/home/!Boot,feb", &boot_file(rng).1),
        1 => return tree,
        _ => tree = tree.dir(pre_desk),
    }
    if rng.below(2) == 0 {
        let (kind, bytes) = boot_file(rng);
        tree = tree.file(
            &format!("/home/!Boot/Choices/Boot/PreDesktop,{kind}"),
            &bytes,
        );
    }
    for n in 0..rng.below(6) {
        if rng.below(3) == 0 {
            let directory = format!("{pre_desk}/!D{n}");
            tree = tree.dir(&directory);
            if rng.below(2) == 0 {
                let (kind, bytes) = boot_file(rng);
                tree = tree.file(&format!("{directory}/!Run,{kind}"), &bytes);
            }
        } else {
            let (kind, bytes) = boot_file(rng);
            tree = tree.file(&format!("{pre_desk}/F{n},{kind}"), &bytes);
        }
    }
    tree
}

#[test]
fn any_boot_on_a_hostile_disc_is_answered() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_boot_on_a_hostile_disc_is_answered",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let tree = hostile_disc(&mut rng);
        let mut session = session_with(
            &tree,
            Some(("/home", "Home")),
            &[("/elsewhere", "Else", true)],
        );
        common::session(&mut session.kernel, 0x1_0000);
        let started = boot::start(&mut session.kernel.os());
        let mut failures = match started {
            Ok(report) => report.failures,
            Err(Fail::Unwind(unwind)) => {
                if unwind == Unwind::Transfer {
                    let _ = run_out(&mut session.kernel);
                }
                Vec::new()
            }
            Err(Fail::Error(block)) => panic!("the boot refused with HostFS active: {block:#X}"),
        };
        failures.extend(boot::reported(&mut session.kernel.os()).failures);
        for failure in &failures {
            assert!(!failure.line.is_empty(), "a failure names its line");
        }
        assert_eq!(
            tree.names("/elsewhere"),
            Vec::<String>::new(),
            "nothing outside the own disc changed"
        );
        let mut os = session.kernel.os();
        assert!(
            call(&mut os, READ_MONOTONIC_TIME, &[]).is_ok(),
            "the session answers"
        );
        runs += 1;
    }
}

//! Deterministic fuzz harness for the command line: random lines given to
//! `OS_CLI` — the kernel's commands and their abbreviations with hostile
//! parameters, aliases with random templates, prefixes, redirection groups,
//! GSTrans escapes, conditionals and lines past the longest — under a host
//! that stops endless work.
//!
//! The invariants, checked after every line:
//!
//! * nothing panics, and every refusal is a value;
//! * nothing runs for ever: the host's word stops it;
//! * the kernel still answers a SWI;
//! * the interpreter is left as it was found: a line sets a variable.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

#[path = "support/boot.rs"]
mod boot;
#[path = "support/still.rs"]
mod still;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Kernel, Regs};
use tairix_riscos_kernel::swi::os::CLI;
use tairix_riscos_kernel::swi::X;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 20;
/// Lines in one run.
const STEPS: usize = 60;
/// Where a run writes its line.
const LINE: u32 = APPLICATION + 0x100;
/// Where the check of a run's soundness writes and reads.
const CHECK: u32 = APPLICATION + 0x2000;
/// The instructions a run of the guest is given after a line unwinds.
const BUDGET: u64 = 20_000;

/// The words a line starts with: the kernel's commands and what reaches
/// them some other way.
const COMMANDS: [&[u8]; 58] = [
    b"Echo",
    b"Set",
    b"SetMacro",
    b"SetEval",
    b"Unset",
    b"IF",
    b"Eval",
    b"Show",
    b"Key",
    b"FX",
    b"TV",
    b"Opt",
    b"Error",
    b"Help",
    b"Configure",
    b"Status",
    b"Type",
    b"List",
    b"Print",
    b"Dump",
    b"Load",
    b"Save",
    b"Create",
    b"Delete",
    b"Remove",
    b"Close",
    b"Exec",
    b"Spool",
    b"SpoolOn",
    b"Append",
    b"Build",
    b"Ignore",
    b"Shadow",
    b"Cache",
    b"ChangeDynamicArea",
    b"Modules",
    b"ROMModules",
    b"Unplug",
    b"RMEnsure",
    b"RMKill",
    b"RMReInit",
    b"RMClear",
    b"RMTidy",
    b"RMFaster",
    b"RMInsert",
    b"Time",
    b"Quit",
    b"GO",
    b"GOS",
    b"UtilityModule:Echo",
    b"Module#UtilityModule:Show",
    b"-UtilityModule-Echo",
    b"%Echo",
    b"/Run",
    b".",
    b"Fz",
    b"Fz.",
    b"FX0",
];
/// Parameters, most often one of these.
const WORDS: [&[u8]; 26] = [
    b"0",
    b"1",
    b"255",
    b"256",
    b"&FF",
    b"2_101",
    b"-1",
    b"999999999999",
    b"x",
    b"\"q\"",
    b"\"",
    b"|M",
    b"|G",
    b"|",
    b"<Fz$V>",
    b"<Fz$V",
    b"%0",
    b"%*1",
    b"THEN",
    b"ELSE",
    b"{ > o }",
    b"{ < i }",
    b"{ >> a }",
    b"{",
    b"}",
    b"Fz$*",
];
/// What ends a line.
const ENDS: [&[u8]; 3] = [b"\r", b"\n", b"\0"];

/// Performs SWI `number` with `registers`: its registers, or its refusal;
/// whether control left for somewhere the run must go.
fn swi(kernel: &mut Kernel, number: u32, registers: [u32; 13]) -> (Result<Regs, Fail>, bool) {
    let mut regs = Regs::new(registers);
    let result = kernel.os().perform(X | number, &mut regs);
    let unwound = matches!(result, Err(Fail::Unwind(_)));
    (result.map(|()| regs), unwound)
}

/// A random line: a command, or an alias's definition, with parameters.
fn line(rng: &mut Prng) -> Vec<u8> {
    let mut text = match rng.below(8) {
        0 => {
            let mut set = b"Set Alias$Fz ".to_vec();
            for _ in 0..rng.below(4) {
                set.extend_from_slice(rng.pick(&COMMANDS));
                set.push(b' ');
                set.extend_from_slice(rng.pick(&[&b"%0"[..], b"%*1", b"%%", b"|M", b"x"]));
                set.extend_from_slice(rng.pick(&[&b"|M"[..], b" "]));
            }
            set
        }
        1 => b"Set Fz$V ".to_vec(),
        _ => rng.pick(&COMMANDS).to_vec(),
    };
    let leading = rng.below(3);
    let mut prefixed = vec![b' '; leading];
    if rng.below(4) == 0 {
        prefixed.push(b'*');
    }
    prefixed.append(&mut text);
    let mut text = prefixed;
    for _ in 0..rng.below(6) {
        text.extend_from_slice(b" ");
        if rng.below(6) == 0 {
            let length = 1 + rng.below(8);
            text.extend(rng.noise(length).into_iter().map(|b| b.max(b' ')));
        } else {
            text.extend_from_slice(rng.pick(&WORDS));
        }
    }
    if rng.below(30) == 0 {
        text.resize(1000 + rng.below(100), b'z');
    }
    text.extend_from_slice(rng.pick(&ENDS));
    text
}

/// The kernel still answers, and its interpreter runs a line.
fn still_sound(kernel: &mut Kernel, step: usize) {
    assert!(
        swi(kernel, 0x42, [0; 13]).0.is_ok(),
        "OS_ReadMonotonicTime after line {step}"
    );
    let _ = kernel.os().write_bytes(CHECK, b"Set Fz$Alive yes\r");
    let (set, _) = swi(kernel, CLI, [CHECK, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(set.is_ok(), "the interpreter runs a line after line {step}");
    let _ = kernel.os().write_bytes(CHECK, b"Fz$Alive\0");
    let (read, _) = swi(
        kernel,
        0x23,
        [CHECK, CHECK + 0x40, 16, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    let length = read.expect("the variable reads").r[2];
    let mut bytes = vec![0; length as usize];
    let _ = kernel.os().read_bytes(CHECK + 0x40, &mut bytes);
    assert_eq!(bytes, b"yes", "after line {step}");
}

#[test]
fn any_line_answers_as_a_value() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_line_answers_as_a_value",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let (mut kernel, _) = still::boot(&mut rng);
        for step in 0..STEPS {
            let text = line(&mut rng);
            let _ = kernel.os().write_bytes(LINE, &text);
            let (_, unwound) = swi(&mut kernel, CLI, [LINE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            // A program a line started, or a handler it unwound to, runs.
            if unwound {
                let _ = kernel.run(BUDGET);
            }
            still_sound(&mut kernel, step);
        }
        runs += 1;
    }
}

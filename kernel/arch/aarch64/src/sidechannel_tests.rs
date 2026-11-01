//! Host pins for every side-channel mechanism the port declares applied.
//!
//! The trap and return paths are assembly no host test can run, so each
//! mechanism is checked against the sources they are built from: a profile
//! that says `Applied` fails here while any path lacks the code. The
//! needles live in this file, never in an inspected one, so a needle
//! cannot match itself.

extern crate std;

use std::collections::BTreeSet;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use super::SideChannel;
use crate::exceptions::eret_tests::instruction_lines;

/// Every source of the port that carries assembly.
const ASM_SOURCES: &[(&str, &str)] = &[
    ("backtrace.rs", include_str!("backtrace.rs")),
    ("context.rs", include_str!("context.rs")),
    ("coreclock.rs", include_str!("coreclock.rs")),
    ("cpufeatures.rs", include_str!("cpufeatures.rs")),
    ("cpuname.rs", include_str!("cpuname.rs")),
    ("entropy.rs", include_str!("entropy.rs")),
    ("exceptions.rs", include_str!("exceptions.rs")),
    ("gic.rs", include_str!("gic.rs")),
    ("gicv3.rs", include_str!("gicv3.rs")),
    ("irqmask.rs", include_str!("irqmask.rs")),
    ("its.rs", include_str!("its.rs")),
    ("kernel_arch.rs", include_str!("kernel_arch.rs")),
    ("lib.rs", include_str!("lib.rs")),
    ("memtag.rs", include_str!("memtag.rs")),
    ("paging.rs", include_str!("paging.rs")),
    ("percpu_hal.rs", include_str!("percpu_hal.rs")),
    ("preempt.rs", include_str!("preempt.rs")),
    ("protection.rs", include_str!("protection.rs")),
    ("psci.rs", include_str!("psci.rs")),
    ("qemu_exit.rs", include_str!("qemu_exit.rs")),
    ("smp.rs", include_str!("smp.rs")),
    ("takeover.rs", include_str!("takeover.rs")),
    ("uaccess.rs", include_str!("uaccess.rs")),
    ("userentry.rs", include_str!("userentry.rs")),
    ("watchdog.rs", include_str!("watchdog.rs")),
    ("boot.s", include_str!("boot.s")),
    ("context.s", include_str!("context.s")),
    ("smp.s", include_str!("smp.s")),
    ("takeover.s", include_str!("takeover.s")),
    ("vectors.s", include_str!("vectors.s")),
];

/// The instructions of `source`: every non-comment line of an assembly
/// source, every string-literal line of a Rust one.
fn instructions(name: &str, source: &str) -> Vec<String> {
    instruction_lines(source)
        .into_iter()
        .filter_map(|line| {
            let text = if tairix_fuzzseed::sources::is_assembly(name) {
                line.split("//")
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            } else {
                let body = line
                    .strip_prefix('"')?
                    .trim_end_matches(',')
                    .strip_suffix('"')?;
                body.strip_suffix("\\n").unwrap_or(body).to_string()
            };
            (!text.is_empty()).then_some(text)
        })
        .collect()
}

#[test]
fn every_eret_is_followed_by_a_speculation_barrier() {
    if !SideChannel::declared_profile()
        .user_return_speculation_barrier
        .is_applied()
    {
        return;
    }
    let mut found = BTreeSet::new();
    for &(name, source) in ASM_SOURCES {
        let lines = instructions(name, source);
        for (at, _) in lines.iter().enumerate().filter(|(_, i)| *i == "eret") {
            found.insert(name);
            assert_eq!(
                lines.get(at + 1..at + 3),
                Some(&[String::from("dsb nsh"), String::from("isb")][..]),
                "{name}: straight-line speculation runs on past an `eret`"
            );
        }
    }
    for known in ["vectors.s", "userentry.rs", "boot.s"] {
        assert!(found.contains(known), "no `eret` found in {known}");
    }
}

/// Immediately before the handler call, after every save, the trampoline
/// zeroes each general-purpose register the call does not replace.
#[test]
fn the_trap_trampoline_zeroes_what_it_saved_before_the_handler_runs() {
    if !SideChannel::declared_profile()
        .user_entry_hardening
        .is_applied()
    {
        return;
    }
    let lines = instructions("vectors.s", include_str!("vectors.s"));
    let call = lines
        .iter()
        .position(|i| i == "bl tairix_aarch64_trap_handler")
        .expect("the handler call");
    let mut zeroed = BTreeSet::new();
    for instruction in lines[..call].iter().rev() {
        if instruction == "mov x1, sp" {
            continue;
        }
        let Some(register) = instruction
            .strip_prefix("mov ")
            .and_then(|operands| operands.strip_suffix(", xzr"))
        else {
            break;
        };
        zeroed.insert(register.to_string());
    }
    let expected: BTreeSet<String> = (2..=28).map(|n| format!("x{n}")).collect();
    assert_eq!(
        zeroed, expected,
        "the handler would run with interrupted values live in a register"
    );
}

#[test]
fn every_source_carrying_assembly_is_inspected() {
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    assert_eq!(
        tairix_fuzzseed::sources::unlisted_assembly(root, ASM_SOURCES).expect("the port's sources"),
        Vec::<String>::new(),
        "each carries assembly but is not inspected"
    );
}

/// The kernel never clears `PSTATE.PAN` to reach a user page, and translation
/// comes on with `SPAN` clear under PAN: the only write to the PAN register is
/// `protection::hold`'s, which sets it, and every `SCTLR_EL1` write installs
/// the MMU-off constant or [`crate::protection::sctlr_mmu_on`]'s value.
#[test]
fn only_hold_writes_pan_and_every_sctlr_write_is_a_known_value() {
    const MMU_OFF: [&str; 2] = ["mov x0, #0x0800", "movk x0, #0x30D0, lsl #16"];
    const MMU_ON: &str =
        "sctlr = in(reg) crate::protection::sctlr_mmu_on(crate::protection::pan_in_force()),";
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut pan_writes = Vec::new();
    let mut sctlr_writes = 0;
    for source in tairix_fuzzseed::sources::crate_sources(root).expect("the port's sources") {
        let lines = instructions(source.name(), source.text());
        for (at, line) in lines.iter().enumerate() {
            let lower = line.to_ascii_lowercase();
            if !lower.starts_with("msr ") {
                continue;
            }
            if lower.starts_with("msr s3_0_c4_c2_3,") || lower.starts_with("msr pan,") {
                pan_writes.push(format!("{}: {line}", source.relative()));
            }
            if lower.starts_with("msr sctlr_el1,") {
                sctlr_writes += 1;
                let known = if tairix_fuzzseed::sources::is_assembly(source.name()) {
                    at >= 2 && lines[at - 2..at] == MMU_OFF
                } else {
                    source.text().matches(MMU_ON).count() == 1
                };
                assert!(
                    known,
                    "{}: `{line}` writes an unknown SCTLR_EL1",
                    source.relative()
                );
            }
        }
    }
    assert_eq!(pan_writes, ["protection.rs: msr S3_0_C4_C2_3, {pan}"]);
    assert!(
        include_str!("protection.rs").contains("pan = in(reg) PSTATE_PAN,"),
        "the one PAN write sets it"
    );
    assert_eq!(
        sctlr_writes, 4,
        "the three MMU-off writes and the one MMU-on write"
    );
}

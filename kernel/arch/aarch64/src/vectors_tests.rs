//! Host unit tests pinning what the exception path reads off an entry against
//! its sources: the frame every entry reserves, which entries the CPU writes a
//! syndrome for, and that `FAR_EL1` is read once, before anything can rewrite
//! it.

use std::string::String;
use std::vec::Vec;

use super::eret_tests::{instruction_lines, line_of};
use super::{has_syndrome, interrupted_kernel_sp, kind, TRAP_FRAME_BYTES};

/// Each vector-table entry's type, from its offset comment (`// 0x080 IRQ`),
/// and the kind its `mov x0, #N` tags it with, in table order.
fn entries() -> Vec<(String, u64)> {
    let mut entries = Vec::new();
    let mut pending = None;
    for line in include_str!("vectors.s").lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let [".balign", "0x80", "//", _offset, entry_type, ..] = words.as_slice() {
            pending = Some(String::from(*entry_type));
        } else if let (["mov", "x0,", immediate], Some(entry_type)) =
            (words.as_slice(), pending.as_ref())
        {
            let kind = immediate
                .strip_prefix('#')
                .and_then(|n| n.parse().ok())
                .expect("a kind immediate");
            entries.push((entry_type.clone(), kind));
            pending = None;
        }
    }
    entries
}

#[test]
fn each_entry_is_tagged_with_its_index() {
    let kinds: Vec<u64> = entries().iter().map(|&(_, kind)| kind).collect();
    assert_eq!(kinds, (0..16).collect::<Vec<u64>>());
}

/// The CPU writes `ESR_EL1` for a synchronous exception and an `SError`, and
/// leaves it stale for an IRQ or FIQ, so only the former may report one.
#[test]
fn only_a_synchronous_or_serror_entry_carries_a_syndrome() {
    let entries = entries();
    assert_eq!(entries.len(), 16);
    for (entry_type, kind) in entries {
        let written = matches!(entry_type.as_str(), "Synchronous" | "SError");
        assert!(
            matches!(
                entry_type.as_str(),
                "Synchronous" | "IRQ" | "FIQ" | "SError"
            ),
            "entry {kind} has an unrecognised type `{entry_type}`"
        );
        assert_eq!(has_syndrome(kind), written, "entry {kind} ({entry_type})");
    }
}

#[test]
fn every_entry_reserves_the_frame_the_interrupted_sp_is_recovered_from() {
    let reserve = std::format!("sub sp, sp, #{TRAP_FRAME_BYTES}");
    let reservations: Vec<String> = instruction_lines(include_str!("vectors.s"))
        .into_iter()
        .filter(|line| line.starts_with("sub sp, sp,"))
        .collect();
    assert_eq!(reservations.len(), 16, "one reservation per entry");
    assert!(reservations.iter().all(|line| *line == reserve));
}

/// Any nested synchronous exception rewrites `FAR_EL1`, so the synchronous
/// path reads it once, ahead of every point that can take one, and takes the
/// PC from the saved frame rather than a live `ELR_EL1`.
#[test]
fn far_is_read_once_before_anything_can_take_a_nested_exception() {
    let src = include_str!("exceptions.rs");
    let far_reads = src.matches("read_far()").count() - src.matches("fn read_far()").count();
    assert_eq!(far_reads, 0, "`FAR_EL1` is read only through `far_of`");
    assert!(
        !src.contains(", ELR_EL1\""),
        "the PC comes from the saved frame, never a live `ELR_EL1`"
    );
    let lines = instruction_lines(src);
    let read = line_of(&lines, "let far = far_of(esr);");
    for can_nest in [
        "enable_fiq_delivery();",
        "let dispatched = crate::syscall_entry::dispatch_svc(&mut syscall_frame);",
        "&& crate::paging::set_accessed_flag_in_active(address)",
        "(tairix_arch_api::fault::user_fault_resolver(), far)",
        "fatal_exception(kind, Some(esr), far, frame);",
    ] {
        assert!(
            read < line_of(&lines, can_nest),
            "`{can_nest}` can take a nested exception before `FAR_EL1` is read",
        );
    }
}

#[test]
fn the_interrupted_stack_pointer_follows_the_entry_group() {
    let frame = 0x4010_0000;
    let saved_sp_el0 = 0x7fff_f000;
    // EL1 on `SP_EL1`: the frame sits just below the interrupted pointer.
    assert_eq!(
        interrupted_kernel_sp(kind::CUR_SPX_SYNC, frame, saved_sp_el0),
        Some(frame + TRAP_FRAME_BYTES)
    );
    // EL1 on `SP_EL0`: the trampoline saved it.
    assert_eq!(
        interrupted_kernel_sp(kind::CUR_SP0_IRQ, frame, saved_sp_el0),
        Some(saved_sp_el0)
    );
    // From EL0, AArch64 or AArch32: no kernel stack was interrupted.
    assert_eq!(
        interrupted_kernel_sp(kind::LOWER_SYNC, frame, saved_sp_el0),
        None
    );
    assert_eq!(interrupted_kernel_sp(12, frame, saved_sp_el0), None);
}

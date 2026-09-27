//! Host unit test pinning that the fault path reads `stval` once.
//!
//! `stval` is not frame-resident: every trap rewrites it, and the fault path
//! runs code that can take one — the A/D walk, a resolver that blocks on I/O,
//! the guarded read of an illegal instruction, the terminator. A read after
//! any of them could report another trap's value as this fault's address. The
//! path builds only for the freestanding target, so the order is pinned
//! against `trap.rs`; the needles live here, where none can match itself.

use super::sret_tests::only;

const TRAP_RS: &str = include_str!("trap.rs");

#[test]
fn the_fault_path_reads_stval_once_before_anything_can_trap() {
    let calls =
        TRAP_RS.matches("read_stval()").count() - TRAP_RS.matches("fn read_stval()").count();
    assert_eq!(calls, 1, "`stval` is read at exactly one site");
    let read = only(TRAP_RS, "= read_stval();");
    for can_trap in [
        "set_accessed_flag_in_active(stval, kind)",
        "resolver(stval, write_fault,",
        "illegal_instruction_needs_fp(pc)",
        "fatal_exception(scause, stval, frame)",
    ] {
        assert!(
            read < only(TRAP_RS, can_trap),
            "`{can_trap}` runs before `stval` is read, so a nested trap may \
             already have rewritten it",
        );
    }
}

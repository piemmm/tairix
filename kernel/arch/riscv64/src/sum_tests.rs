//! Host unit tests pinning that S-mode never reaches a user page by its user
//! address: each hart clears `sstatus.SUM` as it arms its trap vector, and no
//! write in the port sets it again. The instructions cannot run on the host,
//! so they are pinned against the sources; the needles live here, where none
//! can match itself.

use std::vec::Vec;

use super::sret_tests::only;
use super::trap_layout_tests::equ;
use super::{SSTATUS_SIE, SSTATUS_SUM};

const TRAP_RS: &str = include_str!("trap.rs");
const TRAP_S: &str = include_str!("trap.s");

/// Whether `line` is code that writes `sstatus`.
fn writes_sstatus(line: &str) -> bool {
    let code = line.trim_start();
    if !code.contains("sstatus") || code.starts_with("//") || code.starts_with('#') {
        return false;
    }
    let words = code.split_whitespace().collect::<Vec<_>>().join(" ");
    [
        "csrs sstatus",
        "csrsi sstatus",
        "csrw sstatus",
        "csrwi sstatus",
    ]
    .iter()
    .any(|write| words.contains(write))
        || (words.contains(", sstatus,")
            && ["csrrs ", "csrrsi ", "csrrw ", "csrrwi "]
                .iter()
                .any(|swap| words.contains(swap)))
}

#[test]
fn sum_is_bit_eighteen() {
    assert_eq!(SSTATUS_SUM, 1 << 18);
}

#[test]
fn arming_the_trap_vector_clears_sum() {
    let arm = only(TRAP_RS, "pub unsafe fn install_trap_vector() {");
    let end = arm + TRAP_RS[arm..].find("\n}\n").expect("the function ends");
    let clear = only(
        TRAP_RS,
        "\"csrc sstatus, {sum}\", sum = in(reg) SSTATUS_SUM",
    );
    assert!(
        arm < clear && clear < end,
        "arming the vector leaves `SUM` as the firmware left it"
    );
}

/// An instruction that sets `sstatus` bits, as its source spells it, and the
/// bits it sets.
type Set = (&'static str, u64);

/// Every write in the port that sets an `sstatus` bit, and the bits it sets.
/// A write this does not list fails the count, so a new one is checked
/// against `SUM` here before it lands.
#[test]
fn no_write_in_the_port_sets_sum() {
    let sources: [(&str, &str, &[Set]); 6] = [
        (
            "trap.rs",
            TRAP_RS,
            &[("\"csrs sstatus, {}\", in(reg) SSTATUS_SIE", SSTATUS_SIE)],
        ),
        (
            "trap.s",
            TRAP_S,
            &[("csrs    sstatus, t1", equ("SSTATUS_FS_ENABLE"))],
        ),
        (
            "irqmask.rs",
            include_str!("irqmask.rs"),
            &[(
                "\"csrs sstatus, {0}\", in(reg) crate::trap::SSTATUS_SIE",
                SSTATUS_SIE,
            )],
        ),
        (
            "imsic.rs",
            include_str!("imsic.rs"),
            &[("\"csrsi sstatus, 2\"", 2)],
        ),
        (
            "fpstate.rs",
            include_str!("fpstate.rs"),
            &[("\"csrs sstatus, {b}\",", crate::fpstate::FS_MASK)],
        ),
        ("userentry.rs", include_str!("userentry.rs"), &[]),
    ];
    for (file, src, sets) in sources {
        let writes = src.lines().filter(|line| writes_sstatus(line)).count();
        let listed = sets
            .iter()
            .map(|(needle, _)| src.matches(needle).count())
            .sum::<usize>();
        // `trap.s` also reinstalls the `sstatus` its trap saved, pinned below.
        let restores = usize::from(file == "trap.s");
        assert_eq!(
            writes,
            listed + restores,
            "{file} writes `sstatus` somewhere this test does not list"
        );
        for (needle, bits) in sets {
            assert_eq!(bits & SSTATUS_SUM, 0, "{file}: `{needle}` sets `SUM`");
        }
    }
}

/// The epilogue writes `sstatus` whole, but only with the value the trap
/// saved: a hart clears `SUM` before it can take a trap, and U-mode cannot
/// set it, so the saved value never carries it.
#[test]
fn the_only_whole_sstatus_write_reinstalls_the_saved_value() {
    const LOAD: &str = "ld      t0, OFF_SSTATUS(sp)";
    let loaded = only(TRAP_S, LOAD) + LOAD.len();
    let write = only(TRAP_S, "csrw    sstatus, t0");
    assert!(
        loaded < write && TRAP_S[loaded..write].trim().is_empty(),
        "the restore writes the saved value straight back"
    );
}

/// The kernel decides a task's first floating-point use without reading the
/// instruction: it lies in user memory, which no trap path reads.
#[test]
fn the_trap_path_reads_nothing_through_the_guarded_copy() {
    assert!(!TRAP_RS.contains("guarded_copy"));
    assert!(TRAP_RS.contains("crate::fpstate::adopt_on_first_use(anchor)"));
}

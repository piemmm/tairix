//! Host unit tests pinning that S-mode never reaches a user page by its user
//! address: each hart clears `sstatus.SUM` as it arms its trap vector, and no
//! write in any S-mode source — the port's and the kernel binary's — sets it
//! again. The instructions cannot run on the host, so they are pinned against
//! the sources; the needles live here, where none can match itself.

use std::format;
use std::path::Path;
use std::string::String;
use std::vec::Vec;

use super::sret_tests::only;
use super::trap_layout_tests::equ;
use super::{SSTATUS_SIE, SSTATUS_SUM};

const TRAP_RS: &str = include_str!("trap.rs");
const TRAP_S: &str = include_str!("trap.s");

/// Whether `operand` names `sstatus`, by name or by its CSR number.
fn names_sstatus(operand: &str) -> bool {
    matches!(
        operand.to_ascii_lowercase().as_str(),
        "sstatus" | "0x100" | "256"
    )
}

/// Whether `line` is code that puts bits in `sstatus`: a set, or a whole
/// write, however the CSR is named.
fn writes_sstatus(line: &str) -> bool {
    let code = line.trim_start();
    if code.starts_with("//") || code.starts_with('#') {
        return false;
    }
    let words: Vec<&str> = code
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '"' | '(' | ')'))
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().enumerate().any(|(at, word)| {
        let csr = match word.to_ascii_lowercase().as_str() {
            "csrs" | "csrsi" | "csrw" | "csrwi" => words.get(at + 1),
            "csrrs" | "csrrsi" | "csrrw" | "csrrwi" => words.get(at + 2),
            _ => None,
        };
        csr.is_some_and(|csr| names_sstatus(csr))
    })
}

/// What a listed write puts in `sstatus`.
#[derive(Clone, Copy, Debug)]
enum Puts {
    /// These bits, set.
    Bits(u64),
    /// The whole value a trap saved.
    SavedValue,
}

/// A write the port makes to `sstatus`: the source it is in, under its root
/// (`arch/` for the port, `kernel/` for the kernel binary), as the source
/// spells it, and what it puts.
type Listed = (&'static str, &'static str, Puts);

/// Every write in an S-mode source that puts anything in `sstatus`. One this
/// does not list fails the check, so a new one is judged against `SUM` here
/// before it lands.
fn listed() -> [Listed; 6] {
    [
        (
            "arch/trap.rs",
            "\"csrs sstatus, {}\", in(reg) SSTATUS_SIE",
            Puts::Bits(SSTATUS_SIE),
        ),
        (
            "arch/trap.s",
            "csrs    sstatus, t1",
            Puts::Bits(equ("SSTATUS_FS_ENABLE")),
        ),
        ("arch/trap.s", "csrw    sstatus, t0", Puts::SavedValue),
        (
            "arch/irqmask.rs",
            "\"csrs sstatus, {0}\", in(reg) crate::trap::SSTATUS_SIE",
            Puts::Bits(SSTATUS_SIE),
        ),
        ("arch/imsic.rs", "\"csrsi sstatus, 2\"", Puts::Bits(2)),
        (
            "arch/fpstate.rs",
            "\"csrs sstatus, {b}\",",
            Puts::Bits(crate::fpstate::FS_MASK),
        ),
    ]
}

/// Where `sources`, as `(path, text)`, and `listed` disagree: a source whose
/// writes to `sstatus` its listed needles do not account for, and a listed
/// write no source spells.
fn unlisted(sources: &[(String, &str)], listed: &[Listed]) -> Vec<String> {
    let mut wrong = Vec::new();
    for (path, text) in sources {
        let writes = text.lines().filter(|line| writes_sstatus(line)).count();
        let named = listed
            .iter()
            .filter(|(listed_in, ..)| listed_in == path)
            .map(|(_, needle, _)| text.matches(needle).count())
            .sum::<usize>();
        if writes != named {
            wrong.push(format!(
                "{path}: {writes} writes to `sstatus`, {named} listed"
            ));
        }
    }
    for (path, needle, _) in listed {
        let spelled = sources
            .iter()
            .any(|(source, text)| source == path && text.contains(needle));
        if !spelled {
            wrong.push(format!("{path}: no source spells `{needle}`"));
        }
    }
    wrong
}

/// Every S-mode source, as `(path, text)` under the root it was read from.
fn s_mode_sources() -> Vec<(String, String)> {
    let roots = [
        ("arch", concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        (
            "kernel",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tairix-kernel/src"),
        ),
    ];
    roots
        .iter()
        .flat_map(|(label, root)| {
            tairix_fuzzseed::sources::crate_sources(Path::new(root))
                .unwrap_or_else(|err| panic!("{root}: {err}"))
                .into_iter()
                .map(move |source| {
                    (
                        format!("{label}/{}", source.relative()),
                        String::from(source.text()),
                    )
                })
        })
        .collect()
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

/// Every S-mode source's writes to `sstatus` are the listed ones, and none of
/// those sets `SUM`.
#[test]
fn no_write_in_the_port_sets_sum() {
    let sources = s_mode_sources();
    let sources: Vec<(String, &str)> = sources
        .iter()
        .map(|(path, text)| (path.clone(), text.as_str()))
        .collect();
    assert_eq!(unlisted(&sources, &listed()), Vec::<String>::new());
    for (path, needle, puts) in listed() {
        if let Puts::Bits(bits) = puts {
            assert_eq!(bits & SSTATUS_SUM, 0, "{path}: `{needle}` sets `SUM`");
        }
    }
}

#[test]
fn a_set_write_in_a_source_no_entry_names_fails_the_check() {
    for planted in [
        "    core::arch::asm!(\"csrs sstatus, {sum}\", sum = in(reg) SSTATUS_SUM);",
        "    core::arch::asm!(\"csrs 0x100, {sum}\", sum = in(reg) SSTATUS_SUM);",
        "    csrrs   zero, 0x100, t0",
        "    CSRW    SSTATUS, t0",
    ] {
        let sources = [(String::from("arch/uaccess.rs"), planted)];
        let wrong = unlisted(&sources, &[]);
        assert_eq!(wrong.len(), 1, "{planted}");
        assert!(wrong[0].starts_with("arch/uaccess.rs"), "{wrong:?}");
    }
    assert!(
        !writes_sstatus("    csrc sstatus, t0"),
        "a clear sets nothing"
    );
    assert!(
        !writes_sstatus("    csrr t0, sstatus"),
        "a read writes nothing"
    );
    assert!(
        !writes_sstatus("    // csrs sstatus, t0"),
        "a comment is no write"
    );
}

#[test]
fn a_listed_write_no_source_spells_fails_the_check() {
    let wrong = unlisted(&[], &listed());
    assert_eq!(wrong.len(), listed().len());
    assert!(wrong.iter().all(|wrong| wrong.contains("no source spells")));
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

/// A secondary hart arms its trap vector first, so it holds `SUM` and the
/// counters closed whatever its entry callback does.
#[test]
fn a_secondary_hart_arms_its_trap_vector_first() {
    let source = include_str!("smp.rs");
    let entry = source
        .find("extern \"C\" fn tairix_arch_riscv64_secondary_main(")
        .expect("the secondary's entry");
    let first_statement = source[entry..]
        .lines()
        .skip(1)
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .expect("a body");
    assert_eq!(
        first_statement,
        "unsafe { crate::trap::install_trap_vector() };"
    );
}

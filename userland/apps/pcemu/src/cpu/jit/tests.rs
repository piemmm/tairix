use super::differential::{self, Case, Seen};

/// The kinds of instruction the translator takes, each of which the test
/// must reach.
const KINDS: [&str; 74] = [
    "Aaa",
    "Aad",
    "Aam",
    "Aas",
    "AluAccImm",
    "AluRegRm",
    "AluRmImm",
    "AluRmReg",
    "CallFar",
    "CallFarRm",
    "CallNear",
    "CallRm",
    "Cbw",
    "Clc",
    "Cld",
    "Cli",
    "Cmc",
    "Cmps",
    "Cwd",
    "Daa",
    "Das",
    "DecReg",
    "DecRm",
    "ImulImm",
    "IncReg",
    "IncRm",
    "Jcc",
    "Jcxz",
    "JmpFar",
    "JmpFarRm",
    "JmpNear",
    "JmpRm",
    "JmpShort",
    "Lahf",
    "Lds",
    "Lea",
    "Leave",
    "Les",
    "Lods",
    "Loop",
    "Loope",
    "Loopne",
    "MovAccMem",
    "MovMemAcc",
    "MovRegImm",
    "MovRegRm",
    "MovRmImm",
    "MovRmReg",
    "MovRmSeg",
    "MovSegRm",
    "Movs",
    "PopReg",
    "PopRm",
    "PopSeg",
    "Popa",
    "PushImm",
    "PushReg",
    "PushRm",
    "PushSeg",
    "Pusha",
    "Pushf",
    "Ret",
    "Retf",
    "Sahf",
    "Salc",
    "Scas",
    "Shift",
    "Stc",
    "Std",
    "Sti",
    "Stos",
    "TestAccImm",
    "TestRmReg",
    "Unary",
];

#[test]
fn blocks_leave_the_machine_as_the_interpreter_does_on_every_backend() {
    let pattern = differential::pattern();
    let mut rng = tairix_fuzzseed::Prng::new(0x5EED_0018);
    let mut seen = Seen::default();
    let mut blocks = 0;
    for _ in 0..3000 {
        let case = Case::random(&mut || rng.next_u64(), &pattern);
        if differential::agree(&case, &mut seen).is_some() {
            blocks += 1;
        }
    }
    assert!(
        seen.exits.iter().all(|&n| n > 50),
        "every kind of exit: {:?}",
        seen.exits
    );
    assert!(
        seen.ran > blocks * 9 / 10,
        "most blocks run: {} of {blocks}",
        seen.ran
    );
    assert!(
        seen.refreshed > 200,
        "refresh charged inside blocks: {}",
        seen.refreshed
    );
    assert_eq!(seen.modelled, blocks * tairix_codegen::Target::ALL.len());
    for kind in KINDS {
        let count = seen.kinds.get(kind).copied().unwrap_or(0);
        assert!(count >= 10, "{kind} translated {count} times");
    }
    assert!(blocks > 1000, "{blocks} blocks");
}

extern crate std;

use tairix_riscos_kernel::module::Code;

use super::*;
use crate::c::testing::{Client, FREE};

mod recorded;

use recorded::RECORDED;

/// Entry `n`'s routine.
fn routine(n: usize) -> Code {
    match ENTRIES[n] {
        Entry::Own(code) => code,
        Entry::Shared { .. } => panic!("chunk 4 shares nothing"),
    }
}

/// The three comparisons, which answer in the flags.
const COMPARISONS: [usize; 3] = [39, 40, 41];

#[test]
fn every_recorded_call_answers_as_genuine_did() {
    let mut client = Client::with_heap();
    let mut disagreements = std::vec::Vec::new();
    for (entry, calls) in RECORDED {
        for &(given, answered, flags) in calls {
            let ours = if COMPARISONS.contains(&entry) {
                client.running_with(&given);
                let (done, registers, _) = client.subroutine(routine(entry));
                let flags_left = client.session.kernel.os().registers().flags >> 28;
                done.ok().map(|()| (registers[..4].to_vec(), flags_left))
            } else {
                client
                    .outcome(routine(entry), &given)
                    .ok()
                    .map(|regs| (regs.r[..4].to_vec(), 0))
            };
            let agreed = match (answered, &ours) {
                (None, None) => true,
                (Some(words), Some((got, got_flags))) => {
                    got[..words.len()] == *words && *got_flags == flags
                }
                _ => false,
            };
            if !agreed {
                disagreements.push(std::format!(
                    "{entry}({given:08X?}): genuine {answered:08X?} {flags:X}, ours {ours:08X?}"
                ));
            }
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

#[test]
fn a_comparison_keeps_v_which_a_signed_test_reads() {
    let mut client = Client::with_heap();
    client.running_with(&[0xFFFF_FFFF, 0x7FFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF]);
    let (done, registers, _) = client.subroutine(routine(40));
    assert!(done.is_ok());
    let flags = client.session.kernel.os().registers().flags;
    assert_eq!(
        registers[..2],
        [0, 0x8000_0000],
        "LLONG_MAX less minus one wraps"
    );
    assert_eq!(
        flags & (psr::N | psr::V),
        psr::N | psr::V,
        "so GE still holds: N equals V"
    );
}

#[test]
fn freeauto_marks_its_block_free_and_lists_nothing() {
    let mut client = Client::with_heap();
    let block = client.answer(routine(0), &[100]);
    assert_ne!(block, 0, "a block from the heap");
    assert_eq!(client.word(block - 4), 100, "sized as malloc sizes it");
    assert_eq!(client.answer(routine(1), &[block]), 0);
    assert_eq!(client.word(block - 4), 101, "its size word's free bit set");
    let next = client.answer(routine(0), &[100]);
    assert_ne!(next, block, "not listed, so not handed out again");
    assert_eq!(client.answer(routine(1), &[0]), 0, "nought is nothing");
    client.answer(routine(1), &[FREE]);
}

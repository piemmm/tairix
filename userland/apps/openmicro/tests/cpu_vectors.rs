//! Replays the committed `SingleStepTests` sample through the CPU.

use tairix_openmicro::cpu::vectors::{decode, Mismatch, Runner, Vector};

const SAMPLE: &[u8] = include_bytes!("vectors/sst6502.bin");

/// Sixteen cases of each opcode, and 48 more of each of the 32 opcodes
/// decimal mode changes.
const SAMPLE_CASES: usize = 256 * 16 + 32 * 48;

fn sample() -> Vec<Vector> {
    decode(SAMPLE).expect("the committed sample decodes")
}

fn opcode(vector: &Vector) -> u8 {
    let pc = vector.before.registers.pc;
    vector
        .before
        .ram
        .iter()
        .find(|&&(addr, _)| addr == pc)
        .map_or(0, |&(_, byte)| byte)
}

#[test]
fn the_cpu_matches_every_sampled_case() {
    let vectors = sample();
    assert_eq!(vectors.len(), SAMPLE_CASES);
    let mut runner = Runner::new();
    for (index, vector) in vectors.iter().enumerate() {
        if let Err(mismatch) = runner.check(vector) {
            panic!("case {index}, opcode &{:02X}: {mismatch}", opcode(vector));
        }
    }
}

#[test]
fn the_runner_reports_a_wrong_register() {
    let mut vector = sample().swap_remove(0);
    vector.after.registers.a ^= 0xFF;
    assert!(matches!(
        Runner::new().check(&vector),
        Err(Mismatch::Registers { .. })
    ));
}

#[test]
fn the_runner_reports_a_wrong_cycle() {
    let mut vector = sample().swap_remove(0);
    vector.cycles[0].addr ^= 0x0100;
    assert!(matches!(
        Runner::new().check(&vector),
        Err(Mismatch::Cycle { index: 0, .. })
    ));
    let mut vector = sample().swap_remove(0);
    vector.cycles.pop();
    assert!(matches!(
        Runner::new().check(&vector),
        Err(Mismatch::Cycle { expected: None, .. })
    ));
}

#[test]
fn the_runner_reports_wrong_memory() {
    let mut vector = sample().swap_remove(0);
    let entry = vector.after.ram.last_mut().expect("a case names memory");
    entry.1 ^= 0xFF;
    assert!(matches!(
        Runner::new().check(&vector),
        Err(Mismatch::Memory { .. })
    ));
}

#[test]
fn one_runner_carries_nothing_from_one_case_to_the_next() {
    let vectors = sample();
    let mut shared = Runner::new();
    for vector in vectors.iter().rev().take(64) {
        assert_eq!(shared.check(vector), Runner::new().check(vector));
    }
}

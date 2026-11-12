use super::super::bench::machine;
use super::*;
use crate::cpu::{CpuModel, Seg};

/// Whether `op` may move the 8080's PC anywhere but past itself: a return,
/// jump or call on condition, an RST, a JMP, RET, CALL or PCHL, and `ED`.
const fn transfers(op: u8) -> bool {
    matches!(op & 0xC7, 0xC0 | 0xC2 | 0xC4 | 0xC7)
        || matches!(
            op,
            0xC3 | 0xCB | 0xC9 | 0xD9 | 0xCD | 0xDD | 0xED | 0xFD | 0xE9
        )
}

#[test]
fn every_opcode_is_as_long_as_the_executor_reads_it() {
    for op in 0..=u8::MAX {
        let bytes = [op, 0x12, 0x34];
        let insn = decode(bytes);
        assert!(
            !insn.text.is_empty() && (1..=3).contains(&insn.len),
            "{op:02X}"
        );
        if transfers(op) {
            continue;
        }
        // BRKEM 80h into the 8080 code, which then runs one instruction.
        let (mut cpu, mut bench) = machine(CpuModel::V20, &[0x0F, 0xFF, 0x80]);
        bench.vector_to(0x80, 0x0700, 0);
        bench.load(0x0700, 0, &bytes);
        cpu.step(&mut bench, u64::MAX);
        assert!(cpu.in_8080_mode());
        cpu.step(&mut bench, u64::MAX);
        assert_eq!(cpu.regs().ip, u16::from(insn.len), "{op:02X} {}", insn.text);
        assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0700);
    }
}

#[test]
fn opcodes_read_in_intels_mnemonics_and_say_how_they_flow() {
    for (bytes, text, flow) in [
        ([0x00, 0, 0], "NOP", Flow::Other),
        ([0x38, 0, 0], "NOP", Flow::Other),
        ([0x21, 0x34, 0x12], "LXI H,1234h", Flow::Other),
        ([0x36, 0x7F, 0], "MVI M,7Fh", Flow::Other),
        ([0x7E, 0, 0], "MOV A,M", Flow::Other),
        ([0x96, 0, 0], "SUB M", Flow::Other),
        ([0xFE, 0x0D, 0], "CPI 0Dh", Flow::Other),
        ([0xCB, 0x00, 0x01], "JMP 0100h", Flow::Other),
        ([0xDA, 0x00, 0x01], "JC 0100h", Flow::Other),
        ([0xF5, 0, 0], "PUSH PSW", Flow::Other),
        ([0x39, 0, 0], "DAD SP", Flow::Other),
        ([0xD9, 0, 0], "RET", Flow::Returns(None)),
        ([0xE8, 0, 0], "RPE", Flow::Returns(Some(5))),
        ([0xFD, 0x00, 0x02], "CALL 0200h", Flow::Calls),
        ([0xEC, 0x00, 0x02], "CPE 0200h", Flow::Calls),
        ([0xFF, 0, 0], "RST 7", Flow::Calls),
        ([0xED, 0xED, 0x10], "CALLN 10h", Flow::Calls),
        ([0xED, 0x00, 0x30], "CALL 3000h", Flow::Calls),
        ([0xED, 0xFD, 0], "RETEM", Flow::LeavesEmulation),
    ] {
        let insn = decode(bytes);
        assert_eq!(
            (insn.text.as_str(), insn.flow),
            (text, flow),
            "{bytes:02X?}"
        );
    }
}

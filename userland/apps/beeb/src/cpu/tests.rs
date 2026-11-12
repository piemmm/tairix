use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use core::ops::Range;

use super::disasm::Instruction;
use super::isa::{encode, Mnemonic, Mode, OPCODES};
use super::{flag, Bus, Cpu, Registers, ADDRESS_SPACE, IRQ_VECTOR, NMI_VECTOR, RESET_VECTOR};

/// Flat memory, a log of every cycle, and interrupt lines asserted over
/// ranges of cycle counts (the count of cycles completed when sampled).
struct TestBus {
    memory: Box<[u8]>,
    log: Vec<(u16, u8, bool)>,
    irq: Range<usize>,
    nmi: Range<usize>,
}

impl TestBus {
    fn new() -> Self {
        Self {
            memory: alloc::vec![0; ADDRESS_SPACE].into_boxed_slice(),
            log: Vec::new(),
            irq: 0..0,
            nmi: 0..0,
        }
    }

    fn load(&mut self, at: u16, bytes: &[u8]) {
        let start = usize::from(at);
        self.memory[start..start + bytes.len()].copy_from_slice(bytes);
    }

    fn set_vector(&mut self, vector: u16, target: u16) {
        self.load(vector, &target.to_le_bytes());
    }

    fn addresses(&self) -> Vec<u16> {
        self.log.iter().map(|&(addr, _, _)| addr).collect()
    }
}

impl Bus for TestBus {
    fn read(&mut self, addr: u16) -> u8 {
        let data = self.memory[usize::from(addr)];
        self.log.push((addr, data, false));
        data
    }

    fn write(&mut self, addr: u16, data: u8) {
        self.memory[usize::from(addr)] = data;
        self.log.push((addr, data, true));
    }

    fn irq(&self) -> bool {
        self.irq.contains(&self.log.len())
    }

    fn nmi(&self) -> bool {
        self.nmi.contains(&self.log.len())
    }
}

/// A processor past reset, at `pc`, with interrupts enabled or not.
fn running_at(pc: u16, interrupts_enabled: bool) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.reset_pending = false;
    cpu.set_registers(Registers {
        a: 0,
        x: 0,
        y: 0,
        s: 0xFD,
        p: if interrupts_enabled { 0 } else { flag::I },
        pc,
    });
    cpu
}

#[test]
fn reset_takes_seven_cycles_and_the_reset_vector() {
    let mut bus = TestBus::new();
    bus.set_vector(RESET_VECTOR, 0xD9CD);
    let mut cpu = Cpu::new();
    cpu.step(&mut bus);
    assert_eq!(bus.log.len(), 7);
    let r = cpu.registers();
    assert_eq!(r.pc, 0xD9CD);
    assert_eq!(r.s, 0xFD, "reset decrements S three times without writing");
    assert!(bus.log.iter().all(|&(_, _, write)| !write));
    assert_ne!(r.p & flag::I, 0);
}

#[test]
fn an_indexed_read_reads_the_uncorrected_address_only_across_a_page() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[0xBD, 0xFF, 0x12, 0xBD, 0x00, 0x12]);
    bus.load(0x1300, &[0x5A]);
    let mut cpu = running_at(0x0200, false);
    cpu.x = 1;
    cpu.step(&mut bus);
    assert_eq!(bus.addresses(), [0x0200, 0x0201, 0x0202, 0x1200, 0x1300]);
    assert_eq!(cpu.a, 0x5A);
    bus.log.clear();
    cpu.step(&mut bus);
    assert_eq!(bus.addresses(), [0x0203, 0x0204, 0x0205, 0x1201]);
}

#[test]
fn an_indexed_store_always_reads_the_uncorrected_address_first() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[0x9D, 0x00, 0x12]);
    let mut cpu = running_at(0x0200, false);
    cpu.x = 4;
    cpu.a = 0x77;
    cpu.step(&mut bus);
    assert_eq!(
        bus.log,
        [
            (0x0200, 0x9D, false),
            (0x0201, 0x00, false),
            (0x0202, 0x12, false),
            (0x1204, 0x00, false),
            (0x1204, 0x77, true),
        ]
    );
}

#[test]
fn read_modify_write_writes_the_old_value_then_the_new() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[0xEE, 0x40, 0xFE]);
    bus.load(0xFE40, &[0x7F]);
    let mut cpu = running_at(0x0200, false);
    cpu.step(&mut bus);
    assert_eq!(
        &bus.log[3..],
        [
            (0xFE40, 0x7F, false),
            (0xFE40, 0x7F, true),
            (0xFE40, 0x80, true)
        ]
    );
    assert!(cpu.n && !cpu.z);
}

#[test]
fn indirect_jump_takes_its_high_byte_from_the_same_page() {
    let mut bus = TestBus::new();
    bus.load(0x0300, &[0x6C, 0xFF, 0x02]);
    bus.load(0x02FF, &[0x34]);
    bus.load(0x0200, &[0x12]);
    let mut cpu = running_at(0x0300, false);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x1234);
}

#[test]
fn subroutine_call_and_return_resume_after_the_operand() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[0x20, 0x00, 0x30, 0xEA]);
    bus.load(0x3000, &[0x60]);
    let mut cpu = running_at(0x0200, false);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x3000);
    assert_eq!(bus.log.len(), 6);
    assert_eq!(
        &bus.memory[0x01FC..=0x01FD],
        [0x02, 0x02],
        "return address - 1"
    );
    bus.log.clear();
    cpu.step(&mut bus);
    assert_eq!(bus.log.len(), 6);
    assert_eq!(cpu.pc, 0x0203);
}

#[test]
fn brk_pushes_break_and_skips_its_padding_byte() {
    let mut bus = TestBus::new();
    bus.set_vector(IRQ_VECTOR, 0x4000);
    bus.load(0x0200, &[0x00, 0xFF]);
    bus.load(0x4000, &[0x40]);
    let mut cpu = running_at(0x0200, true);
    cpu.c = true;
    cpu.step(&mut bus);
    assert_eq!(bus.log.len(), 7);
    assert_eq!(cpu.pc, 0x4000);
    assert_eq!(
        bus.memory[0x01FB] & (flag::B | flag::U | flag::C),
        flag::B | flag::U | flag::C
    );
    assert!(cpu.i);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0202);
    assert!(!cpu.i, "RTI restores the pushed status");
}

/// The decimal value of a BCD byte.
fn bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0F)
}

/// The BCD byte of a value below 100.
fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

#[test]
fn decimal_arithmetic_on_valid_bcd_is_base_ten() {
    for a in (0..100).map(to_bcd) {
        for m in (0..100).map(to_bcd) {
            for carry in [false, true] {
                let mut cpu = running_at(0, false);
                cpu.d = true;
                cpu.a = a;
                cpu.c = carry;
                cpu.adc(m);
                let sum = u16::from(bcd(a)) + u16::from(bcd(m)) + u16::from(carry);
                assert_eq!(u16::from(bcd(cpu.a)), sum % 100, "{a:02X}+{m:02X}+{carry}");
                assert_eq!(cpu.c, sum >= 100);

                let mut cpu = running_at(0, false);
                cpu.d = true;
                cpu.a = a;
                cpu.c = carry;
                cpu.sbc(m);
                let difference = i16::from(bcd(a)) - i16::from(bcd(m)) - i16::from(!carry);
                assert_eq!(
                    i16::from(bcd(cpu.a)),
                    difference.rem_euclid(100),
                    "{a:02X}-{m:02X}-{}",
                    !carry
                );
                assert_eq!(cpu.c, difference >= 0);
            }
        }
    }
}

#[test]
fn an_irq_after_cli_waits_one_instruction() {
    let mut bus = TestBus::new();
    bus.set_vector(IRQ_VECTOR, 0x4000);
    bus.load(0x0200, &[0x58, 0xEA, 0xEA]);
    bus.irq = 0..usize::MAX;
    let mut cpu = running_at(0x0200, false);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0201);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0202, "the instruction after CLI runs first");
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_eq!(&bus.memory[0x01FC..=0x01FD], [0x02, 0x02]);
}

#[test]
fn an_irq_pending_at_sei_is_still_taken() {
    let mut bus = TestBus::new();
    bus.set_vector(IRQ_VECTOR, 0x4000);
    bus.load(0x0200, &[0x78, 0xEA]);
    bus.irq = 0..usize::MAX;
    let mut cpu = running_at(0x0200, true);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_ne!(bus.memory[0x01FB] & flag::I, 0, "pushed after SEI set I");
    assert_eq!(
        bus.memory[0x01FB] & flag::B,
        0,
        "a hardware interrupt clears B"
    );
}

#[test]
fn a_taken_branch_in_its_page_defers_an_irq_seen_on_its_operand_cycle() {
    let mut bus = TestBus::new();
    bus.set_vector(IRQ_VECTOR, 0x4000);
    // BNE +0 (taken, same page), then NOP.
    bus.load(0x0200, &[0xD0, 0x00, 0xEA]);
    bus.irq = 2..usize::MAX;
    let mut cpu = running_at(0x0200, true);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0202);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0203, "the NOP runs before the deferred IRQ");
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x4000);
}

#[test]
fn an_irq_seen_on_a_page_crossing_branch_is_taken_after_it() {
    let mut bus = TestBus::new();
    bus.set_vector(IRQ_VECTOR, 0x4000);
    // BNE +&10 from &02F0 crosses into page 3.
    bus.load(0x02F0, &[0xD0, 0x10]);
    bus.irq = 2..usize::MAX;
    let mut cpu = running_at(0x02F0, true);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0302);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x4000);
}

#[test]
fn nmi_is_taken_once_per_edge() {
    let mut bus = TestBus::new();
    bus.set_vector(NMI_VECTOR, 0x0D00);
    bus.load(0x0200, &[0xEA, 0xEA, 0xEA]);
    bus.load(0x0D00, &[0xEA, 0xEA, 0xEA, 0xEA]);
    bus.nmi = 0..usize::MAX;
    let mut cpu = running_at(0x0200, false);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0D00, "NMI ignores the interrupt mask");
    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.pc, 0x0D03, "a held line is one edge");
}

#[test]
fn an_nmi_during_brk_takes_over_the_vector() {
    let mut bus = TestBus::new();
    bus.set_vector(IRQ_VECTOR, 0x4000);
    bus.set_vector(NMI_VECTOR, 0x0D00);
    bus.load(0x0200, &[0x00, 0x00]);
    bus.nmi = 3..usize::MAX;
    let mut cpu = running_at(0x0200, false);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0D00);
    assert_ne!(
        bus.memory[0x01FB] & flag::B,
        0,
        "the hijacked BRK still pushes B"
    );
}

#[test]
fn jam_halts_until_reset() {
    let mut bus = TestBus::new();
    bus.set_vector(RESET_VECTOR, 0xC000);
    bus.load(0x0200, &[0x02]);
    bus.irq = 0..usize::MAX;
    bus.nmi = 0..usize::MAX;
    let mut cpu = running_at(0x0200, true);
    for _ in 0..10 {
        cpu.step(&mut bus);
    }
    assert!(cpu.is_jammed());
    assert_eq!(cpu.pc, 0x0201);
    assert_eq!(
        &bus.addresses()[2..6],
        [0xFFFF, 0xFFFE, 0xFFFE, 0xFFFF],
        "the halted bus"
    );
    cpu.reset();
    cpu.step(&mut bus);
    assert!(!cpu.is_jammed());
    assert_eq!(cpu.pc, 0xC000);
}

#[test]
fn the_table_documents_exactly_the_published_instruction_set() {
    let documented = OPCODES.iter().filter(|op| op.documented).count();
    assert_eq!(documented, 151);
    let mnemonics: Vec<Mnemonic> = OPCODES
        .iter()
        .filter(|op| op.documented)
        .map(|op| op.mnemonic)
        .fold(Vec::new(), |mut seen, mnemonic| {
            if !seen.contains(&mnemonic) {
                seen.push(mnemonic);
            }
            seen
        });
    assert_eq!(mnemonics.len(), 56);
}

#[test]
fn encoding_inverts_the_table_for_documented_opcodes() {
    for (code, opcode) in OPCODES.iter().enumerate() {
        let encoded = encode(opcode.mnemonic, opcode.mode);
        if opcode.documented {
            assert_eq!(encoded.map(usize::from), Some(code));
        }
    }
    assert_eq!(encode(Mnemonic::Sbc, Mode::Immediate), Some(0xE9));
    assert_eq!(encode(Mnemonic::Lax, Mode::ZeroPage), None, "undocumented");
    assert_eq!(encode(Mnemonic::Sta, Mode::Immediate), None);
}

#[test]
fn disassembly_uses_acorn_syntax() {
    let text = |pc: u16, bytes: [u8; 3]| format!("{}", Instruction::decode(pc, bytes));
    assert_eq!(text(0, [0xA9, 0x20, 0]), "LDA #&20");
    assert_eq!(text(0, [0xB1, 0x70, 0]), "LDA (&70),Y");
    assert_eq!(text(0, [0x6C, 0x0E, 0x02]), "JMP (&020E)");
    assert_eq!(text(0, [0x0A, 0, 0]), "ASL A");
    assert_eq!(text(0x1000, [0xD0, 0xFE, 0]), "BNE &1000");
    assert_eq!(text(0, [0x9D, 0x00, 0x7C]), "STA &7C00,X");
}

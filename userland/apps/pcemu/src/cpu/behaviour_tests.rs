//! What the vectors cannot show: interrupts, the queue, resumption, the
//! host call, 8080 emulation and the idioms software detects CPUs by.

use super::*;
use alloc::vec;
use alloc::vec::Vec;

struct TestBus {
    ram: Vec<u8>,
    intr: bool,
    vector: u8,
    nmi: bool,
    rom: core::ops::Range<u32>,
    /// The cycle from which TEST reads low.
    test_low: u64,
    /// The ESC instructions handed over.
    escapes: Vec<bus::Escape>,
}

impl TestBus {
    fn new() -> Self {
        Self {
            ram: vec![0; 1 << 20],
            intr: false,
            vector: 0,
            nmi: false,
            rom: 0..0,
            test_low: 0,
            escapes: Vec::new(),
        }
    }

    fn load(&mut self, segment: u16, offset: u16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            let at = linear(segment, offset.wrapping_add(u16::try_from(i).unwrap()));
            self.ram[at as usize] = b;
        }
    }

    fn vector_to(&mut self, vector: u8, segment: u16, offset: u16) {
        let at = usize::from(vector) * 4;
        self.ram[at..at + 4].copy_from_slice(&[lo(offset), hi(offset), lo(segment), hi(segment)]);
    }

    fn word(&self, segment: u16, offset: u16) -> u16 {
        let lo = self.ram[linear(segment, offset) as usize];
        let hi = self.ram[linear(segment, offset.wrapping_add(1)) as usize];
        crate::bits::word(lo, hi)
    }
}

use crate::bits::{hi, lo};

impl Bus for TestBus {
    fn read(&mut self, addr: u32, _at: u64) -> u8 {
        self.ram[addr as usize]
    }
    fn write(&mut self, addr: u32, value: u8, _at: u64) {
        self.ram[addr as usize] = value;
    }
    fn peek(&self, addr: u32) -> u8 {
        self.ram[addr as usize]
    }
    fn input(&mut self, _port: u16, _at: u64) -> u8 {
        0xFF
    }
    fn output(&mut self, _port: u16, _value: u8, _at: u64) {}
    fn intr(&self) -> bool {
        self.intr
    }
    fn acknowledge(&mut self) -> u8 {
        self.intr = false;
        self.vector
    }
    fn nmi(&self) -> bool {
        self.nmi
    }
    fn acknowledge_nmi(&mut self) {
        self.nmi = false;
    }
    fn host_code(&self, addr: u32) -> bool {
        self.rom.contains(&addr)
    }
    fn escape(&mut self, escape: bus::Escape, _at: u64) {
        self.escapes.push(escape);
    }
    fn test_low_at(&mut self, at: u64) -> u64 {
        self.test_low.max(at)
    }
    // No access waits, so none is ever pending.
    fn steal(&mut self, _cycles: u32) {}
}

const CODE: u16 = 0x0050;
const STACK: u16 = 0x0900;

fn machine(model: CpuModel, code: &[u8]) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.load(CODE, 0, code);
    let mut cpu = Cpu::new(model);
    let regs = cpu.regs_mut();
    regs.set_sreg(Seg::Cs, CODE);
    regs.ip = 0;
    regs.set_sreg(Seg::Ss, STACK);
    regs.set_sreg(Seg::Ds, CODE);
    regs.set_sreg(Seg::Es, CODE);
    regs.gpr[gpr::SP] = 0x0100;
    (cpu, bus)
}

/// Runs `n` instructions, prefixes and interrupt entries included.
fn steps(cpu: &mut Cpu, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.step(bus, u64::MAX);
    }
}

#[test]
fn sti_enables_interrupts_after_the_next_instruction() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0xFB, 0x90, 0x90, 0x90]);
    bus.vector_to(0x20, 0x0100, 0x0000);
    bus.intr = true;
    bus.vector = 0x20;
    steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        (cpu.regs().sreg(Seg::Cs), cpu.regs().ip),
        (CODE, 2),
        "STI's shadow covers the NOP"
    );
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(
        cpu.regs().sreg(Seg::Cs),
        0x0100,
        "the interrupt is taken before the second NOP"
    );
    assert_eq!(
        bus.word(STACK, cpu.regs().sp()),
        2,
        "it returns to the second NOP"
    );
}

#[test]
fn a_segment_load_shadows_the_next_instruction_where_the_model_does() {
    // MOV DS, AX; NOP; NOP — the 8086 family inhibits after any segment
    // load, the 80186 only after SS.
    for (model, shadowed) in [(CpuModel::I8086, true), (CpuModel::I80186, false)] {
        let (mut cpu, mut bus) = machine(model, &[0x8E, 0xD8, 0x90, 0x90]);
        cpu.regs_mut().flags |= flag::IF;
        bus.vector_to(0x21, 0x0200, 0);
        steps(&mut cpu, &mut bus, 1);
        bus.intr = true;
        bus.vector = 0x21;
        steps(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.regs().sreg(Seg::Cs) == CODE, shadowed, "{model:?}");
    }
}

#[test]
fn nmi_wakes_hlt_and_returns_after_it() {
    let (mut cpu, mut bus) = machine(CpuModel::I8086, &[0xF4, 0x90]);
    bus.vector_to(2, 0x0300, 0);
    assert_eq!(cpu.step(&mut bus, u64::MAX), Some(Stop::Halted));
    assert_eq!(
        cpu.step(&mut bus, u64::MAX),
        Some(Stop::Halted),
        "nothing pending keeps it halted"
    );
    bus.nmi = true;
    cpu.step(&mut bus, u64::MAX);
    assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0300);
    assert_eq!(
        bus.word(STACK, cpu.regs().sp()),
        1,
        "the NMI returns past HLT"
    );
}

#[test]
fn the_v20_leaves_hlt_on_intr_with_interrupts_disabled() {
    for (model, leaves) in [(CpuModel::V20, true), (CpuModel::I8088, false)] {
        let (mut cpu, mut bus) = machine(model, &[0xF4, 0x90, 0x90]);
        cpu.step(&mut bus, u64::MAX);
        bus.intr = true;
        let stop = cpu.step(&mut bus, u64::MAX);
        assert_eq!(stop != Some(Stop::Halted), leaves, "{model:?}");
        assert_eq!(
            cpu.regs().sreg(Seg::Cs),
            CODE,
            "{model:?} takes no interrupt with IF clear"
        );
    }
}

#[test]
fn a_flag_set_by_popf_traps_after_the_following_instruction() {
    // PUSH 0x0100 is unavailable on the 8086, so: MOV AX,0x0100; PUSH AX;
    // POPF; NOP; NOP.
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0xB8, 0x00, 0x01, 0x50, 0x9D, 0x90, 0x90]);
    bus.vector_to(1, 0x0400, 0);
    steps(&mut cpu, &mut bus, 3);
    assert_eq!(
        cpu.regs().sreg(Seg::Cs),
        CODE,
        "no trap after the POPF that set TF"
    );
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(
        cpu.regs().sreg(Seg::Cs),
        0x0400,
        "the trap follows the next instruction"
    );
    assert_eq!(bus.word(STACK, cpu.regs().sp()), 6);
    assert!(!cpu.regs().flag(flag::TF), "the handler runs untraced");
}

#[test]
fn an_interrupted_rep_resumes_where_each_model_returns() {
    // ES: REP MOVSB, interrupted after its first iteration: the 8086 family
    // returns to its last prefix, so the override is lost; the V-series keeps
    // every prefix.
    for (model, resume) in [
        (CpuModel::I8088, 1u16),
        (CpuModel::I80188, 1),
        (CpuModel::V20, 0),
    ] {
        let (mut cpu, mut bus) = machine(model, &[0x26, 0xF3, 0xA4, 0x90]);
        cpu.regs_mut().gpr[gpr::CX] = 10;
        cpu.regs_mut().gpr[gpr::SI] = 0x0200;
        cpu.regs_mut().gpr[gpr::DI] = 0x0300;
        cpu.regs_mut().flags |= flag::IF;
        bus.vector_to(0x30, 0x0500, 0);
        bus.vector = 0x30;
        steps(&mut cpu, &mut bus, 1);
        bus.intr = true;
        steps(&mut cpu, &mut bus, 3);
        assert_eq!(
            cpu.regs().sreg(Seg::Cs),
            0x0500,
            "{model:?} takes the interrupt mid-string"
        );
        assert_eq!(bus.word(STACK, cpu.regs().sp()), resume, "{model:?}");
    }
}

#[test]
fn an_interrupt_let_in_by_a_paused_rep_returns_as_one_inside_it_would() {
    // ES: REP MOVSB paused at the cycle limit after an iteration, and an
    // interrupt raised then, as a device's event at the limit raises one: it
    // returns where one taken inside the string would, not to the first
    // prefix the pause itself resumes at.
    for (model, resume) in [
        (CpuModel::I8088, 1u16),
        (CpuModel::I80188, 1),
        (CpuModel::V20, 0),
    ] {
        let (mut cpu, mut bus) = machine(model, &[0x26, 0xF3, 0xA4, 0x90]);
        cpu.regs_mut().gpr[gpr::CX] = 10;
        cpu.regs_mut().flags |= flag::IF;
        bus.vector_to(0x30, 0x0500, 0);
        bus.vector = 0x30;
        steps(&mut cpu, &mut bus, 2);
        let limit = cpu.cycles() + 1;
        assert_eq!(cpu.step(&mut bus, limit), None);
        assert_eq!(cpu.regs().cx(), 9, "{model:?} paused after an iteration");
        assert!(cpu.resumes_string());
        bus.intr = true;
        steps(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0500, "{model:?}");
        assert_eq!(bus.word(STACK, cpu.regs().sp()), resume, "{model:?}");
        assert!(!cpu.resumes_string());
    }
}

#[test]
fn a_traced_rep_traps_where_the_limit_falls_as_anywhere_else() {
    // With TF set the string yields to the trap after every iteration,
    // whether or not the cycle limit falls there: the trap returns to the
    // 8088's last prefix either way.
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x26, 0xF3, 0xA4, 0x90]);
    cpu.regs_mut().gpr[gpr::CX] = 10;
    cpu.regs_mut().flags |= flag::TF;
    bus.vector_to(1, 0x0500, 0);
    steps(&mut cpu, &mut bus, 2);
    let limit = cpu.cycles() + 1;
    assert_eq!(cpu.step(&mut bus, limit), None);
    assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0500, "the trap was taken");
    assert_eq!(bus.word(STACK, cpu.regs().sp()), 1);
    assert!(!cpu.resumes_string());
}

#[test]
fn a_rep_stopped_by_the_limit_resumes_exactly() {
    let code = [0x26, 0xF3, 0xA4, 0x90];
    let fill = |bus: &mut TestBus| {
        for i in 0..64u16 {
            bus.load(CODE, 0x0200 + i, &[u8::try_from(i).unwrap() ^ 0x5A]);
        }
    };
    let (mut whole, mut whole_bus) = machine(CpuModel::I8086, &code);
    let (mut split, mut split_bus) = machine(CpuModel::I8086, &code);
    for (cpu, bus) in [(&mut whole, &mut whole_bus), (&mut split, &mut split_bus)] {
        fill(bus);
        cpu.regs_mut().gpr[gpr::CX] = 64;
        cpu.regs_mut().gpr[gpr::SI] = 0x0200;
        cpu.regs_mut().gpr[gpr::DI] = 0x0400;
    }
    while whole.regs().ip != 3 {
        whole.step(&mut whole_bus, u64::MAX);
    }
    let mut limit = split.cycles();
    let mut slices = 0;
    while split.regs().ip != 3 {
        limit += 50;
        split.step(&mut split_bus, limit);
        slices += 1;
    }
    assert!(slices > 10, "the string was paused and resumed");
    assert_eq!(split.regs().cx(), 0);
    assert_eq!(&split_bus.ram[..0x10_000], &whole_bus.ram[..0x10_000]);
    assert_eq!(split.regs().gpr, whole.regs().gpr);
}

#[test]
fn the_queue_hides_a_store_into_bytes_already_fetched() {
    // MOV BYTE [0x0009], 0x40 (INC AX), then four NOPs and a NOP at 9.
    // The 8088's four-byte queue already holds offsets 5–8, so the store at
    // distance 4 takes; the 8086's six-byte queue holds it, so the old NOP
    // runs. The classic test that tells them apart.
    let code = [
        0xC6, 0x06, 0x09, 0x00, 0x40, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    ];
    for (model, executed_inc) in [(CpuModel::I8088, true), (CpuModel::I8086, false)] {
        let (mut cpu, mut bus) = machine(model, &code);
        steps(&mut cpu, &mut bus, 6);
        assert_eq!(cpu.regs().ax() == 1, executed_inc, "{model:?}");
        assert_eq!(
            bus.ram[linear(CODE, 9) as usize],
            0x40,
            "memory always changes"
        );
    }
}

#[test]
fn pop_cs_keeps_executing_the_old_queue() {
    // MOV AX,0x0060; PUSH AX; POP CS, then INC AX ×5 in the old segment and
    // NOPs in the new one: the 8088's four queued INCs run first.
    let (mut cpu, mut bus) = machine(
        CpuModel::I8088,
        &[0xB8, 0x60, 0x00, 0x50, 0x0F, 0x40, 0x40, 0x40, 0x40, 0x40],
    );
    bus.load(0x0060, 0, &[0x90; 16]);
    steps(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0060);
    steps(&mut cpu, &mut bus, 6);
    assert_eq!(
        cpu.regs().ax(),
        0x64,
        "four queued INCs, then the new segment's NOPs"
    );
}

#[test]
fn the_host_call_stops_only_inside_the_emulators_rom() {
    let call = [0xFE, 0x38, 0x34, 0x12, 0x90];
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &call);
    bus.rom = linear(CODE, 0)..linear(CODE, 0x100);
    assert_eq!(cpu.step(&mut bus, u64::MAX), Some(Stop::HostCall(0x1234)));
    assert_eq!(cpu.regs().ip, 4, "the CPU resumes after the call");
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &call);
    assert_eq!(
        cpu.step(&mut bus, u64::MAX),
        None,
        "outside the ROM it is the CPU's own FE /7"
    );
    assert_eq!(cpu.regs().ip, 2);
}

#[test]
fn brkem_runs_8080_code_until_retem() {
    // BRKEM 0x80 to an 8080 routine: MVI A,41h; INR A; MOV B,A; RETEM.
    let (mut cpu, mut bus) = machine(CpuModel::V20, &[0x0F, 0xFF, 0x80, 0x90]);
    bus.vector_to(0x80, 0x0700, 0);
    bus.load(0x0700, 0, &[0x3E, 0x41, 0x3C, 0x47, 0xED, 0xFD]);
    steps(&mut cpu, &mut bus, 1);
    assert!(cpu.in_8080_mode());
    steps(&mut cpu, &mut bus, 4);
    assert!(!cpu.in_8080_mode());
    assert_eq!(cpu.regs().get8(0), 0x42, "A lives in AL");
    assert_eq!(cpu.regs().get8(5), 0x42, "B lives in CH");
    assert_eq!(
        (cpu.regs().sreg(Seg::Cs), cpu.regs().ip),
        (CODE, 3),
        "RETEM returns past BRKEM"
    );
    assert!(cpu.regs().flag(flag::MD));
}

#[test]
fn calln_and_iret_round_trip_through_native_code() {
    // In 8080 mode, CALLN 0x81 runs a native handler (INC AX; IRET), then
    // 8080 execution resumes: INR A; RETEM.
    let (mut cpu, mut bus) = machine(CpuModel::V20, &[0x0F, 0xFF, 0x80]);
    bus.vector_to(0x80, 0x0700, 0);
    bus.vector_to(0x81, 0x0800, 0);
    bus.load(0x0700, 0, &[0xED, 0xED, 0x81, 0x3C, 0xED, 0xFD]);
    bus.load(0x0800, 0, &[0x40, 0xCF]);
    steps(&mut cpu, &mut bus, 2);
    assert!(!cpu.in_8080_mode(), "the CALLN handler runs native");
    steps(&mut cpu, &mut bus, 2);
    assert!(
        cpu.in_8080_mode(),
        "IRET restores the emulation mode CALLN saved"
    );
    steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        cpu.regs().get8(0),
        2,
        "one INC in native mode, one INR in 8080 mode"
    );
    assert!(!cpu.in_8080_mode());
}

#[test]
fn detection_idioms_tell_the_models_apart() {
    // MOV CL,33; MOV AX,1; SHL AX,CL — only the 80186 masks the count.
    for (model, ax) in [
        (CpuModel::I8088, 0),
        (CpuModel::V20, 0),
        (CpuModel::I80186, 2),
    ] {
        let (mut cpu, mut bus) = machine(model, &[0xB1, 0x21, 0xB8, 0x01, 0x00, 0xD3, 0xE0]);
        steps(&mut cpu, &mut bus, 3);
        assert_eq!(cpu.regs().ax(), ax, "{model:?}");
    }
    // XOR AX,AX; PUSH AX; POPF; PUSHF; POP AX — bits 12–15 stay set.
    for model in CpuModel::ALL {
        let (mut cpu, mut bus) = machine(model, &[0x31, 0xC0, 0x50, 0x9D, 0x9C, 0x58]);
        steps(&mut cpu, &mut bus, 5);
        assert_eq!(cpu.regs().ax() & 0xF000, 0xF000, "{model:?}");
    }
    // MOV AX,0x0105; AAD 7 — the V-series ignores the base.
    for (model, al) in [(CpuModel::I8086, 12), (CpuModel::V30, 15)] {
        let (mut cpu, mut bus) = machine(model, &[0xB8, 0x05, 0x01, 0xD5, 0x07]);
        steps(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.regs().get8(0), al, "{model:?}");
    }
    // PUSH SP pushes the decremented value on every modelled CPU.
    for model in CpuModel::ALL {
        let (mut cpu, mut bus) = machine(model, &[0x54]);
        steps(&mut cpu, &mut bus, 1);
        assert_eq!(bus.word(STACK, 0x00FE), 0x00FE, "{model:?}");
    }
}

#[test]
fn exceptions_return_where_each_model_returns() {
    // An unused opcode on the 80186 returns to itself.
    let (mut cpu, mut bus) = machine(CpuModel::I80186, &[0x90, 0x0F]);
    bus.vector_to(6, 0x0A00, 0);
    steps(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0A00);
    assert_eq!(bus.word(STACK, cpu.regs().sp()), 1);
    // DIV by zero returns past the DIV on the 8086 family.
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0xF6, 0xF1, 0x90]);
    bus.vector_to(0, 0x0B00, 0);
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(
        (cpu.regs().sreg(Seg::Cs), bus.word(STACK, cpu.regs().sp())),
        (0x0B00, 2)
    );
    // BOUND out of range: the 80186 returns to it, the V-series past it.
    for (model, ret) in [(CpuModel::I80186, 0u16), (CpuModel::V20, 4)] {
        let (mut cpu, mut bus) = machine(model, &[0x62, 0x06, 0x00, 0x02, 0x90]);
        bus.load(CODE, 0x0200, &[0x10, 0x00, 0x20, 0x00]);
        cpu.regs_mut().gpr[gpr::AX] = 0x0030;
        bus.vector_to(5, 0x0C00, 0);
        steps(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0C00, "{model:?}");
        assert_eq!(bus.word(STACK, cpu.regs().sp()), ret, "{model:?}");
    }
}

#[test]
fn wait_takes_its_base_clocks_and_five_a_look_while_test_reads_high() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x9B, 0x90]);
    assert_eq!(cpu.step(&mut bus, u64::MAX), None);
    let base = u64::from(timing::costs(CpuModel::I8088.family()).wait);
    let began = cpu.cycles() - base;
    // Again with TEST high for 12 clocks after the WAIT began: three looks
    // find it high.
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x9B, 0x90]);
    bus.test_low = began + 12;
    assert_eq!(cpu.step(&mut bus, u64::MAX), Some(Stop::Halted));
    assert!(cpu.waiting());
    assert!(!cpu.can_enter_block(&bus));
    cpu.idle_until(began + 12);
    assert_eq!(cpu.step(&mut bus, u64::MAX), None);
    assert!(!cpu.waiting());
    assert_eq!(cpu.cycles(), began + base + 15);
    assert_eq!(cpu.regs().ip, 1, "past the WAIT");
}

#[test]
fn an_interrupt_taken_while_waiting_returns_to_the_wait() {
    let (mut cpu, mut bus) = machine(CpuModel::I8086, &[0x90, 0x9B, 0x90]);
    cpu.regs_mut().flags |= flag::IF;
    bus.test_low = u64::MAX;
    bus.vector_to(0x30, 0x0300, 0);
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.step(&mut bus, u64::MAX), Some(Stop::Halted));
    assert_eq!(
        cpu.step(&mut bus, u64::MAX),
        Some(Stop::Halted),
        "still waiting"
    );
    bus.intr = true;
    bus.vector = 0x30;
    assert_eq!(cpu.step(&mut bus, u64::MAX), None);
    assert_eq!(cpu.regs().sreg(Seg::Cs), 0x0300);
    assert_eq!(
        bus.word(STACK, cpu.regs().sp()),
        1,
        "the WAIT's own address"
    );
    assert!(!cpu.waiting());
    // NMI likewise.
    let (mut cpu, mut bus) = machine(CpuModel::V20, &[0x9B]);
    bus.test_low = u64::MAX;
    bus.vector_to(2, 0x0400, 0);
    assert_eq!(cpu.step(&mut bus, u64::MAX), Some(Stop::Halted));
    bus.nmi = true;
    cpu.step(&mut bus, u64::MAX);
    assert_eq!(bus.word(STACK, cpu.regs().sp()), 0);
}

#[test]
fn an_escape_hands_over_its_opcode_bits_its_address_and_its_operand() {
    // MOV BX,0010h; ES: FLD QWORD [BX+5]; FADD ST,ST(1).
    let (mut cpu, mut bus) = machine(
        CpuModel::I8088,
        &[0xBB, 0x10, 0x00, 0x26, 0xDD, 0x47, 0x05, 0xD8, 0xC1],
    );
    cpu.regs_mut().set_sreg(Seg::Es, 0x2000);
    bus.load(0x2000, 0x15, &[0x34, 0x12]);
    steps(&mut cpu, &mut bus, 4);
    assert_eq!(
        bus.escapes,
        [
            bus::Escape {
                opcode: 0x547,
                address: linear(CODE, 4),
                operand: Some((linear(0x2000, 0x15), 0x1234)),
            },
            bus::Escape {
                opcode: 0x0C1,
                address: linear(CODE, 7),
                operand: None,
            },
        ]
    );
}

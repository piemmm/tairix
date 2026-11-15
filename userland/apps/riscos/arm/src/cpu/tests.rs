//! The processor's instruction classes, exceptions and models, each run
//! from a program on a flat memory.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::{BadState, Cpu, Exception, Run, State, Stop, Unmodelled};
use crate::a32::decode;
use crate::bus::{Abort, Bus, Fetch, Width};
use crate::fpa::{self, Internal};
use crate::instruction::{Op, Set};
use crate::profile::{Profile, BIT26, BIT32};
use crate::psr::{self, Mode};
use crate::reg::Reg;
use crate::t32::{self, Position};

/// Memory: 64 KiB at zero and a page of high vectors, with a page that
/// refuses writes, a page the bus does not map, and host pages.
pub(super) struct Ram {
    low: Vec<u8>,
    high: Vec<u8>,
    host: Vec<u32>,
    /// Each data access: address, width, write, privileged.
    accesses: Vec<(u32, Width, bool, bool)>,
}

pub(super) const LOW: u32 = 0x1_0000;
const HIGH: u32 = 0xFFFF_0000;
const READ_ONLY: u32 = 0xE000;
pub(super) const UNMAPPED: u32 = 0xF000;

impl Ram {
    pub(super) fn new() -> Self {
        Self {
            low: vec![0; LOW as usize],
            high: vec![0; 0x1000],
            host: Vec::new(),
            accesses: Vec::new(),
        }
    }

    fn slot(&mut self, address: u32) -> Result<&mut [u8], Abort> {
        let page = address & !0xFFF;
        if page == UNMAPPED {
            return Err(Abort::Translation);
        }
        if address < LOW {
            Ok(&mut self.low[address as usize..])
        } else if address >= HIGH && address - HIGH < 0x1000 {
            Ok(&mut self.high[(address - HIGH) as usize..])
        } else {
            Err(Abort::Translation)
        }
    }

    pub(super) fn word(&mut self, address: u32) -> u32 {
        self.read(address, Width::Word, true).expect("mapped")
    }

    pub(super) fn put(&mut self, address: u32, word: u32) {
        self.slot(address).expect("mapped")[..4].copy_from_slice(&word.to_le_bytes());
    }

    pub(super) fn put_half(&mut self, address: u32, half: u16) {
        self.slot(address).expect("mapped")[..2].copy_from_slice(&half.to_le_bytes());
    }

    /// Writes the Thumb `program` at `address`, each instruction checked
    /// against the text the disassembler gives it where it stands in the
    /// straight-line code's `IT` blocks.
    pub(super) fn thumb(&mut self, address: u32, program: &[(&[u16], &str)]) {
        let mut at = address;
        let mut itstate = 0u8;
        for &(halves, text) in program {
            let second = halves.get(1).copied().unwrap_or(0);
            let decoded = t32::decode(halves[0], second, BIT32.isa, Position::of(itstate))
                .unwrap_or_else(|| panic!("{halves:04X?} decodes"));
            assert_eq!(decoded.wide, halves.len() == 2, "{halves:04X?}");
            let shown = format!("{}", decoded.instruction.text(Set::T32, at));
            assert_eq!(shown, text, "{halves:04X?}");
            itstate = match decoded.instruction.op {
                Op::IfThen { firstcond, mask } => (firstcond.bits().to_le_bytes()[0] << 4) | mask,
                _ if itstate.trailing_zeros() >= 3 => 0,
                _ => (itstate & 0xE0) | ((itstate << 1) & 0x1F),
            };
            for &half in halves {
                self.put_half(at, half);
                at += 2;
            }
        }
    }

    /// Writes `program` at `address`, each word checked against the text
    /// the disassembler gives it so a test cannot run a word it misread.
    pub(super) fn program(&mut self, profile: &Profile, address: u32, program: &[(u32, &str)]) {
        for (n, &(word, text)) in (0u32..).zip(program) {
            let at = address + 4 * n;
            let shown = decode(word, profile.isa)
                .map(|instruction| format!("{}", instruction.text(Set::A32, at)));
            assert_eq!(shown.as_deref(), Some(text), "{word:08X}");
            self.put(at, word);
        }
    }
}

impl Bus for Ram {
    fn fetch(&mut self, address: u32, _privileged: bool) -> Result<Fetch, Abort> {
        let host = self.host.contains(&(address & !0xFFF));
        let slot = self.slot(address)?;
        let word = u32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]);
        Ok(Fetch { word, host })
    }

    fn read(&mut self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort> {
        self.accesses.push((address, width, false, privileged));
        let slot = self.slot(address)?;
        Ok(match width {
            Width::Byte => u32::from(slot[0]),
            Width::Half => u32::from(u16::from_le_bytes([slot[0], slot[1]])),
            Width::Word => u32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]),
        })
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        self.accesses.push((address, width, true, privileged));
        if address & !0xFFF == READ_ONLY && !privileged {
            return Err(Abort::Permission);
        }
        let slot = self.slot(address)?;
        let bytes = value.to_le_bytes();
        let n = width.bytes() as usize;
        slot[..n].copy_from_slice(&bytes[..n]);
        Ok(())
    }
}

pub(super) fn r(n: u32) -> Reg {
    Reg::new(n).expect("a register")
}

/// A processor of `profile` in `mode` at `pc`, flags clear, interrupts
/// enabled.
pub(super) fn processor(profile: &'static Profile, mode: Mode, pc: u32) -> Cpu {
    let mut cpu = Cpu::new(profile, 4);
    cpu.set_cpsr(mode.bits()).expect("a mode the profile has");
    cpu.set_reg(Reg::PC, pc);
    cpu
}

pub(super) fn run(cpu: &mut Cpu, ram: &mut Ram, instructions: u64) -> Run {
    cpu.run(ram, instructions)
}

#[test]
fn data_processing_sets_the_flags_and_the_shifter_carry() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE3E0_0000, "MVN     R0,#0"),
            (0xE290_1001, "ADDS    R1,R0,#1"),
            (0xE1B0_2080, "MOVS    R2,R0,LSL #1"),
            (0xE3A0_3102, "MOV     R3,#&80000000"),
            (0xE253_4001, "SUBS    R4,R3,#1"),
            (0xE1B0_5063, "MOVS    R5,R3,RRX"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.reg(r(1)), 0);
    assert_eq!(
        cpu.cpsr() & psr::NZCV,
        psr::Z | psr::C,
        "carry out, no overflow"
    );
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(2)), 0xFFFF_FFFE);
    assert_eq!(
        cpu.cpsr() & psr::NZCV,
        psr::N | psr::C,
        "the bit shifted out"
    );
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.reg(r(4)), 0x7FFF_FFFF);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::C | psr::V, "signed overflow");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(5)), 0xC000_0000, "the carry rotated in");
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::N | psr::V, "bit 0 out, V kept");
}

#[test]
fn a_register_shifted_operand_shifts_by_the_bottom_byte() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE3A0_0001, "MOV     R0,#1"),
            (0xE3A0_1F42, "MOV     R1,#&108"),
            (0xE1B0_2110, "MOVS    R2,R0,LSL R1"),
            (0xE3A0_1020, "MOV     R1,#&20"),
            (0xE1B0_3110, "MOVS    R3,R0,LSL R1"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 3);
    assert_eq!(cpu.reg(r(2)), 0x100, "shifted by 8, the byte of &108");
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.reg(r(3)), 0);
    assert_ne!(cpu.cpsr() & psr::C, 0, "LSL by 32 carries bit 0");
}

#[test]
fn a_failed_condition_does_nothing_but_count() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0x03A0_0001, "MOVEQ   R0,#1"),
            (0x13A0_1001, "MOVNE   R1,#1"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    let ran = run(&mut cpu, &mut ram, 2);
    assert_eq!(ran.executed, 2);
    assert_eq!((cpu.reg(r(0)), cpu.reg(r(1))), (0, 1));
}

#[test]
fn the_26_bit_r15_carries_the_psr_where_the_architecture_shows_it() {
    let mut ram = Ram::new();
    ram.program(
        &BIT26,
        0x8000,
        &[
            (0xE1A0_000F, "MOV     R0,PC"),
            (0xE08F_1000, "ADD     R1,PC,R0"),
            (0xE58F_F000, "STR     PC,[PC]"),
            (0xEB00_0000, "BL      &00008014"),
        ],
    );
    let mut cpu = processor(&BIT26, Mode::Supervisor26, 0x8000);
    cpu.set_cpsr(Mode::Supervisor26.bits() | psr::C | psr::I)
        .expect("SVC26");
    run(&mut cpu, &mut ram, 1);
    let psr = psr::C | psr::R15_I | 0b11;
    assert_eq!(
        cpu.reg(r(0)),
        0x8008 | psr,
        "as the second operand, with the PSR"
    );
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        cpu.reg(r(1)),
        0x800C + (0x8008 | psr),
        "as the first, the PC alone"
    );
    run(&mut cpu, &mut ram, 1);
    assert_eq!(ram.word(0x8010), 0x8010 | psr, "stored with the PSR, PC+8");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(Reg::LR), 0x8010 | psr, "BL keeps the PSR in R14");
    assert_eq!(cpu.pc(), 0x8014);
}

#[test]
fn teqp_and_movs_pc_write_the_26_bit_psr() {
    let mut ram = Ram::new();
    ram.program(
        &BIT26,
        0x8000,
        &[
            (0xE33F_F000, "TEQP    PC,#0"),
            (0xE33F_F003, "TEQP    PC,#3"),
            (0xE1B0_F00E, "MOVS    PC,R14"),
        ],
    );
    let mut cpu = processor(&BIT26, Mode::Supervisor26, 0x8000);
    cpu.set_reg(r(13), 0x5000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::User26, "TEQP PC,#0 drops to USR26");
    assert_ne!(cpu.reg(r(13)), 0x5000, "and to its bank");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::User26, "USR26 writes only the flags");
    cpu.set_cpsr(Mode::Supervisor26.bits()).expect("SVC26");
    cpu.set_reg(Reg::LR, 0x9000 | psr::N | psr::R15_F);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x9000);
    assert_eq!(cpu.mode(), Mode::User26);
    assert_eq!(cpu.cpsr() & (psr::NZCV | psr::I | psr::F), psr::N | psr::F);
}

#[test]
fn ldm_with_the_s_bit_and_r15_restores_the_26_bit_psr() {
    let mut ram = Ram::new();
    ram.program(&BIT26, 0x8000, &[(0xE8FD_8000, "LDMFD   R13!,{PC}^")]);
    ram.put(0x4000, 0x0000_A000 | psr::Z);
    let mut cpu = processor(&BIT26, Mode::Supervisor26, 0x8000);
    cpu.set_reg(r(13), 0x4000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0xA000);
    assert_eq!(cpu.mode(), Mode::User26);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::Z);
    let mut supervisor = cpu.state();
    supervisor.cpsr = Mode::Supervisor26.bits();
    assert_eq!(supervisor.banked[1][0], 0x4004, "SVC26's R13 written back");
}

/// The flat memory, seen again every 32 KiB.
struct Mirrored<'a>(&'a mut Ram);

impl Bus for Mirrored<'_> {
    fn fetch(&mut self, address: u32, privileged: bool) -> Result<Fetch, Abort> {
        self.0.fetch(address & 0x7FFF, privileged)
    }

    fn read(&mut self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort> {
        self.0.read(address & 0x7FFF, width, privileged)
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        self.0.write(address & 0x7FFF, width, value, privileged)
    }
}

#[test]
fn the_26_bit_pc_wraps_within_64_mib() {
    let mut ram = Ram::new();
    ram.program(&BIT26, 0x7FFC, &[(0xE1A0_0000, "MOV     R0,R0")]);
    let mut cpu = processor(&BIT26, Mode::User26, 0x03FF_FFFC);
    cpu.run(&mut Mirrored(&mut ram), 1);
    assert_eq!(cpu.pc(), 0, "past the top of the 26-bit space");
}

#[test]
fn block_transfers_write_back_in_every_mode() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE8A0_0006, "STMIA   R0!,{R1,R2}"),
            (0xE9A0_0006, "STMIB   R0!,{R1,R2}"),
            (0xE820_0006, "STMDA   R0!,{R1,R2}"),
            (0xE920_0006, "STMDB   R0!,{R1,R2}"),
            (0xE890_0018, "LDMIA   R0,{R3,R4}"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(0), 0x4000);
    cpu.set_reg(r(1), 0x11);
    cpu.set_reg(r(2), 0x22);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x4008);
    assert_eq!((ram.word(0x4000), ram.word(0x4004)), (0x11, 0x22));
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x4010);
    assert_eq!((ram.word(0x400C), ram.word(0x4010)), (0x11, 0x22));
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x4008);
    assert_eq!((ram.word(0x400C), ram.word(0x4010)), (0x11, 0x22));
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x4000);
    assert_eq!((ram.word(0x4000), ram.word(0x4004)), (0x11, 0x22));
    run(&mut cpu, &mut ram, 1);
    assert_eq!((cpu.reg(r(3)), cpu.reg(r(4))), (0x11, 0x22));
}

#[test]
fn the_s_bit_transfers_the_user_bank_from_fiq_mode() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE8C0_6100, "STMIA   R0,{R8,R13,R14}^"),
            (0xE8D0_0700, "LDMIA   R0,{R8-R10}^"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(8), 0x88);
    cpu.set_reg(r(13), 0xDD);
    cpu.set_reg(r(14), 0xEE);
    cpu.set_cpsr(Mode::Fiq.bits()).expect("FIQ");
    cpu.set_reg(r(0), 0x4000);
    cpu.set_reg(r(8), 0xF8);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        (ram.word(0x4000), ram.word(0x4004), ram.word(0x4008)),
        (0x88, 0xDD, 0xEE),
        "User's registers, not FIQ's"
    );
    ram.put(0x4000, 0x123);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(8)), 0xF8, "FIQ's R8 untouched");
    assert_eq!(cpu.state().user[8], 0x123, "User's loaded");
}

#[test]
fn an_exception_return_by_ldm_restores_the_cpsr() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xE8FD_8001, "LDMFD   R13!,{R0,PC}^")]);
    ram.put(0x4000, 7);
    ram.put(0x4004, 0xA000);
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.enter(Exception::Swi, 0x8000);
    let mut state = cpu.state();
    state.spsr[2] = Mode::User.bits() | psr::C;
    state.banked[1][0] = 0x4000;
    state.pc = 0x8000;
    cpu.set_state(&state).expect("SVC");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 7);
    assert_eq!(cpu.pc(), 0xA000);
    assert_eq!(cpu.mode(), Mode::User);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::C);
}

#[test]
fn branches_and_loads_to_the_pc_interwork_as_the_architecture_version_does() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE12F_FF10, "BX      R0"),
            (0xE591_F000, "LDR     PC,[R1]"),
            (0xE1A0_F002, "MOV     PC,R2"),
        ],
    );
    ram.thumb(0x9000, &[(&[0x2105], "MOVS    R1,#5")]);
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(0), 0x9001);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x9000);
    assert_ne!(cpu.cpsr() & psr::T, 0, "bit 0 selects Thumb");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(1)), 5, "the Thumb instruction ran");
    assert_eq!(cpu.pc(), 0x9002);
    for (word, target) in [(0x8004u32, 0x9003u32), (0x8008, 0x9005)] {
        let mut cpu = processor(&BIT32, Mode::User, word);
        cpu.set_reg(r(1), 0x4000);
        cpu.set_reg(r(2), target);
        ram.put(0x4000, target);
        run(&mut cpu, &mut ram, 1);
        assert_eq!(cpu.pc(), target & !1);
        assert_ne!(cpu.cpsr() & psr::T, 0, "{word:X}: ARMv7 interworks");
    }
}

#[test]
fn armv4_loads_to_the_pc_do_not_interwork() {
    let mut ram = Ram::new();
    ram.program(&BIT26, 0x8000, &[(0xE591_F000, "LDR     PC,[R1]")]);
    ram.put(0x4000, 0x9003);
    let mut cpu = processor(&BIT26, Mode::User, 0x8000);
    cpu.set_reg(r(1), 0x4000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x9000);
    assert_eq!(cpu.cpsr() & psr::T, 0);
}

#[test]
fn a_misaligned_load_rotates_on_armv4_and_faults_on_armv7() {
    let mut ram = Ram::new();
    ram.put(0x4000, 0x4433_2211);
    ram.program(&BIT26, 0x8000, &[(0xE591_0001, "LDR     R0,[R1,#1]")]);
    let mut legacy = processor(&BIT26, Mode::User, 0x8000);
    legacy.set_reg(r(1), 0x4000);
    run(&mut legacy, &mut ram, 1);
    assert_eq!(legacy.reg(r(0)), 0x1144_3322);
    let mut modern = processor(&BIT32, Mode::User, 0x8000);
    modern.set_reg(r(1), 0x4000);
    run(&mut modern, &mut ram, 1);
    assert_eq!(modern.mode(), Mode::Abort);
    assert_eq!(modern.pc(), 0xFFFF_0010);
    assert_eq!(modern.reg(Reg::LR), 0x8008);
    assert_eq!(modern.reg(r(0)), 0, "nothing loaded");
}

#[test]
fn an_aborted_transfer_restores_its_base() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE8B0_0006, "LDMIA   R0!,{R1,R2}"),
            (0xE4A1_3004, "STRT    R3,[R1],#4"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(0), UNMAPPED - 4);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Abort);
    let state = cpu.state();
    assert_eq!(state.user[0], UNMAPPED - 4, "the base as it was");
    assert_eq!(state.user[1], 0, "the first register not loaded");
    let mut unprivileged = processor(&BIT32, Mode::Supervisor, 0x8004);
    unprivileged.set_reg(r(1), READ_ONLY);
    run(&mut unprivileged, &mut ram, 1);
    assert_eq!(unprivileged.mode(), Mode::Abort, "STRT is a User access");
    assert_eq!(
        ram.accesses.last(),
        Some(&(READ_ONLY, Width::Word, true, false))
    );
}

#[test]
fn halfwords_signed_bytes_and_doublewords_transfer() {
    let mut ram = Ram::new();
    ram.put(0x4000, 0x8081_F0FF);
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE1D0_20B2, "LDRH    R2,[R0,#2]"),
            (0xE1D0_30D0, "LDRSB   R3,[R0]"),
            (0xE1D0_40F2, "LDRSH   R4,[R0,#2]"),
            (0xE1C0_20F8, "STRD    R2,R3,[R0,#8]"),
            (0xE1E0_60D8, "LDRD    R6,R7,[R0,#8]!"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(0), 0x4000);
    run(&mut cpu, &mut ram, 3);
    assert_eq!(cpu.reg(r(2)), 0x8081);
    assert_eq!(cpu.reg(r(3)), 0xFFFF_FFFF);
    assert_eq!(cpu.reg(r(4)), 0xFFFF_8081);
    run(&mut cpu, &mut ram, 2);
    assert_eq!((cpu.reg(r(6)), cpu.reg(r(7))), (0x8081, 0xFFFF_FFFF));
    assert_eq!(cpu.reg(r(0)), 0x4008);
}

#[test]
fn the_multiplies_compute_and_saturate() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE081_0392, "UMULL   R0,R1,R2,R3"),
            (0xE0E5_4392, "SMLAL   R4,R5,R2,R3"),
            (0xE106_3382, "SMLABB  R6,R2,R3,R3"),
            (0xE710_F312, "SDIV    R0,R2,R3"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(2), 0xFFFF_FFFF);
    cpu.set_reg(r(3), 2);
    cpu.set_reg(r(4), 1);
    run(&mut cpu, &mut ram, 2);
    assert_eq!((cpu.reg(r(0)), cpu.reg(r(1))), (0xFFFF_FFFE, 1));
    assert_eq!(
        (cpu.reg(r(4)), cpu.reg(r(5))),
        (0xFFFF_FFFF, 0xFFFF_FFFF),
        "1 + -2"
    );
    cpu.set_reg(r(2), 0x0000_8000);
    cpu.set_reg(r(3), 0x7FFF_8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(6)), 0x7FFF_8000u32.wrapping_add(0x4000_0000));
    assert_ne!(cpu.cpsr() & psr::Q, 0, "the accumulation overflowed");
    cpu.set_reg(r(3), 0);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0, "division by zero gives zero");
}

#[test]
fn the_most_significant_word_multiplies_wrap_as_the_architecture_does() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE750_3332, "SMMLAR  R0,R2,R3,R3"),
            (0xE751_33F2, "SMMLSR  R1,R2,R3,R3"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(2), 0x8000_0000);
    cpu.set_reg(r(3), 0x8000_0000);
    run(&mut cpu, &mut ram, 2);
    // (-2^31 << 32) + 2^62 + 2^31, and (-2^31 << 32) - 2^62 + 2^31, kept to
    // 64 bits.
    assert_eq!(cpu.reg(r(0)), 0xC000_0000);
    assert_eq!(cpu.reg(r(1)), 0x4000_0000);
}

#[test]
fn the_media_instructions_compute() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE6AF_1072, "SXTB    R1,R2"),
            (0xE6BF_3F32, "REV     R3,R2"),
            (0xE7C7_4192, "BFI     R4,R2,#3,#5"),
            (0xE7E3_5252, "UBFX    R5,R2,#4,#4"),
            (0xE6AF_6012, "SSAT    R6,#16,R2"),
            (0xE16F_7F12, "CLZ     R7,R2"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(2), 0x0012_34F6);
    cpu.set_reg(r(4), 0xFFFF_FFFF);
    run(&mut cpu, &mut ram, 6);
    assert_eq!(cpu.reg(r(1)), 0xFFFF_FFF6);
    assert_eq!(cpu.reg(r(3)), 0xF634_1200);
    assert_eq!(
        cpu.reg(r(4)),
        0xFFFF_FFB7,
        "the low five bits of &F6 at bit 3"
    );
    assert_eq!(cpu.reg(r(5)), 0xF);
    assert_eq!(cpu.reg(r(6)), 0x7FFF);
    assert_ne!(cpu.cpsr() & psr::Q, 0);
    assert_eq!(cpu.reg(r(7)), 11);
}

#[test]
fn a_swi_stops_past_itself_and_the_vector_can_be_entered() {
    let mut ram = Ram::new();
    ram.program(&BIT26, 0x8000, &[(0xEF00_0011, "SWI     &000011")]);
    let mut cpu = processor(&BIT26, Mode::User26, 0x8000);
    cpu.set_cpsr(Mode::User26.bits() | psr::V).expect("USR26");
    let ran = run(&mut cpu, &mut ram, 10);
    assert_eq!(ran.stop, Stop::Swi { number: 0x11 });
    assert_eq!(ran.executed, 1);
    assert_eq!(cpu.pc(), 0x8004);
    cpu.enter(Exception::Swi, cpu.pc());
    assert_eq!(
        cpu.mode(),
        Mode::Supervisor,
        "the SA-110 enters 32-bit modes"
    );
    assert_eq!(cpu.pc(), 0x08);
    assert_eq!(cpu.reg(Reg::LR), 0x8004);
    assert_eq!(cpu.spsr(), Some(Mode::User26.bits() | psr::V));
    assert_ne!(cpu.cpsr() & psr::I, 0);
}

#[test]
fn an_undefined_instruction_enters_the_undefined_vector() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xE7F0_00F0, "UDF     #0")]);
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Undefined);
    assert_eq!(cpu.pc(), 0xFFFF_0004);
    assert_eq!(cpu.reg(Reg::LR), 0x8004);
}

#[test]
fn a_udf_on_a_host_page_calls_the_host() {
    let mut ram = Ram::new();
    ram.host.push(0x9000);
    ram.program(&BIT32, 0x9000, &[(0xE7F1_23F4, "UDF     #&1234")]);
    let mut cpu = processor(&BIT32, Mode::User, 0x9000);
    let ran = run(&mut cpu, &mut ram, 5);
    assert_eq!(ran.stop, Stop::HostCall { number: 0x1234 });
    assert_eq!(cpu.pc(), 0x9004);
    assert_eq!(cpu.mode(), Mode::User);
}

#[test]
fn interrupts_are_taken_between_instructions_unless_masked() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE1A0_0000, "MOV     R0,R0"),
            (0xE1A0_0000, "MOV     R0,R0"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 1);
    cpu.set_irq(true);
    cpu.set_fiq(true);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Fiq, "FIQ first");
    assert_eq!(cpu.reg(Reg::LR), 0x8008, "the next instruction plus 4");
    assert_eq!(
        cpu.pc(),
        0xFFFF_0020,
        "the FIQ vector's first instruction ran"
    );
    assert_ne!(cpu.cpsr() & (psr::I | psr::F), 0);
    cpu.set_fiq(false);
    let masked = cpu.pc();
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Fiq, "IRQ masked");
    assert_eq!(cpu.pc(), masked + 4);
}

#[test]
fn wfi_waits_until_a_line_is_asserted() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xE320_F003, "WFI")]);
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x8000);
    cpu.set_cpsr(Mode::Supervisor.bits() | psr::I).expect("SVC");
    assert_eq!(run(&mut cpu, &mut ram, 1).stop, Stop::WaitForInterrupt);
    assert_eq!(cpu.pc(), 0x8004);
    cpu.set_reg(Reg::PC, 0x8000);
    cpu.set_irq(true);
    assert_eq!(
        run(&mut cpu, &mut ram, 1).stop,
        Stop::Budget,
        "a masked line still wakes it"
    );
}

#[test]
fn a_mode_change_by_msr_banks_the_registers() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE321_F011, "MSR     CPSR_c,#&11"),
            (0xE3A0_80FF, "MOV     R8,#&FF"),
            (0xE321_F013, "MSR     CPSR_c,#&13"),
            (0xE321_F010, "MSR     CPSR_c,#&10"),
            (0xE321_F013, "MSR     CPSR_c,#&13"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x8000);
    cpu.set_reg(r(8), 0x88);
    run(&mut cpu, &mut ram, 3);
    assert_eq!(cpu.mode(), Mode::Supervisor);
    assert_eq!(cpu.reg(r(8)), 0x88, "FIQ's R8 put away");
    assert_eq!(cpu.state().fiq[0], 0xFF);
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.mode(), Mode::User, "User mode cannot change mode");
}

#[test]
fn the_system_coprocessor_answers_privileged_code_and_names_what_it_refuses() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEE10_0F10, "MRC     CP15,0,R0,C0,C0,0"),
            (0xEE02_0F10, "MCR     CP15,0,R0,C2,C0,0"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x000F_0000);
    let ran = run(&mut cpu, &mut ram, 1);
    assert_eq!(
        ran.stop,
        Stop::Unmodelled(Unmodelled::System {
            register: [0, 2, 0, 0],
            write: true
        })
    );
    assert_eq!(ran.executed, 0);
    assert_eq!(cpu.pc(), 0x8004, "the instruction did not execute");
    let mut user = processor(&BIT32, Mode::User, 0x8000);
    run(&mut user, &mut ram, 1);
    assert_eq!(user.mode(), Mode::Undefined);
}

#[test]
fn an_exclusive_store_succeeds_only_while_its_load_s_mark_stands() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE190_1F9F, "LDREX   R1,[R0]"),
            (0xE180_2F93, "STREX   R2,R3,[R0]"),
            (0xE180_2F93, "STREX   R2,R3,[R0]"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(0), 0x4000);
    cpu.set_reg(r(3), 0x99);
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.reg(r(2)), 0, "stored");
    assert_eq!(ram.word(0x4000), 0x99);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(2)), 1, "the mark is spent");
}

#[test]
fn rewritten_code_runs_as_rewritten() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xE3A0_0001, "MOV     R0,#1")]);
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 1);
    ram.put(0x8000, 0xE3A0_0002);
    cpu.set_reg(Reg::PC, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 2);
    assert!(cpu.memo_bytes() > 0);
    cpu.release_memo();
    assert_eq!(cpu.memo_bytes(), 0);
}

#[test]
fn an_unpredictable_encoding_is_refused_on_the_32_bit_profile_and_runs_on_the_26_bit() {
    let mut ram = Ram::new();
    ram.put(0x4004, 0x55);
    ram.program(&BIT26, 0x8000, &[(0xE5B1_1004, "LDR     R1,[R1,#4]!")]);
    let mut legacy = processor(&BIT26, Mode::User, 0x8000);
    legacy.set_reg(r(1), 0x4000);
    run(&mut legacy, &mut ram, 1);
    assert_eq!(legacy.reg(r(1)), 0x55, "the load wins");
    let mut modern = processor(&BIT32, Mode::User, 0x8000);
    modern.set_reg(r(1), 0x4000);
    run(&mut modern, &mut ram, 1);
    assert_eq!(modern.mode(), Mode::Undefined);
}

#[test]
fn the_state_round_trips_with_every_bank() {
    let mut cpu = Cpu::new(&BIT32, 0);
    let mut state = State {
        cpsr: Mode::Irq.bits() | psr::Z,
        pc: 0x1234,
        ..State::default()
    };
    for (n, value) in state.user.iter_mut().enumerate() {
        *value = 0x100 + u32::try_from(n).expect("small");
    }
    state.fiq = [1, 2, 3, 4, 5, 6, 7];
    state.banked = [[10, 11], [12, 13], [14, 15], [16, 17]];
    state.spsr = [20, 21, 22, 23, 24];
    cpu.set_state(&state).expect("valid");
    assert_eq!(cpu.state(), state);
    assert_eq!(cpu.reg(r(13)), 10, "IRQ's R13");
    assert_eq!(cpu.spsr(), Some(21));
    let mut bad = state;
    bad.cpsr = Mode::User26.bits();
    assert_eq!(
        cpu.set_state(&bad),
        Err(BadState::Mode),
        "no 26-bit modes here"
    );
}

#[test]
fn a_reset_leaves_the_interrupt_lines_as_their_devices_hold_them() {
    let mut ram = Ram::new();
    let mut cpu = Cpu::new(&BIT32, 0);
    cpu.set_irq(true);
    cpu.reset();
    cpu.set_cpsr(Mode::Supervisor.bits()).expect("SVC");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Irq, "the line still asserted is taken");
}

#[test]
fn reset_enters_supervisor_mode_at_the_reset_vector() {
    let cpu = Cpu::new(&BIT32, 0);
    assert_eq!(cpu.mode(), Mode::Supervisor);
    assert_eq!(cpu.pc(), 0xFFFF_0000);
    assert_eq!(
        cpu.cpsr() & (psr::I | psr::F | psr::A),
        psr::I | psr::F | psr::A
    );
    let legacy = Cpu::new(&BIT26, 0);
    assert_eq!(legacy.pc(), 0);
}

/// A processor of the 32-bit profile in Thumb state.
fn thumb(mode: Mode, pc: u32) -> Cpu {
    let mut cpu = processor(&BIT32, mode, pc);
    cpu.set_cpsr(mode.bits() | psr::T).expect("Thumb state");
    cpu
}

#[test]
fn thumb_data_processing_sets_the_flags_outside_a_block() {
    let mut ram = Ram::new();
    ram.thumb(
        0x8000,
        &[
            (&[0x2005], "MOVS    R0,#5"),
            (&[0x1D41], "ADDS    R1,R0,#5"),
            (&[0x1A0A], "SUBS    R2,R1,R0"),
            (&[0x4240], "RSBS    R0,R0,#0"),
            (&[0x0049], "MOVS    R1,R1,LSL #1"),
            (&[0xF04F, 0x0307], "MOV     R3,#7"),
        ],
    );
    let mut cpu = thumb(Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 3);
    assert_eq!((cpu.reg(r(1)), cpu.reg(r(2))), (10, 5));
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0xFFFF_FFFB);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::N, "the negation borrowed");
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.reg(r(1)), 20);
    assert_eq!(cpu.reg(r(3)), 7);
    assert_eq!(cpu.cpsr() & psr::NZCV, 0, "MOV.W left the flags");
    assert_eq!(cpu.pc(), 0x800E);
}

#[test]
fn an_it_block_conditions_its_instructions_and_ends() {
    let mut ram = Ram::new();
    ram.thumb(
        0x8000,
        &[
            (&[0x2800], "CMP     R0,#0"),
            (&[0xBF0A], "ITET    EQ"),
            (&[0x2101], "MOVEQ   R1,#1"),
            (&[0x2202], "MOVNE   R2,#2"),
            (&[0x2303], "MOVEQ   R3,#3"),
            (&[0x2404], "MOVS    R4,#4"),
        ],
    );
    for (r0, expected) in [(0, [1, 0, 3]), (1, [0, 2, 0])] {
        let mut cpu = thumb(Mode::User, 0x8000);
        cpu.set_reg(r(0), r0);
        run(&mut cpu, &mut ram, 5);
        assert_eq!(
            [cpu.reg(r(1)), cpu.reg(r(2)), cpu.reg(r(3))],
            expected,
            "R0 = {r0}: a MOV in a block sets no flags, so the block reads one Z"
        );
        assert_eq!(
            cpu.cpsr() & (psr::IT_LOW | psr::IT_HIGH),
            0,
            "the block ended"
        );
        run(&mut cpu, &mut ram, 1);
        assert_eq!(cpu.reg(r(4)), 4);
        assert_eq!(
            cpu.cpsr() & psr::Z,
            0,
            "MOVS after the block sets the flags"
        );
    }
}

#[test]
fn compare_and_branch_and_the_table_branches_go_where_they_point() {
    let mut ram = Ram::new();
    ram.thumb(0x8000, &[(&[0xB108], "CBZ     R0,&00008006")]);
    for (r0, target) in [(0, 0x8006), (1, 0x8002)] {
        let mut cpu = thumb(Mode::User, 0x8000);
        cpu.set_reg(r(0), r0);
        run(&mut cpu, &mut ram, 1);
        assert_eq!(cpu.pc(), target, "R0 = {r0}");
    }
    ram.thumb(0x8100, &[(&[0xE8DF, 0xF001], "TBB     [PC,R1]")]);
    ram.put(0x8104, 0x0000_0302);
    for (index, target) in [(0, 0x8108), (1, 0x810A)] {
        let mut cpu = thumb(Mode::User, 0x8100);
        cpu.set_reg(r(1), index);
        run(&mut cpu, &mut ram, 1);
        assert_eq!(cpu.pc(), target, "TBB entry {index}");
    }
    ram.thumb(0x8200, &[(&[0xE8DF, 0xF011], "TBH     [PC,R1,LSL #1]")]);
    ram.put(0x8204, 0x0005_0001);
    let mut cpu = thumb(Mode::User, 0x8200);
    cpu.set_reg(r(1), 1);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x8204 + 2 * 5, "TBH's second halfword entry");
}

#[test]
fn thumb_interworks_with_arm_as_armv7_defines_it() {
    let mut ram = Ram::new();
    ram.thumb(
        0x9000,
        &[
            (&[0xF000, 0xE804], "BLX     &0000900C"),
            (&[0x2303], "MOVS    R3,#3"),
        ],
    );
    ram.program(
        &BIT32,
        0x900C,
        &[(0xE3A0_2007, "MOV     R2,#7"), (0xE12F_FF1E, "BX      R14")],
    );
    let mut cpu = thumb(Mode::User, 0x9000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x900C);
    assert_eq!(cpu.cpsr() & psr::T, 0, "BLX selects ARM");
    assert_eq!(cpu.reg(Reg::LR), 0x9005, "the return is Thumb code");
    run(&mut cpu, &mut ram, 3);
    assert_eq!((cpu.reg(r(2)), cpu.reg(r(3))), (7, 3));
    assert_ne!(cpu.cpsr() & psr::T, 0, "BX returned to Thumb");

    ram.thumb(
        0xA000,
        &[
            (&[0xBD00], "LDMFD   R13!,{PC}"),
            (&[0x4687], "MOV     PC,R0"),
            (&[0xF8D1, 0xF000], "LDR     PC,[R1]"),
        ],
    );
    ram.put(0x4000, 0x0000_A101);
    let mut cpu = thumb(Mode::User, 0xA000);
    cpu.set_reg(Reg::SP, 0x4000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0xA100);
    assert_ne!(
        cpu.cpsr() & psr::T,
        0,
        "POP {{PC}} interworks: bit 0 set stays Thumb"
    );
    let mut cpu = thumb(Mode::User, 0xA002);
    cpu.set_reg(r(0), 0xA200);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0xA200);
    assert_ne!(cpu.cpsr() & psr::T, 0, "MOV PC stays in Thumb state");
    let mut cpu = thumb(Mode::User, 0xA004);
    cpu.set_reg(r(1), 0x4000);
    ram.put(0x4000, 0x0000_A300);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0xA300);
    assert_eq!(cpu.cpsr() & psr::T, 0, "LDR PC interworks to ARM");
}

#[test]
fn literal_addressing_reads_the_pc_word_aligned() {
    let mut ram = Ram::new();
    ram.thumb(
        0x8002,
        &[
            (&[0x4801], "LDR     R0,[PC,#4]"),
            (&[0xA001], "ADD     R0,PC,#4"),
        ],
    );
    ram.put(0x8008, 0x1234_5678);
    let mut cpu = thumb(Mode::User, 0x8002);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x1234_5678, "from &8004 plus 4");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 0x800C, "ADR from &8008 plus 4");
}

#[test]
fn exceptions_from_thumb_return_to_it() {
    let mut ram = Ram::new();
    ram.thumb(
        0x9000,
        &[(&[0xDE00], "UDF     #0"), (&[0x2101], "MOVS    R1,#1")],
    );
    ram.program(&BIT32, HIGH + 4, &[(0xE1B0_F00E, "MOVS    PC,R14")]);
    let mut cpu = thumb(Mode::User, 0x9000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Undefined);
    assert_eq!(cpu.pc(), HIGH + 4);
    assert_eq!(cpu.reg(Reg::LR), 0x9002, "the undefined instruction plus 2");
    assert_eq!(cpu.cpsr() & psr::T, 0, "exceptions are taken in ARM state");
    assert_ne!(cpu.spsr().unwrap_or(0) & psr::T, 0);
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.mode(), Mode::User);
    assert_eq!(cpu.reg(r(1)), 1, "back in Thumb past the UDF");

    ram.thumb(0x9100, &[(&[0xBE00], "BKPT    #0")]);
    let mut cpu = thumb(Mode::User, 0x9100);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Abort);
    assert_eq!(cpu.reg(Reg::LR), 0x9104, "a breakpoint's return is plus 4");
}

#[test]
fn a_swi_in_a_block_leaves_the_block_advanced_for_its_handler() {
    let mut ram = Ram::new();
    ram.thumb(
        0x8000,
        &[
            (&[0x4280], "CMP     R0,R0"),
            (&[0xBF04], "ITT     EQ"),
            (&[0xDF01], "SWIEQ   &000001"),
            (&[0x2101], "MOVEQ   R1,#1"),
        ],
    );
    let mut cpu = thumb(Mode::User, 0x8000);
    let ran = run(&mut cpu, &mut ram, 10);
    assert_eq!(ran.stop, Stop::Swi { number: 1 });
    assert_eq!(cpu.pc(), 0x8006);
    cpu.enter(Exception::Swi, cpu.pc());
    let spsr = cpu.spsr().unwrap_or(0);
    assert_eq!(cpu.reg(Reg::LR), 0x8006);
    assert_ne!(spsr & psr::T, 0);
    assert_eq!(
        (spsr >> 25) & 3 | ((spsr >> 10) & 0x3F) << 2,
        0x08,
        "the SPSR holds the block's last instruction to come"
    );
}

#[test]
fn a_32_bit_instruction_may_straddle_a_word_and_its_second_halfword_may_abort() {
    let mut ram = Ram::new();
    ram.thumb(0x8002, &[(&[0xF04F, 0x0007], "MOV     R0,#7")]);
    let mut cpu = thumb(Mode::User, 0x8002);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 7);
    assert_eq!(cpu.pc(), 0x8006);
    ram.put_half(UNMAPPED - 2, 0xF04F);
    let mut cpu = thumb(Mode::User, UNMAPPED - 2);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Abort);
    assert_eq!(cpu.pc(), HIGH + 0x0C);
    assert_eq!(
        cpu.reg(Reg::LR),
        UNMAPPED + 2,
        "the instruction's address plus 4"
    );
}

#[test]
fn the_unconditional_space_executes_on_the_32_bit_profile() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xF10C_0080, "CPSID   I"),
            (0xF101_0200, "SETEND  BE"),
            (0xF101_0000, "SETEND  LE"),
            (0xFA00_0000, "BLX     &00008014"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_ne!(cpu.cpsr() & psr::I, 0, "CPSID I masked IRQs");
    run(&mut cpu, &mut ram, 1);
    assert_ne!(cpu.cpsr() & psr::E, 0);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.cpsr() & psr::E, 0);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x8014);
    assert_ne!(cpu.cpsr() & psr::T, 0, "BLX selects Thumb");
    assert_eq!(cpu.reg(Reg::LR), 0x8010);
}

#[test]
fn thumb_code_decodes_again_where_it_or_its_block_changes() {
    let mut ram = Ram::new();
    ram.thumb(0x8000, &[(&[0x2001], "MOVS    R0,#1")]);
    let mut cpu = thumb(Mode::User, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 1);
    ram.put_half(0x8000, 0x2002);
    cpu.set_reg(Reg::PC, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(0)), 2, "rewritten");
    ram.thumb(
        0x8100,
        &[(&[0x2800], "CMP     R0,#0"), (&[0xBF08], "IT      EQ")],
    );
    ram.put_half(0x8104, 0x2000);
    let mut cpu = thumb(Mode::User, 0x8104);
    cpu.set_reg(r(0), 5);
    run(&mut cpu, &mut ram, 1);
    assert_ne!(cpu.cpsr() & psr::Z, 0, "outside a block MOVS #0 sets Z");
    cpu.set_reg(r(0), 0);
    cpu.set_reg(Reg::PC, 0x8100);
    run(&mut cpu, &mut ram, 1);
    cpu.set_cpsr(cpu.cpsr() & !psr::Z).expect("Z clear");
    run(&mut cpu, &mut ram, 2);
    assert_eq!(
        cpu.reg(r(0)),
        0,
        "MOVEQ skipped under the block's condition"
    );
    assert_eq!(cpu.cpsr() & psr::Z, 0, "and set no flags");
}

#[test]
fn an_exception_return_restores_an_it_state_only_into_thumb_state() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xE16F_F000, "MSR     SPSR_fsxc,R0"),
            (0xE1B0_F00E, "MOVS    PC,R14"),
        ],
    );
    let user = Mode::User.bits();
    for (spsr, kept) in [
        (user | 0x0400_0000, 0, "an IT state returning to ARM state"),
        (
            user | psr::T | 0x0000_0800,
            0x0000_0800,
            "a block's last, in Thumb",
        ),
        (user | psr::T | 0x0000_1000, 0, "a reserved IT state"),
    ]
    .map(|(spsr, kept, _)| (spsr, kept))
    {
        let mut cpu = processor(&BIT32, Mode::Supervisor, 0x8000);
        cpu.set_reg(r(0), spsr);
        cpu.set_reg(Reg::LR, 0x9000);
        run(&mut cpu, &mut ram, 2);
        assert_eq!(cpu.mode(), Mode::User);
        assert_eq!(
            cpu.cpsr() & (psr::IT_LOW | psr::IT_HIGH),
            kept,
            "SPSR {spsr:08X}"
        );
    }
}

const ONE: [u32; 3] = [0x3FFF, 0x8000_0000, 0];
const TWO: [u32; 3] = [0x4000, 0x8000_0000, 0];
const THREE: [u32; 3] = [0x4000, 0xC000_0000, 0];

fn set_fpa(cpu: &mut Cpu, n: u8, words: [u32; 3]) {
    cpu.fpa_mut().set_register(n, Internal::from_words(words));
}

fn fpa_words(cpu: &Cpu, n: u8) -> [u32; 3] {
    cpu.fpa().register(n).words()
}

#[test]
fn fpa_transfers_address_memory_and_write_back_as_fpemulator_does() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEDB1_8102, "LDFD    F0,[R1,#8]!"),
            (0xEC21_0101, "STFS    F0,[R1],#-4"),
            (0xECD1_1103, "LDCL    CP1,C1,[R1],{3}"),
            (0xEDBF_2101, "LDFS    F2,[PC,#4]!"),
        ],
    );
    ram.put(0x4008, 0x3FF0_0000);
    ram.put(0x8018, 0x4000_0000);
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(1), 0x4000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(fpa_words(&cpu, 0), ONE, "a double, high word first");
    assert_eq!(cpu.reg(r(1)), 0x4008, "pre-indexed, written back");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(ram.word(0x4008), 0x3F80_0000, "stored at the base");
    assert_eq!(cpu.reg(r(1)), 0x4004, "then moved down a word");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        cpu.reg(r(1)),
        0x4004,
        "unindexed: at the base, no writeback"
    );
    assert_eq!(
        fpa_words(&cpu, 1),
        [0x4000_0000, 0x3F80_0000, 0],
        "an extended denormal, marked uncommon"
    );
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        fpa_words(&cpu, 2),
        TWO,
        "the PC as the instruction's address plus 8"
    );
    assert_eq!(cpu.pc(), 0x8010, "and never written back");
}

#[test]
fn lfm_and_sfm_move_the_internal_format_wrapping_past_f7() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEDC0_E200, "SFM     F6,3,[R0,#0]"),
            (0xEDD0_7203, "LFM     F7,2,[R0,#&C]"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(0), 0x4000);
    for (n, words) in [(6, ONE), (7, TWO), (0, THREE)] {
        set_fpa(&mut cpu, n, words);
    }
    run(&mut cpu, &mut ram, 1);
    let stored: Vec<u32> = (0..9).map(|n| ram.word(0x4000 + 4 * n)).collect();
    assert_eq!(stored, [ONE, TWO, THREE].concat(), "F6, F7, then F0");
    set_fpa(&mut cpu, 7, [0; 3]);
    set_fpa(&mut cpu, 0, [0; 3]);
    run(&mut cpu, &mut ram, 1);
    assert_eq!((fpa_words(&cpu, 7), fpa_words(&cpu, 0)), (TWO, THREE));
}

#[test]
fn a_trapped_store_writes_nothing_and_a_trapped_packed_load_writes_its_base_back() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEDA1_0101, "STFS    F0,[R1,#4]!"),
            (0xEDC1_A100, "STFP    F2,[R1,#0]"),
            (0xECF1_9103, "LDFP    F1,[R1],#&C"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(1), 0x4000);
    // A third, and a tenth: inexact as a single, and as packed decimal.
    set_fpa(&mut cpu, 0, [0x3FFD, 0xAAAA_AAAA, 0xAAAA_AAAB]);
    set_fpa(&mut cpu, 2, [0x3FFB, 0xCCCC_CCCC, 0xCCCC_CCCD]);
    let inexact = fpa::fpsr::SYSTEM_ID | fpa::Exception::Inexact.enable();
    cpu.fpa_mut().write_fpsr(inexact);
    let trap = Stop::FloatingPoint {
        exception: fpa::Exception::Inexact,
    };
    assert_eq!(run(&mut cpu, &mut ram, 1).stop, trap);
    assert_eq!(ram.word(0x4004), 0, "nothing stored");
    assert_eq!(cpu.reg(r(1)), 0x4000, "nor written back");
    assert_eq!(cpu.pc(), 0x8004, "the PC past the instruction");
    assert_eq!(cpu.fpa().fpsr(), inexact, "its flag left clear");
    cpu.fpa_mut().write_fpsr(fpa::fpsr::SYSTEM_ID);
    assert_eq!(run(&mut cpu, &mut ram, 1).stop, Stop::Budget);
    cpu.fpa_mut().write_fpsr(inexact);
    assert_eq!(run(&mut cpu, &mut ram, 1).stop, trap);
    assert_eq!(cpu.reg(r(1)), 0x400C, "the base written back first");
    assert_eq!(fpa_words(&cpu, 1), [0; 3], "the register not loaded");
}

#[test]
fn a_comparison_sets_the_flags_or_writes_them_to_its_register() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEE90_F111, "CMF     F0,F1"),
            (0xEE90_3111, "MRC     CP1,4,R3,C0,C1,0"),
            (0xEE90_F112, "CMF     F0,F2"),
            (0xEE90_F112, "CMF     F0,F2"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    set_fpa(&mut cpu, 0, ONE);
    set_fpa(&mut cpu, 1, TWO);
    set_fpa(&mut cpu, 2, [0x4000_7FFF, 0xC000_0000, 0]);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::N, "less than");
    cpu.set_cpsr(Mode::User.bits()).expect("User mode");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.reg(r(3)), psr::N, "the flags to R3");
    assert_eq!(cpu.cpsr() & psr::NZCV, 0, "and not to the PSR");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::V, "unordered");
    cpu.fpa_mut().write_fpsr(fpa::fpsr::AC);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        cpu.cpsr() & psr::NZCV,
        psr::C | psr::V,
        "unordered, C too with AC"
    );
}

#[test]
fn flt_and_wfs_read_r15_as_fpemulator_does() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[(0xEE00_F110, "FLTS    F0,PC"), (0xEE20_F110, "WFS     PC")],
    );
    let float = |value: u32| {
        let mut expected = fpa::Fpa::new();
        expected
            .float(fpa::Precision::Single, fpa::Rounding::Nearest, 0, value)
            .expect("no trap enabled");
        expected.register(0).words()
    };
    let mut user = processor(&BIT32, Mode::User, 0x8000);
    run(&mut user, &mut ram, 2);
    assert_eq!(
        fpa_words(&user, 0),
        float(0x8004),
        "User mode: the next instruction"
    );
    assert_eq!(
        user.fpa().fpsr(),
        fpa::fpsr::SYSTEM_ID | 0x8010,
        "its address plus 12"
    );
    let mut supervisor = processor(&BIT32, Mode::Supervisor, 0x8000);
    supervisor.set_reg(Reg::LR, 100);
    run(&mut supervisor, &mut ram, 1);
    assert_eq!(
        fpa_words(&supervisor, 0),
        float(100),
        "a privileged mode: R14"
    );
    // The 26-bit PSR rides in R15, its mode bits reaching the FPSR's flags.
    let mut legacy = processor(&BIT26, Mode::Supervisor26, 0x8000);
    legacy
        .set_cpsr(Mode::User26.bits() | psr::Z)
        .expect("26-bit User mode");
    run(&mut legacy, &mut ram, 1);
    assert_eq!(fpa_words(&legacy, 0), float(0x4000_8004));
    let mut legacy = processor(&BIT26, Mode::Supervisor26, 0x8004);
    run(&mut legacy, &mut ram, 1);
    assert_eq!(legacy.fpa().fpsr(), fpa::fpsr::SYSTEM_ID | 0x8010 | 3);
}

#[test]
fn system_mode_loses_what_the_fpa_writes_to_r13_and_r14() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEE30_D110, "RFS     R13"),
            (0xEE30_C110, "RFS     R12"),
            (0xECBD_0101, "LDFS    F0,[R13],#4"),
        ],
    );
    let fpsr = fpa::fpsr::SYSTEM_ID;
    let mut system = processor(&BIT32, Mode::System, 0x8000);
    system.set_reg(Reg::SP, 0x4000);
    run(&mut system, &mut ram, 3);
    assert_eq!(
        system.reg(Reg::SP),
        0x4000,
        "neither RFS nor writeback reaches R13"
    );
    assert_eq!(system.reg(r(12)), fpsr, "but R12 is written");
    let mut supervisor = processor(&BIT32, Mode::Supervisor, 0x8000);
    supervisor.set_reg(Reg::SP, 0x4000);
    run(&mut supervisor, &mut ram, 1);
    assert_eq!(
        supervisor.reg(Reg::SP),
        fpsr,
        "Supervisor mode's is written"
    );
    supervisor.set_reg(Reg::SP, 0x4000);
    supervisor.set_reg(Reg::PC, 0x8008);
    run(&mut supervisor, &mut ram, 1);
    assert_eq!(supervisor.reg(Reg::SP), 0x4004, "and written back");
}

#[test]
fn user_mode_fpa_transfers_move_each_word_without_privilege() {
    let mut ram = Ram::new();
    ram.program(&BIT26, 0x8000, &[(0xED91_8100, "LDFD    F0,[R1,#0]")]);
    ram.put(0x4000, 0xF000_003F);
    let mut user = processor(&BIT26, Mode::User26, 0x8000);
    user.set_reg(r(1), 0x4001);
    run(&mut user, &mut ram, 1);
    assert_eq!(
        ram.accesses,
        [
            (0x4000, Width::Word, false, false),
            (0x4004, Width::Word, false, false)
        ],
        "a word at a time, as LDRT reads"
    );
    assert_eq!(fpa_words(&user, 0), ONE, "each rotated as ARMv4 rotates");
    ram.accesses.clear();
    let mut supervisor = processor(&BIT26, Mode::Supervisor26, 0x8000);
    supervisor.set_reg(r(1), 0x4001);
    run(&mut supervisor, &mut ram, 1);
    assert_eq!(
        ram.accesses,
        [
            (0x4000, Width::Word, false, true),
            (0x4004, Width::Word, false, true)
        ],
        "a block, as LDM reads"
    );
    let mut unrotated = fpa::Fpa::new();
    unrotated
        .load(fpa::Format::Double, 0, [0xF000_003F, 0, 0, 0])
        .expect("a double load raises nothing");
    assert_eq!(fpa_words(&supervisor, 0), unrotated.register(0).words());
    let mut modern = processor(&BIT32, Mode::User, 0x8000);
    modern.set_reg(r(1), 0x4001);
    run(&mut modern, &mut ram, 1);
    assert_eq!(modern.mode(), Mode::Abort, "the 32-bit profile faults it");
}

#[test]
fn an_aborted_fpa_load_changes_nothing() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xEDF1_0101, "LDFE    F0,[R1,#4]!")]);
    let mut cpu = processor(&BIT32, Mode::User, 0x8000);
    cpu.set_reg(r(1), UNMAPPED - 12);
    set_fpa(&mut cpu, 0, ONE);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.mode(), Mode::Abort);
    assert_eq!(cpu.state().user[1], UNMAPPED - 12, "the base as it was");
    assert_eq!(fpa_words(&cpu, 0), ONE, "the register as it was");
}

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::*;
use crate::asm::{Asm, Cond, Reg16};
use crate::board::BoardConfig;
use crate::cpu::jit::cache::Cache;
use crate::cpu::jit::interpreted::{Compiler, Interpreted};
use crate::cpu::{gpr, CpuModel};
use crate::machine::{Machine, Template};
use crate::time::{Nanos, NANOS_PER_MILLISECOND as MS};

/// Where a program is loaded and runs from, as `0000:0400`.
const ORIGIN: u16 = 0x0400;
/// The timer interrupt's handler, and the word it counts in.
const HANDLER: u16 = 0x0500;
const TICKS: u16 = 0x0600;

/// A machine `config` describes, `program` loaded at [`ORIGIN`] and the CPU
/// about to run it, the timer interrupting it a thousand times a second.
fn machine(config: &BoardConfig, program: &[u8]) -> Machine {
    let mut m = Machine::new(config).unwrap();
    // INC WORD [TICKS]; end the interrupt at the 8259A; IRET.
    let [low, high] = TICKS.to_le_bytes();
    load(
        &mut m,
        u32::from(HANDLER),
        &[0xFF, 0x06, low, high, 0xB0, 0x20, 0xE6, 0x20, 0xCF],
    );
    let [low, high] = HANDLER.to_le_bytes();
    load(&mut m, 8 * 4, &[low, high, 0, 0]);
    load(&mut m, u32::from(ORIGIN), program);
    let regs = m.cpu_mut().regs_mut();
    regs.set_sreg(Seg::Cs, 0);
    regs.ip = ORIGIN;
    regs.set_sreg(Seg::Ss, 0);
    regs.gpr[gpr::SP] = 0x7C00;
    m
}

fn load(m: &mut Machine, at: u32, bytes: &[u8]) {
    for (addr, &byte) in (at..).zip(bytes) {
        m.board_mut().write(addr, byte, 0);
    }
}

/// The timer at 1193 counts, the 8259A as the BIOS sets it with only the
/// timer unmasked, and interrupts enabled.
fn interrupted(asm: &mut Asm) {
    asm.bytes(&[
        0xB0, 0x34, 0xE6, 0x43, 0xB0, 0xA9, 0xE6, 0x40, 0xB0, 0x04, 0xE6, 0x40,
    ]);
    asm.bytes(&[
        0xB0, 0x13, 0xE6, 0x20, 0xB0, 0x08, 0xE6, 0x21, 0xB0, 0x09, 0xE6, 0x21, 0xB0, 0xFE, 0xE6,
        0x21,
    ]);
    asm.sti();
}

/// Squares, sums and scatters through an array, calling a routine that
/// branches on what it reads, for ever.
fn compute() -> Vec<u8> {
    let mut asm = Asm::new(ORIGIN);
    interrupted(&mut asm);
    let (outer, inner, routine, skip) = (asm.label(), asm.label(), asm.label(), asm.label());
    asm.bind(&outer);
    asm.mov_imm16(Reg16::Cx, 0x0400);
    asm.zero(Reg16::Si);
    asm.mov_imm16(Reg16::Bx, 0x2000);
    asm.bind(&inner);
    asm.bytes(&[
        0x89, 0xF0, 0xF7, 0xE0, 0x01, 0xD0, 0x15, 0x34, 0x12, 0x30, 0x00,
    ]);
    asm.bytes(&[0xD1, 0xE0, 0x89, 0x80, 0x00, 0x01]);
    asm.call(&routine);
    asm.bytes(&[0x46, 0x81, 0xE6, 0xFF, 0x00]);
    asm.dec16(Reg16::Cx);
    asm.jcc(Cond::Ne, &inner);
    asm.jmp(&outer);
    asm.bind(&routine);
    asm.bytes(&[0x50, 0x8A, 0x00, 0x3C, 0x80]);
    asm.jcc(Cond::B, &skip);
    asm.bytes(&[0xF6, 0xD0]);
    asm.bind(&skip);
    asm.bytes(&[0x88, 0x80, 0x00, 0x03, 0x58]);
    asm.ret();
    asm.finish().unwrap()
}

/// A hot loop adding an immediate, which the code after it rewrites every
/// 256 times round: the loop is compiled, its rewriting drops it, and once
/// rewritten often enough it is left to the interpreter.
fn rewriting() -> Vec<u8> {
    let mut asm = Asm::new(ORIGIN);
    interrupted(&mut asm);
    let again = asm.label();
    asm.mov_imm16(Reg16::Cx, 0x0100);
    asm.bind(&again);
    let imm = asm.here() + 1;
    // MOV AX, imm; ADD [TICKS + 2], AX
    asm.bytes(&[0xB8, 0x00, 0x00, 0x01, 0x06]);
    asm.word(TICKS + 2);
    asm.dec16(Reg16::Cx);
    asm.jcc(Cond::Ne, &again);
    // INC WORD [imm]
    asm.bytes(&[0xFF, 0x06]);
    asm.word(imm);
    asm.mov_imm16(Reg16::Cx, 0x0100);
    asm.jmp(&again);
    asm.finish().unwrap()
}

/// The words the expanded-memory routines add into: page 0's, page 1's,
/// and the one the data window's byte is added to.
const SUMS: [u16; 3] = [TICKS + 4, TICKS + 8, TICKS + 12];

/// In expanded-memory pages 0 and 1, a routine of each's own adding the
/// byte at its offset `10h` to a word of `SUMS` and returning far; in pages
/// 2 and 3, a byte of data at offset 0. The frame's first window shows page
/// 0 and its second page 2.
fn banked(m: &mut Machine) {
    for page in 0..4u8 {
        m.board_mut().output(0x258, page, 0);
        m.board_mut().output(0x259, 0x80, 0);
        let mut bytes = Vec::new();
        if page < 2 {
            let [low, high] = SUMS[usize::from(page)].to_le_bytes();
            bytes.extend([
                0x2E, 0xA0, 0x10, 0x00, 0x30, 0xE4, 0x01, 0x06, low, high, 0xCB,
            ]);
            bytes.resize(0x10, 0x90);
            bytes.push(1 + page);
        } else {
            bytes.push(0x10 * page);
        }
        load(m, 0xD_0000, &bytes);
    }
    for (window, page) in [(0, 0), (1, 2)] {
        m.board_mut().output(0x258 + 2 * window, page, 0);
        m.board_mut().output(0x259 + 2 * window, 0x80, 0);
    }
}

/// Two hundred times, calls the routine in the frame's first window and
/// adds the byte its second shows to a word of `SUMS`; then shows the other
/// page in each, for ever.
fn calling_the_frame() -> Vec<u8> {
    let mut asm = Asm::new(ORIGIN);
    interrupted(&mut asm);
    // MOV AX, D400h; MOV ES, AX
    asm.bytes(&[0xB8, 0x00, 0xD4, 0x8E, 0xC0]);
    let (outer, inner) = (asm.label(), asm.label());
    asm.bind(&outer);
    asm.mov_imm16(Reg16::Cx, 200);
    asm.bind(&inner);
    asm.call_far(0xD000, 0x0000);
    // MOV AL, ES:[0]; XOR AH, AH; ADD [SUMS[2]], AX
    let [low, high] = SUMS[2].to_le_bytes();
    asm.bytes(&[0x26, 0xA0, 0x00, 0x00, 0x30, 0xE4, 0x01, 0x06, low, high]);
    asm.dec16(Reg16::Cx);
    asm.jcc(Cond::Ne, &inner);
    let [low, high] = (TICKS + 2).to_le_bytes();
    // MOV AL, [shown]; XOR AL, 1; MOV [shown], AL
    asm.bytes(&[0xA0, low, high, 0x34, 0x01, 0xA2, low, high]);
    // MOV DX, 258h; OUT DX, AL; MOV AL, 80h; INC DX; OUT DX, AL
    asm.bytes(&[0xBA, 0x58, 0x02, 0xEE, 0xB0, 0x80, 0x42, 0xEE]);
    // MOV AL, [shown]; ADD AL, 2; INC DX; OUT DX, AL; MOV AL, 80h; INC DX; OUT DX, AL
    asm.bytes(&[
        0xA0, low, high, 0x04, 0x02, 0x42, 0xEE, 0xB0, 0x80, 0x42, 0xEE,
    ]);
    asm.jmp(&outer);
    asm.finish().unwrap()
}

/// Timer 2 asking the 80186's first DMA channel for a byte every 200
/// clocks, memory to memory, then [`compute`]'s loop: the bus is owed for
/// the transfers between slices.
fn transferring() -> Vec<u8> {
    let mut asm = Asm::new(ORIGIN);
    // Each `MOV DX, port; MOV AX, value; OUT DX, AX`.
    for (port, value) in [
        (0xFF62u16, 50u16),
        (0xFF66, 0xC001),
        (0xFFC0, 0x3000),
        (0xFFC2, 0),
        (0xFFC4, 0x4000),
        (0xFFC6, 0),
        (0xFFC8, 0xFFFF),
        (0xFFCA, 0xB416),
    ] {
        asm.mov_imm16(Reg16::Dx, port);
        asm.mov_imm16(Reg16::Ax, value);
        asm.bytes(&[0xEF]);
    }
    let body = compute();
    // The compute loop's bytes past its own setup, run from here.
    asm.bytes(&body[setup_len()..]);
    asm.finish().unwrap()
}

/// The bytes [`interrupted`] lays down.
fn setup_len() -> usize {
    let mut asm = Asm::new(ORIGIN);
    interrupted(&mut asm);
    asm.finish().unwrap().len()
}

/// Two machines built alike, the second running its blocks on the IR
/// interpreter, its compiler served between slices.
struct Pair {
    plain: Machine,
    jit: Machine,
    compiler: Compiler,
}

impl Pair {
    fn new(config: &BoardConfig, program: &[u8]) -> Self {
        Self::with(config, program, |_| {})
    }

    /// The pair, each machine prepared by `prepare` before it runs.
    fn with(config: &BoardConfig, program: &[u8], prepare: impl Fn(&mut Machine)) -> Self {
        let mut plain = machine(config, program);
        let mut jit = machine(config, program);
        prepare(&mut plain);
        prepare(&mut jit);
        let (executor, compiler) = Interpreted::new();
        jit.attach(Jit::new(Cache::new(config.cpu), Box::new(executor)));
        Self {
            plain,
            jit,
            compiler,
        }
    }

    /// Runs both for `span` in slices of `slice`, compiling what was asked
    /// for between them and holding the machines to each other at every
    /// slice's end; answers the blocks installed.
    fn run(&mut self, span: Nanos, slice: Nanos) -> usize {
        let mut installed = 0;
        let end = self.plain.now() + span;
        while self.plain.now() < end {
            let until = self.plain.now() + slice;
            self.plain.run_until(until);
            self.jit.run_until(until);
            let (jit, board) = self.jit.blocks().unwrap();
            assert!(!jit.is_broken(), "a block answered no exit");
            while let Some(request) = jit.next_request() {
                let code = self.compiler.compile(&request);
                installed += usize::from(code.is_some());
                let code = code.map(|handle| (handle, 64));
                jit.answer((request.key, request.number), code, board);
            }
            assert_eq!(
                self.plain.cpu().cycles(),
                self.jit.cpu().cycles(),
                "cycles at {until}"
            );
            assert_eq!(
                self.plain.cpu().regs(),
                self.jit.cpu().regs(),
                "registers at {until}"
            );
            assert!(
                self.plain.snapshot(b"") == self.jit.snapshot(b""),
                "the machines differ at {until}"
            );
        }
        installed
    }
}

#[test]
fn a_machine_running_its_blocks_is_the_machine_interpreted() {
    for model in CpuModel::ALL {
        let mut config = Template::IbmXt.config();
        config.cpu = model;
        let mut pair = Pair::new(&config, &compute());
        let installed = pair.run(40 * MS, MS);
        assert!(installed >= 3, "{model:?}: {installed} blocks installed");
        assert!(
            pair.compiler.runs() > 1000,
            "{model:?}: {} blocks run",
            pair.compiler.runs()
        );
    }
}

#[test]
fn code_that_rewrites_itself_runs_as_it_is_rewritten() {
    for template in [Template::IbmPc, Template::Workstation186] {
        let mut pair = Pair::new(&template.config(), &rewriting());
        let installed = pair.run(60 * MS, MS);
        // Some forty rewrites in the time: recompiled a few times over,
        // then left to the interpreter.
        assert!(
            (4..30).contains(&installed),
            "{template:?}: {installed} blocks installed"
        );
        assert!(
            pair.compiler.runs() > 300,
            "{template:?}: {} blocks run",
            pair.compiler.runs()
        );
    }
}

#[test]
fn code_run_from_an_expanded_memory_window_follows_the_page_it_shows() {
    let mut pair = Pair::with(&Template::TurboXt.config(), &calling_the_frame(), banked);
    let installed = pair.run(60 * MS, MS);
    assert!(installed >= 4, "{installed} blocks installed");
    for sum in SUMS {
        let at = u32::from(sum);
        let value =
            u16::from_le_bytes([pair.plain.board().peek(at), pair.plain.board().peek(at + 1)]);
        assert_ne!(value, 0, "the word at {sum:04X}");
    }
}

#[test]
fn a_block_waits_its_turn_behind_what_the_bus_is_owed() {
    let source = |m: &mut Machine| load(m, 0x3000, &[0xA5; 0x100]);
    let mut pair = Pair::with(&Template::Workstation186.config(), &transferring(), source);
    let installed = pair.run(30 * MS, MS);
    assert!(installed >= 3, "{installed} blocks installed");
    assert_eq!(pair.plain.board().peek(0x40FF), 0xA5, "the DMA copied");
}

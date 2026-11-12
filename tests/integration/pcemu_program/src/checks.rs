//! The checks' pure part: the program the fixture's diskette holds, the sum
//! it prints, and what a screen must show.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_fat::stamp::Stamp;
use tairix_pcemu::asm::{Asm, AsmError, Cond, Reg16, Reg8};
use tairix_pcemu::dos::setup::ROOT_PROMPT;
use tairix_pcemu::dos::volume::{self, File, VolumeError};
use tairix_pcemu::dos::Diskette;
use tairix_pcemu::link::Text;
use tairix_pcemu::storage::sparse::SparseDisk;

use crate::PASS_MARKER;

/// The command typed at DOS's prompt: the program, from the diskette in A:.
pub const TYPED: &[u8] = b"A:LOOP";

/// The program's name on the diskette.
const PROGRAM: &str = "LOOP.COM";

/// The diskette's volume serial number.
const DISKETTE_SERIAL: u32 = 0x4C4F_4F50;

/// Times the program's count runs down: with [`COUNT`], some seconds of the
/// default machine's time, long enough for its loop to be compiled and run
/// natively for most of them.
const PASSES: u16 = 5_000;

/// What the count runs down from.
const COUNT: u16 = 1_000;

/// Where a `.COM` program is loaded in its segment.
const COM_ORIGIN: u16 = 0x100;

/// Hex digits in a word.
const DIGITS: u16 = 4;

/// Bits in a hex digit.
const DIGIT_BITS: u8 = 4;

// Instructions the encoder has no method for.
const ADD_AX_CX: [u8; 2] = [0x01, 0xC8];
const ROL_BX_CL: [u8; 2] = [0xD3, 0xC3];
const MOV_DL_BL: [u8; 2] = [0x88, 0xDA];
const AND_DL_LOW_NIBBLE: [u8; 3] = [0x80, 0xE2, 0x0F];

fn add_dl(asm: &mut Asm, value: u8) {
    asm.bytes(&[0x80, 0xC2, value]);
}

/// DOS's character output: DL to the console.
fn print_dl(asm: &mut Asm) {
    asm.mov_imm8(Reg8::Ah, 2);
    asm.int(0x21);
}

/// [`PROGRAM`]: [`PASSES`] times, adds [`COUNT`] down to 1 into AX; then
/// prints AX as four hex digits and a line break, and exits with code 0.
fn program() -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(COM_ORIGIN);
    let (pass, count, digit, decimal) = (asm.label(), asm.label(), asm.label(), asm.label());
    asm.zero(Reg16::Ax);
    asm.mov_imm16(Reg16::Dx, PASSES);
    asm.bind(&pass);
    asm.mov_imm16(Reg16::Cx, COUNT);
    asm.bind(&count);
    asm.bytes(&ADD_AX_CX);
    asm.dec16(Reg16::Cx);
    asm.jcc(Cond::Ne, &count);
    asm.dec16(Reg16::Dx);
    asm.jcc(Cond::Ne, &pass);
    asm.mov16(Reg16::Bx, Reg16::Ax);
    asm.mov_imm16(Reg16::Si, DIGITS);
    asm.bind(&digit);
    asm.mov_imm8(Reg8::Cl, DIGIT_BITS);
    asm.bytes(&ROL_BX_CL);
    asm.bytes(&MOV_DL_BL);
    asm.bytes(&AND_DL_LOW_NIBBLE);
    add_dl(&mut asm, b'0');
    asm.cmp_imm8(Reg8::Dl, b'9');
    asm.jcc(Cond::Be, &decimal);
    add_dl(&mut asm, b'A' - b'9' - 1);
    asm.bind(&decimal);
    print_dl(&mut asm);
    asm.dec16(Reg16::Si);
    asm.jcc(Cond::Ne, &digit);
    for byte in *b"\r\n" {
        asm.mov_imm8(Reg8::Dl, byte);
        print_dl(&mut asm);
    }
    asm.mov_imm16(Reg16::Ax, 0x4C00);
    asm.int(0x21);
    asm.finish()
}

/// AX once the loop is done: [`PASSES`] times the triangular number of
/// [`COUNT`], modulo 2¹⁶ as the register keeps it.
fn sum() -> u16 {
    let triangle = u32::from(COUNT) * (u32::from(COUNT) + 1) / 2;
    let [low, high, ..] = triangle.to_le_bytes();
    u16::from_le_bytes([low, high]).wrapping_mul(PASSES)
}

/// The line the program prints: its sum, as four hex digits.
#[must_use]
pub fn sum_line() -> String {
    format!("{:04X}", sum())
}

/// Why the diskette could not be made: a defect of the fixture's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmade {
    /// The program did not assemble.
    Program(AsmError),
    /// The volume could not hold it.
    Volume(VolumeError),
}

/// A 1.44 MiB diskette holding the program alone.
///
/// # Errors
///
/// [`Unmade`].
pub fn diskette() -> Result<SparseDisk, Unmade> {
    let program = program().map_err(Unmade::Program)?;
    let bpb = Diskette::K1440.volume(DISKETTE_SERIAL);
    let mut disk = SparseDisk::new(u64::from(bpb.total_sectors));
    let files = [File {
        path: PROGRAM,
        bytes: &program,
        attributes: 0,
    }];
    volume::build(&bpb, None, &files, Stamp::NONE, &mut disk).map_err(Unmade::Volume)?;
    Ok(disk)
}

/// Whether `text` stands at the prompt the default disk leaves the shell at.
#[must_use]
pub fn at_prompt(text: &Text<'_>) -> bool {
    text.last_line() == ROOT_PROMPT
}

/// Whether `text` shows [`TYPED`] answered with `sum`: the command after the
/// prompt, the sum below it, and the prompt again.
#[must_use]
pub fn answered(text: &Text<'_>, sum: &[u8]) -> bool {
    let mut after = text
        .lines()
        .skip_while(|line| line.strip_prefix(ROOT_PROMPT) != Some(TYPED));
    after.next().is_some() && after.any(|line| line == sum) && at_prompt(text)
}

/// The line a pass prints: the marker, then what the run did.
#[must_use]
pub fn pass_line(installed: u32, runs: u64) -> String {
    format!("{PASS_MARKER}: {installed} blocks installed, {runs} runs of them native\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use tairix_pcemu::cpu::{gpr, linear, Bus, Cpu, CpuModel, Seg, Stop};
    use tairix_pcemu::storage::BlockDevice;

    /// Where the test's DOS is: its INT 21h, a host call.
    const DOS: (u16, u16) = (0xF000, 0);
    /// The program's segment.
    const PSP: u16 = 0x1000;
    /// Cycles the program has to end in: many times what it takes.
    const CYCLES: u64 = 1 << 32;

    /// A megabyte of memory, its INT 21h handler a host call.
    struct Flat(Vec<u8>);

    fn at(addr: u32) -> usize {
        usize::try_from(addr).unwrap()
    }

    impl Bus for Flat {
        fn read(&mut self, addr: u32, _at: u64) -> u8 {
            self.peek(addr)
        }
        fn write(&mut self, addr: u32, value: u8, _at: u64) {
            self.0[at(addr)] = value;
        }
        fn peek(&self, addr: u32) -> u8 {
            self.0[at(addr)]
        }
        fn input(&mut self, _port: u16, _at: u64) -> u8 {
            0xFF
        }
        fn output(&mut self, _port: u16, _value: u8, _at: u64) {}
        fn intr(&self) -> bool {
            false
        }
        fn acknowledge(&mut self) -> u8 {
            0
        }
        fn nmi(&self) -> bool {
            false
        }
        fn acknowledge_nmi(&mut self) {}
        fn host_code(&self, addr: u32) -> bool {
            addr == linear(DOS.0, DOS.1)
        }
        fn steal(&mut self, _cycles: u32) {}
    }

    /// What `program` printed on the default machine's processor, and the
    /// code it exited with.
    fn run(program: &[u8]) -> (Vec<u8>, u8) {
        let mut memory = vec![0u8; 1 << 20];
        let load = at(linear(PSP, COM_ORIGIN));
        memory[load..load + program.len()].copy_from_slice(program);
        let vector = 4 * 0x21;
        memory[vector..vector + 2].copy_from_slice(&DOS.1.to_le_bytes());
        memory[vector + 2..vector + 4].copy_from_slice(&DOS.0.to_le_bytes());
        let mut handler = Asm::new(DOS.1);
        handler.host_call(0x21);
        handler.iret();
        let handler = handler.finish().unwrap();
        let entry = at(linear(DOS.0, DOS.1));
        memory[entry..entry + handler.len()].copy_from_slice(&handler);
        let mut bus = Flat(memory);
        let mut cpu = Cpu::new(CpuModel::V20);
        let regs = cpu.regs_mut();
        for seg in [Seg::Cs, Seg::Ds, Seg::Es, Seg::Ss] {
            regs.set_sreg(seg, PSP);
        }
        regs.ip = COM_ORIGIN;
        regs.gpr[gpr::SP] = 0xFFFE;
        let mut printed = Vec::new();
        loop {
            match cpu.run(&mut bus, CYCLES) {
                Stop::HostCall(0x21) => {
                    let regs = cpu.regs();
                    match regs.get8(Reg8::Ah as u8) {
                        0x02 => printed.push(regs.get8(Reg8::Dl as u8)),
                        0x4C => return (printed, regs.get8(Reg8::Al as u8)),
                        other => panic!("INT 21h function {other:#04x}"),
                    }
                }
                stop => panic!("the program stopped: {stop:?}"),
            }
        }
    }

    #[test]
    fn the_program_prints_its_sum_and_exits_cleanly() {
        let (printed, code) = run(&program().unwrap());
        assert_eq!(printed, [sum_line().as_bytes(), b"\r\n"].concat());
        assert_eq!(code, 0);
    }

    #[test]
    fn the_diskette_is_a_high_density_one() {
        let disk = diskette().unwrap();
        let bpb = Diskette::K1440.volume(DISKETTE_SERIAL);
        assert_eq!(disk.sectors(), u64::from(bpb.total_sectors));
    }

    /// A screen of `rows`, each padded to the width.
    fn screen(rows: &[&[u8]]) -> Vec<u8> {
        rows.iter()
            .flat_map(|row| {
                let mut padded = row.to_vec();
                padded.resize(40, b' ');
                padded
            })
            .collect()
    }

    fn text(codes: &[u8]) -> Text<'_> {
        Text {
            columns: 40,
            rows: u16::try_from(codes.len() / 40).unwrap(),
            codes,
        }
    }

    #[test]
    fn the_prompt_is_the_last_line_holding_anything() {
        assert!(at_prompt(&text(&screen(&[b"", ROOT_PROMPT, b""]))));
        assert!(!at_prompt(&text(&screen(&[ROOT_PROMPT, b"Starting"]))));
    }

    #[test]
    fn the_program_is_answered_only_below_its_own_command_and_at_a_prompt_again() {
        let sum = sum_line();
        let sum = sum.as_bytes();
        let command = [ROOT_PROMPT, TYPED].concat();
        let answer = screen(&[&command, sum, b"", ROOT_PROMPT]);
        assert!(answered(&text(&answer), sum));
        let above = screen(&[sum, ROOT_PROMPT, &command]);
        assert!(
            !answered(&text(&above), sum),
            "a sum above the command is not its answer"
        );
        let running = screen(&[&command]);
        assert!(!answered(&text(&running), sum), "typed, not yet answered");
        let wrong = screen(&[&command, b"0000", ROOT_PROMPT]);
        assert!(!answered(&text(&wrong), sum), "another sum");
    }
}

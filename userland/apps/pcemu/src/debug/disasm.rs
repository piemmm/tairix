//! The debugger's listing: instructions from a CS:IP in Intel syntax as MASM
//! writes it, decoded by the CPU's one decoder and, for the 8087's, by the
//! coprocessor's; the V-series' 8080 emulation mode in the 8080's own
//! mnemonics. Memory is read without side effects, so listing code a
//! device's registers overlay changes nothing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::i8080;
use crate::cpu::decode::{
    decode, Alu, Base, Cond, Count, Ea, Fetch, Insn, Mem, Nec, Op, Prefix, Rep, Shift, Unary,
    LONGEST,
};
use crate::cpu::{linear, Bus, CpuModel, Seg};
use crate::fpu::exec::{self as fpu, Operand};

/// Prefixes a line folds into its instruction at most; one past them is a
/// line of its own.
const MOST_PREFIXES: usize = 6;
/// The bytes a line holds at most.
pub const MOST_BYTES: usize = MOST_PREFIXES + LONGEST;
/// The characters a line's text runs to at most: `LOCK` and the widest
/// memory operand, the longest immediate after it.
pub const MOST_TEXT: usize = 48;
/// The furthest back a listing looks for the instructions before its first.
const REACH: u16 = 128;

const REG8: [&str; 8] = ["AL", "CL", "DL", "BL", "AH", "CH", "DH", "BH"];
const REG16: [&str; 8] = ["AX", "CX", "DX", "BX", "SP", "BP", "SI", "DI"];
const SEGMENTS: [&str; 4] = ["ES", "CS", "SS", "DS"];

/// One line of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Its offset in the code segment.
    pub ip: u16,
    /// Its bytes, at most [`MOST_BYTES`].
    pub bytes: Vec<u8>,
    /// What they say.
    pub text: String,
}

/// How the processor is reading code: the listing reads it so too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The processor.
    pub model: CpuModel,
    /// Whether a V-series part is in 8080 emulation mode.
    pub emulating: bool,
}

/// What an instruction does to the flow of the code around it, as a step
/// over it and a step out of its procedure must know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Runs code that returns to the next instruction: a CALL, an INT, a
    /// repeated string instruction an interrupt may enter a handler from.
    Calls,
    /// Returns to an address it pops.
    Returns(Return),
    /// Anything else.
    Other,
}

/// How a return pops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Return {
    /// IP from SS:SP, then this many more bytes.
    Near(u16),
    /// IP and CS from SS:SP, then this many more bytes.
    Far(u16),
    /// IP, CS and FLAGS from SS:SP: IRET, and RETEM leaving 8080 mode.
    Interrupt,
    /// The 8080's RET, or RET on condition `cc`: IP from DS:BP.
    I8080(Option<u8>),
}

/// The bytes from `cs:ip` as the decoder takes them, read without side
/// effects, the offset wrapping within the segment as the CPU's does.
struct Peeking<'a, B: Bus + ?Sized> {
    bus: &'a B,
    cs: u16,
    ip: u16,
    taken: [u8; MOST_BYTES],
    len: usize,
}

impl<B: Bus + ?Sized> Fetch for Peeking<'_, B> {
    fn fetch(&mut self) -> u8 {
        let byte = self.bus.peek(linear(self.cs, self.ip));
        self.ip = self.ip.wrapping_add(1);
        if let Some(slot) = self.taken.get_mut(self.len) {
            *slot = byte;
            self.len += 1;
        }
        byte
    }
}

/// The prefixes gathered ahead of an instruction.
#[derive(Clone, Copy, Debug, Default)]
struct Prefixes {
    segment: Option<Seg>,
    rep: Option<Rep>,
    lock: bool,
}

/// A native instruction as the processor would take it from `cs:ip`: its
/// prefixes folded in, and its bytes.
struct Unit {
    insn: Insn,
    prefixes: Prefixes,
    bytes: [u8; MOST_BYTES],
    len: usize,
    /// The next instruction's offset.
    next: u16,
}

fn unit<B: Bus + ?Sized>(model: CpuModel, bus: &B, cs: u16, ip: u16) -> Unit {
    let mut fetch = Peeking {
        bus,
        cs,
        ip,
        taken: [0; MOST_BYTES],
        len: 0,
    };
    let mut prefixes = Prefixes::default();
    let mut gathered = 0;
    let insn = loop {
        let host = bus.host_code(linear(cs, fetch.ip));
        let insn = decode(model, host, &mut fetch);
        match insn.op {
            Op::Prefix(prefix) if gathered < MOST_PREFIXES => {
                gathered += 1;
                match prefix {
                    Prefix::Segment(seg) => prefixes.segment = Some(seg),
                    Prefix::Rep(rep) => prefixes.rep = Some(rep),
                    Prefix::Lock => prefixes.lock = true,
                }
            }
            _ => break insn,
        }
    };
    Unit {
        insn,
        prefixes,
        bytes: fetch.taken,
        len: fetch.len,
        next: fetch.ip,
    }
}

/// The 8080 instruction at `cs:ip`.
fn unit_8080<B: Bus + ?Sized>(bus: &B, cs: u16, ip: u16) -> (i8080::Insn, [u8; i8080::LONGEST]) {
    let mut bytes = [0; i8080::LONGEST];
    for (slot, n) in bytes.iter_mut().zip(0..) {
        *slot = bus.peek(linear(cs, ip.wrapping_add(n)));
    }
    (i8080::decode(bytes), bytes)
}

/// The line at `cs:ip`.
fn line<B: Bus + ?Sized>(reading: Reading, bus: &B, cs: u16, ip: u16) -> Line {
    if reading.emulating {
        let (insn, bytes) = unit_8080(bus, cs, ip);
        return Line {
            ip,
            bytes: bytes[..usize::from(insn.len)].to_vec(),
            text: insn.text,
        };
    }
    let unit = unit(reading.model, bus, cs, ip);
    let mut text = if unit.prefixes.lock {
        String::from("LOCK ")
    } else {
        String::new()
    };
    text.push_str(&render(&unit.insn, unit.prefixes, unit.next));
    Line {
        ip,
        bytes: unit.bytes[..unit.len].to_vec(),
        text,
    }
}

/// The instruction at `cs:ip` as `reading` takes it: its length and its
/// flow.
pub(crate) fn flow<B: Bus + ?Sized>(reading: Reading, bus: &B, cs: u16, ip: u16) -> (u16, Flow) {
    if reading.emulating {
        let (insn, _) = unit_8080(bus, cs, ip);
        let flow = match insn.flow {
            i8080::Flow::Calls => Flow::Calls,
            i8080::Flow::Returns(cc) => Flow::Returns(Return::I8080(cc)),
            i8080::Flow::LeavesEmulation => Flow::Returns(Return::Interrupt),
            i8080::Flow::Other => Flow::Other,
        };
        return (u16::from(insn.len), flow);
    }
    let unit = unit(reading.model, bus, cs, ip);
    let len = unit.next.wrapping_sub(ip);
    let insn = &unit.insn;
    let flow = match insn.op {
        Op::CallNear
        | Op::CallFar
        | Op::CallRm
        | Op::CallFarRm
        | Op::Int
        | Op::Int3
        | Op::Into
        | Op::Nec(Nec::Brkem) => Flow::Calls,
        Op::Movs | Op::Cmps | Op::Stos | Op::Lods | Op::Scas | Op::Ins | Op::Outs
            if unit.prefixes.rep.is_some() =>
        {
            Flow::Calls
        }
        Op::Ret { pops } => Flow::Returns(Return::Near(if pops { insn.imm } else { 0 })),
        Op::Retf { pops } => Flow::Returns(Return::Far(if pops { insn.imm } else { 0 })),
        Op::Iret => Flow::Returns(Return::Interrupt),
        _ => Flow::Other,
    };
    (len, flow)
}

/// `after` lines of `reading`'s code from `cs:ip` in `bus`, and up to
/// `before` lines of what leads to it.
#[must_use]
pub fn listing<B: Bus + ?Sized>(
    reading: Reading,
    bus: &B,
    (cs, ip): (u16, u16),
    (before, after): (usize, usize),
) -> Vec<Line> {
    let mut lines = leading(reading, bus, (cs, ip), before);
    let mut at = ip;
    for _ in 0..after {
        let line = line(reading, bus, cs, at);
        at = at.wrapping_add(u16::try_from(line.bytes.len()).unwrap_or(1));
        lines.push(line);
    }
    lines
}

/// Up to `count` lines ending where `ip` begins. Code read backwards is
/// ambiguous, but x86 decoding resynchronises: every start within [`REACH`]
/// is decoded forwards, each instruction of a run that meets `ip` exactly
/// gets a vote, and the lines are those most runs agree on, the nearer of
/// two equally agreed taken.
fn leading<B: Bus + ?Sized>(
    reading: Reading,
    bus: &B,
    (cs, ip): (u16, u16),
    count: usize,
) -> Vec<Line> {
    let length = |at: u16| flow(reading, bus, cs, at).0.max(1);
    // Room for the lines asked for, and a lead-in for the runs to agree in.
    let lines = u16::try_from(count).unwrap_or(u16::MAX);
    let reach = REACH.min(lines.saturating_mul(4).saturating_add(32));
    let first = ip.wrapping_sub(reach);
    let mut votes = [0u16; REACH as usize];
    for start in 0..reach {
        let mut at = first.wrapping_add(start);
        let mut run = [0u16; REACH as usize];
        let mut taken = 0;
        while at != ip && ip.wrapping_sub(at) <= reach {
            run[taken] = at.wrapping_sub(first);
            taken += 1;
            at = at.wrapping_add(length(at));
        }
        if at == ip {
            for &place in &run[..taken] {
                votes[usize::from(place)] += 1;
            }
        }
    }
    let mut starts = Vec::with_capacity(count);
    let mut end = ip;
    while starts.len() < count {
        let best = (1..=u16::try_from(MOST_BYTES).unwrap_or(1))
            .map(|back| end.wrapping_sub(back))
            .filter(|&at| at.wrapping_sub(first) < reach && at.wrapping_add(length(at)) == end)
            .map(|at| (votes[usize::from(at.wrapping_sub(first))], at))
            .filter(|&(votes, _)| votes > 0)
            .max_by_key(|&(votes, at)| (votes, at.wrapping_sub(first)));
        let Some((_, at)) = best else {
            break;
        };
        starts.push(at);
        end = at;
    }
    starts
        .iter()
        .rev()
        .map(|&at| line(reading, bus, cs, at))
        .collect()
}

/// `value` in MASM's hexadecimal: `digits` places, a leading zero where it
/// would begin with a letter, and `h`.
pub(crate) fn hex(value: u16, digits: usize) -> String {
    let text = format!("{value:0digits$X}h");
    if text.starts_with(|c: char| c.is_ascii_alphabetic()) {
        format!("0{text}")
    } else {
        text
    }
}

fn immediate(insn: &Insn) -> String {
    if insn.w {
        hex(insn.imm, 4)
    } else {
        hex(insn.imm & 0xFF, 2)
    }
}

fn register(w: bool, n: u8) -> &'static str {
    let n = usize::from(n & 7);
    if w {
        REG16[n]
    } else {
        REG8[n]
    }
}

const fn segment(seg: Seg) -> &'static str {
    SEGMENTS[seg.index()]
}

const fn base(base: Base) -> &'static str {
    match base {
        Base::BxSi => "BX+SI",
        Base::BxDi => "BX+DI",
        Base::BpSi => "BP+SI",
        Base::BpDi => "BP+DI",
        Base::Si => "SI",
        Base::Di => "DI",
        Base::Bp => "BP",
        Base::Bx => "BX",
        Base::Absolute => "",
    }
}

/// A memory operand: `size` (a `PTR` or nothing), the segment override,
/// and the address, a byte displacement signed as its encoding has it.
fn memory(insn: &Insn, mem: Mem, size: &str, seg: Option<Seg>) -> String {
    let prefix = seg.map_or(String::new(), |seg| format!("{}:", segment(seg)));
    let size = if size.is_empty() {
        String::new()
    } else {
        format!("{size} ")
    };
    let address = if mem.base == Base::Absolute {
        hex(mem.disp, 4)
    } else if !mem.displaced {
        String::from(base(mem.base))
    } else if insn.modrm.is_some_and(|modrm| modrm >> 6 == 1) {
        let signed = mem.disp.cast_signed();
        let sign = if signed < 0 { '-' } else { '+' };
        format!("{}{sign}{}", base(mem.base), hex(signed.unsigned_abs(), 2))
    } else {
        format!("{}+{}", base(mem.base), hex(mem.disp, 4))
    };
    format!("{size}{prefix}[{address}]")
}

/// The `r/m` operand, its size written where nothing else gives it.
fn rm(insn: &Insn, sized: bool, prefixes: Prefixes) -> String {
    match insn.ea {
        Ea::Reg(n) => String::from(register(insn.w, n)),
        Ea::Mem(mem) => {
            let size = match (sized, insn.w) {
                (false, _) => "",
                (true, true) => "WORD PTR",
                (true, false) => "BYTE PTR",
            };
            memory(insn, mem, size, prefixes.segment)
        }
    }
}

fn reg(insn: &Insn) -> &'static str {
    register(insn.w, insn.reg)
}

const fn accumulator(insn: &Insn) -> &'static str {
    if insn.w {
        "AX"
    } else {
        "AL"
    }
}

const fn alu(op: Alu) -> &'static str {
    match op {
        Alu::Add => "ADD",
        Alu::Or => "OR",
        Alu::Adc => "ADC",
        Alu::Sbb => "SBB",
        Alu::And => "AND",
        Alu::Sub => "SUB",
        Alu::Xor => "XOR",
        Alu::Cmp => "CMP",
    }
}

const fn condition(cond: Cond) -> &'static str {
    match cond {
        Cond::O => "JO",
        Cond::No => "JNO",
        Cond::B => "JB",
        Cond::Nb => "JNB",
        Cond::E => "JE",
        Cond::Ne => "JNE",
        Cond::Be => "JBE",
        Cond::A => "JA",
        Cond::S => "JS",
        Cond::Ns => "JNS",
        Cond::P => "JP",
        Cond::Np => "JNP",
        Cond::L => "JL",
        Cond::Ge => "JGE",
        Cond::Le => "JLE",
        Cond::G => "JG",
    }
}

const fn shift(op: Shift) -> &'static str {
    match op {
        Shift::Rol => "ROL",
        Shift::Ror => "ROR",
        Shift::Rcl => "RCL",
        Shift::Rcr => "RCR",
        Shift::Shl => "SHL",
        Shift::Shr => "SHR",
        Shift::Setmo => "SETMO",
        Shift::Sar => "SAR",
    }
}

const fn unary(op: Unary) -> &'static str {
    match op {
        Unary::Test => "TEST",
        Unary::Not => "NOT",
        Unary::Neg => "NEG",
        Unary::Mul => "MUL",
        Unary::Imul => "IMUL",
        Unary::Div => "DIV",
        Unary::Idiv => "IDIV",
    }
}

/// A relative transfer's target: the next instruction's offset plus the
/// displacement, in the segment.
fn target(insn: &Insn, next: u16) -> String {
    hex(next.wrapping_add(insn.imm), 4)
}

/// `insn`, its prefixes gathered, the next instruction at `next`.
fn render(insn: &Insn, prefixes: Prefixes, next: u16) -> String {
    match insn.op {
        Op::AluRmReg(op) => format!("{} {},{}", alu(op), rm(insn, false, prefixes), reg(insn)),
        Op::AluRegRm(op) => format!("{} {},{}", alu(op), reg(insn), rm(insn, false, prefixes)),
        Op::AluAccImm(op) => format!("{} {},{}", alu(op), accumulator(insn), immediate(insn)),
        Op::AluRmImm(op) => format!(
            "{} {},{}",
            alu(op),
            rm(insn, true, prefixes),
            immediate(insn)
        ),
        Op::TestRmReg => format!("TEST {},{}", rm(insn, false, prefixes), reg(insn)),
        Op::XchgRmReg => format!("XCHG {},{}", rm(insn, false, prefixes), reg(insn)),
        Op::MovRmReg => format!("MOV {},{}", rm(insn, false, prefixes), reg(insn)),
        Op::MovRegRm => format!("MOV {},{}", reg(insn), rm(insn, false, prefixes)),
        Op::MovRmImm => format!("MOV {},{}", rm(insn, true, prefixes), immediate(insn)),
        Op::Shift(op, count) => {
            let count = match count {
                Count::One => String::from("1"),
                Count::Cl => String::from("CL"),
                Count::Imm => hex(insn.imm & 0xFF, 2),
            };
            format!("{} {},{count}", shift(op), rm(insn, true, prefixes))
        }
        Op::Unary(Unary::Test) => {
            format!("TEST {},{}", rm(insn, true, prefixes), immediate(insn))
        }
        Op::Unary(op) => format!("{} {}", unary(op), rm(insn, true, prefixes)),
        Op::IncRm => format!("INC {}", rm(insn, true, prefixes)),
        Op::DecRm => format!("DEC {}", rm(insn, true, prefixes)),
        _ => render_rest(insn, prefixes, next),
    }
}

fn render_rest(insn: &Insn, prefixes: Prefixes, next: u16) -> String {
    let word = |name: &str| format!("{name} {}", REG16[usize::from(insn.reg & 7)]);
    let seg_field = segment(Seg::from_field(insn.reg));
    match insn.op {
        Op::PushSeg(seg) => format!("PUSH {}", segment(seg)),
        Op::PopSeg(seg) => format!("POP {}", segment(seg)),
        Op::IncReg => word("INC"),
        Op::DecReg => word("DEC"),
        Op::PushReg => word("PUSH"),
        Op::PopReg => word("POP"),
        Op::XchgAx => match REG16[usize::from(insn.reg & 7)] {
            "AX" => String::from("NOP"),
            other => format!("XCHG AX,{other}"),
        },
        Op::MovRegImm => format!("MOV {},{}", reg(insn), immediate(insn)),
        Op::MovRmSeg => format!("MOV {},{seg_field}", rm(insn, false, prefixes)),
        Op::MovSegRm => format!("MOV {seg_field},{}", rm(insn, false, prefixes)),
        Op::Lea => format!("LEA {},{}", reg(insn), rm(insn, false, prefixes)),
        Op::Les => format!("LES {},{}", reg(insn), rm(insn, false, prefixes)),
        Op::Lds => format!("LDS {},{}", reg(insn), rm(insn, false, prefixes)),
        Op::Bound => format!("BOUND {},{}", reg(insn), rm(insn, false, prefixes)),
        Op::PopRm => format!("POP {}", rm(insn, true, prefixes)),
        Op::PushRm => format!("PUSH {}", rm(insn, true, prefixes)),
        Op::MovAccMem | Op::MovMemAcc => {
            let size = if insn.w { "WORD PTR" } else { "BYTE PTR" };
            let mem = Mem {
                base: Base::Absolute,
                disp: insn.imm,
                displaced: false,
            };
            let operand = memory(insn, mem, size, prefixes.segment);
            if insn.op == Op::MovAccMem {
                format!("MOV {},{operand}", accumulator(insn))
            } else {
                format!("MOV {operand},{}", accumulator(insn))
            }
        }
        Op::TestAccImm => format!("TEST {},{}", accumulator(insn), immediate(insn)),
        Op::ImulImm => format!(
            "IMUL {},{},{}",
            reg(insn),
            rm(insn, false, prefixes),
            hex(insn.imm, 4)
        ),
        Op::PushImm => format!("PUSH {}", hex(insn.imm, 4)),
        Op::InImm => format!("IN {},{}", accumulator(insn), hex(insn.imm & 0xFF, 2)),
        Op::OutImm => format!("OUT {},{}", hex(insn.imm & 0xFF, 2), accumulator(insn)),
        Op::InDx => format!("IN {},DX", accumulator(insn)),
        Op::OutDx => format!("OUT DX,{}", accumulator(insn)),
        Op::Movs | Op::Cmps | Op::Stos | Op::Lods | Op::Scas | Op::Ins | Op::Outs => {
            string(insn, prefixes)
        }
        Op::Esc => escape(insn, prefixes),
        Op::Nec(op) => nec(insn, op, prefixes),
        _ => transfer(insn, next),
    }
}

/// A string instruction, its repeat and segment override written ahead.
fn string(insn: &Insn, prefixes: Prefixes) -> String {
    let (name, compares) = match insn.op {
        Op::Movs => ("MOVS", false),
        Op::Cmps => ("CMPS", true),
        Op::Stos => ("STOS", false),
        Op::Lods => ("LODS", false),
        Op::Scas => ("SCAS", true),
        Op::Ins => ("INS", false),
        _ => ("OUTS", false),
    };
    let rep = match (prefixes.rep, compares) {
        (None, _) => "",
        (Some(Rep::Z), false) => "REP ",
        (Some(Rep::Z), true) => "REPE ",
        (Some(Rep::Nz), _) => "REPNE ",
        (Some(Rep::C), _) => "REPC ",
        (Some(Rep::Nc), _) => "REPNC ",
    };
    let seg = prefixes
        .segment
        .map_or(String::new(), |seg| format!("{}: ", segment(seg)));
    let width = if insn.w { 'W' } else { 'B' };
    format!("{seg}{rep}{name}{width}")
}

/// The transfers and the instructions with no operand.
fn transfer(insn: &Insn, next: u16) -> String {
    let far = || format!("{}:{}", hex(insn.imm2, 4), hex(insn.imm, 4));
    let named = |name: &str| String::from(name);
    match insn.op {
        Op::Jcc(cond) => format!("{} {}", condition(cond), target(insn, next)),
        Op::Loopne => format!("LOOPNE {}", target(insn, next)),
        Op::Loope => format!("LOOPE {}", target(insn, next)),
        Op::Loop => format!("LOOP {}", target(insn, next)),
        Op::Jcxz => format!("JCXZ {}", target(insn, next)),
        Op::CallNear => format!("CALL {}", target(insn, next)),
        Op::JmpNear | Op::JmpShort => format!("JMP {}", target(insn, next)),
        Op::CallFar => format!("CALL {}", far()),
        Op::JmpFar => format!("JMP {}", far()),
        Op::CallRm | Op::JmpRm | Op::CallFarRm | Op::JmpFarRm => indirect(insn),
        Op::Ret { pops: true } => format!("RET {}", hex(insn.imm, 4)),
        Op::Ret { pops: false } => named("RET"),
        Op::Retf { pops: true } => format!("RETF {}", hex(insn.imm, 4)),
        Op::Retf { pops: false } => named("RETF"),
        Op::Int => format!("INT {}", hex(insn.imm & 0xFF, 2)),
        Op::Int3 => named("INT 3"),
        Op::Aam if insn.imm & 0xFF == 10 => named("AAM"),
        Op::Aam => format!("AAM {}", hex(insn.imm & 0xFF, 2)),
        Op::Aad if insn.imm & 0xFF == 10 => named("AAD"),
        Op::Aad => format!("AAD {}", hex(insn.imm & 0xFF, 2)),
        Op::Enter => format!("ENTER {},{}", hex(insn.imm, 4), hex(insn.imm2 & 0xFF, 2)),
        Op::HostCall => format!("HOSTCALL {}", hex(insn.imm, 4)),
        _ => named(bare(insn.op)),
    }
}

/// CALL and JMP through a register or memory.
fn indirect(insn: &Insn) -> String {
    let name = if matches!(insn.op, Op::CallRm | Op::CallFarRm) {
        "CALL"
    } else {
        "JMP"
    };
    let far = matches!(insn.op, Op::CallFarRm | Op::JmpFarRm);
    match insn.ea {
        Ea::Reg(n) => format!("{name} {}", REG16[usize::from(n & 7)]),
        Ea::Mem(mem) => {
            let size = if far { "DWORD PTR" } else { "WORD PTR" };
            format!("{name} {}", memory(insn, mem, size, None))
        }
    }
}

/// The names of the instructions with no operand.
const fn bare(op: Op) -> &'static str {
    match op {
        Op::Daa => "DAA",
        Op::Das => "DAS",
        Op::Aaa => "AAA",
        Op::Aas => "AAS",
        Op::Cbw => "CBW",
        Op::Cwd => "CWD",
        Op::Wait => "WAIT",
        Op::Pushf => "PUSHF",
        Op::Popf => "POPF",
        Op::Sahf => "SAHF",
        Op::Lahf => "LAHF",
        Op::Into => "INTO",
        Op::Iret => "IRET",
        Op::Salc => "SALC",
        Op::Xlat => "XLAT",
        Op::Hlt => "HLT",
        Op::Cmc => "CMC",
        Op::Clc => "CLC",
        Op::Stc => "STC",
        Op::Cli => "CLI",
        Op::Sti => "STI",
        Op::Cld => "CLD",
        Op::Std => "STD",
        Op::Pusha => "PUSHA",
        Op::Popa => "POPA",
        Op::Leave => "LEAVE",
        Op::Nop => "NOP",
        Op::Prefix(Prefix::Segment(Seg::Es)) => "ES:",
        Op::Prefix(Prefix::Segment(Seg::Cs)) => "CS:",
        Op::Prefix(Prefix::Segment(Seg::Ss)) => "SS:",
        Op::Prefix(Prefix::Segment(Seg::Ds)) => "DS:",
        Op::Prefix(Prefix::Rep(_)) => "REP",
        Op::Prefix(Prefix::Lock) => "LOCK",
        _ => "(unused)",
    }
}

/// An ESC: the 8087 instruction its opcode names, or NEC's `FPO2`.
fn escape(insn: &Insn, prefixes: Prefixes) -> String {
    if !(0xD8..=0xDF).contains(&insn.opcode) {
        return String::from("FPO2");
    }
    let modrm = insn.modrm.unwrap_or(0);
    let opcode = (u16::from(insn.opcode & 7) << 8) | u16::from(modrm);
    let decoded = fpu::decode(opcode, matches!(insn.ea, Ea::Mem(_)));
    let (name, operand) = fpu::mnemonic(decoded);
    match (operand, insn.ea) {
        (Operand::None, _) | (Operand::Memory(_), Ea::Reg(_)) => String::from(name),
        (Operand::Memory(size), Ea::Mem(mem)) => {
            format!("{name} {}", memory(insn, mem, size, prefixes.segment))
        }
        (Operand::Register(i), _) => format!("{name} ST({i})"),
        (Operand::ToTop(i), _) => format!("{name} ST,ST({i})"),
        (Operand::FromTop(i), _) => format!("{name} ST({i}),ST"),
    }
}

/// A NEC extended operation.
fn nec(insn: &Insn, op: Nec, prefixes: Prefixes) -> String {
    // The second byte's bit 3 says an immediate bit or field takes CL's
    // place.
    let immediate_form = insn.opcode & 8 != 0;
    let bit = || {
        if immediate_form {
            hex(insn.imm & 0xFF, 2)
        } else {
            String::from("CL")
        }
    };
    let name = match op {
        Nec::Test1 => "TEST1",
        Nec::Clr1 => "CLR1",
        Nec::Set1 => "SET1",
        Nec::Not1 => "NOT1",
        Nec::Add4s => return String::from("ADD4S"),
        Nec::Sub4s => return String::from("SUB4S"),
        Nec::Cmp4s => return String::from("CMP4S"),
        Nec::Rol4 => return format!("ROL4 {}", rm(insn, true, prefixes)),
        Nec::Ror4 => return format!("ROR4 {}", rm(insn, true, prefixes)),
        Nec::InsBits | Nec::ExtBits => {
            let name = if op == Nec::InsBits { "INS" } else { "EXT" };
            let field = if immediate_form {
                hex(insn.imm & 0x0F, 2)
            } else {
                String::from(register(false, insn.reg))
            };
            return format!("{name} {},{field}", rm(insn, false, prefixes));
        }
        Nec::Brkem => return format!("BRKEM {}", hex(insn.imm & 0xFF, 2)),
        Nec::Unassigned => return String::from("(unused)"),
    };
    format!("{name} {},{}", rm(insn, true, prefixes), bit())
}

#[cfg(test)]
#[path = "disasm_tests.rs"]
mod tests;

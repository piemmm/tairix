//! What each mnemonic does, after its opcode has been fetched.
//!
//! A mnemonic is a marker type; its semantics are [`Execute`] impls, generic
//! over the operand flavour a mode supplies where every such mode behaves
//! alike, and concrete where the instruction's cycles are its own.

use super::modes::{
    indexed, zero_page_pointer, Absolute, AbsoluteX, AbsoluteY, Implied, Indirect, IndirectIndexed,
    Modify, Read, Relative, Write,
};
use super::{flag, Bus, Cpu};

/// The semantics of an instruction in one addressing mode.
pub trait Execute<M> {
    /// Perform the instruction's cycles after the opcode fetch.
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B);
}

macro_rules! mnemonics {
    ($($name:ident),* $(,)?) => {$(
        #[doc = concat!("The `", stringify!($name), "` instruction.")]
        pub struct $name;
    )*};
}

mnemonics!(
    Adc, And, Asl, Bcc, Bcs, Beq, Bit, Bmi, Bne, Bpl, Brk, Bvc, Bvs, Clc, Cld, Cli, Clv, Cmp, Cpx,
    Cpy, Dec, Dex, Dey, Eor, Inc, Inx, Iny, Jmp, Jsr, Lda, Ldx, Ldy, Lsr, Nop, Ora, Pha, Php, Pla,
    Plp, Rol, Ror, Rti, Rts, Sbc, Sec, Sed, Sei, Sta, Stx, Sty, Tax, Tay, Tsx, Txa, Txs, Tya, Alr,
    Anc, Ane, Arr, Dcp, Isc, Jam, Las, Lax, Lxa, Rla, Rra, Sax, Sbx, Sha, Shx, Shy, Slo, Sre, Tas,
);

/// The constant the unstable `ANE` and `LXA` merge into the accumulator. It
/// varies between chips and with temperature; `0xEE` is the value most NMOS
/// parts settle on.
const MAGIC: u8 = 0xEE;

macro_rules! read_ops {
    ($($name:ident => |$cpu:ident, $value:ident| $body:expr;)*) => {$(
        impl<M: Read> Execute<M> for $name {
            fn execute<B: Bus>($cpu: &mut Cpu, bus: &mut B) {
                let $value = M::read($cpu, bus);
                $body
            }
        }
    )*};
}

read_ops! {
    Lda => |cpu, v| cpu.load_a(v);
    Ldx => |cpu, v| { cpu.x = v; cpu.set_nz(v) };
    Ldy => |cpu, v| { cpu.y = v; cpu.set_nz(v) };
    Lax => |cpu, v| { cpu.x = v; cpu.load_a(v) };
    Las => |cpu, v| { let r = v & cpu.s; cpu.s = r; cpu.x = r; cpu.load_a(r) };
    Ora => |cpu, v| cpu.load_a(cpu.a | v);
    And => |cpu, v| cpu.load_a(cpu.a & v);
    Eor => |cpu, v| cpu.load_a(cpu.a ^ v);
    Adc => |cpu, v| cpu.adc(v);
    Sbc => |cpu, v| cpu.sbc(v);
    Cmp => |cpu, v| cpu.compare(cpu.a, v);
    Cpx => |cpu, v| cpu.compare(cpu.x, v);
    Cpy => |cpu, v| cpu.compare(cpu.y, v);
    Bit => |cpu, v| {
        cpu.z = cpu.a & v == 0;
        cpu.n = v & flag::N != 0;
        cpu.v = v & flag::V != 0;
    };
    Nop => |_cpu, _v| {};
    Anc => |cpu, v| { cpu.load_a(cpu.a & v); cpu.c = cpu.n };
    Alr => |cpu, v| { let r = cpu.lsr_value(cpu.a & v); cpu.a = r };
    Arr => |cpu, v| cpu.arr(v);
    Ane => |cpu, v| cpu.load_a((cpu.a | MAGIC) & cpu.x & v);
    Lxa => |cpu, v| { let r = (cpu.a | MAGIC) & v; cpu.x = r; cpu.load_a(r) };
    Sbx => |cpu, v| {
        let (r, borrow) = (cpu.a & cpu.x).overflowing_sub(v);
        cpu.c = !borrow;
        cpu.x = r;
        cpu.set_nz(r);
    };
}

macro_rules! store_ops {
    ($($name:ident => |$cpu:ident| $value:expr;)*) => {$(
        impl<M: Write> Execute<M> for $name {
            fn execute<B: Bus>($cpu: &mut Cpu, bus: &mut B) {
                let addr = M::address($cpu, bus);
                let value = $value;
                $cpu.write(bus, addr, value);
            }
        }
    )*};
}

store_ops! {
    Sta => |cpu| cpu.a;
    Stx => |cpu| cpu.x;
    Sty => |cpu| cpu.y;
    Sax => |cpu| cpu.a & cpu.x;
}

macro_rules! modify_ops {
    ($($name:ident => |$cpu:ident, $value:ident| $body:expr;)*) => {$(
        impl<M: Modify> Execute<M> for $name {
            fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
                M::modify(cpu, bus, |$cpu, $value| $body);
            }
        }
    )*};
}

modify_ops! {
    Asl => |cpu, v| cpu.asl_value(v);
    Lsr => |cpu, v| cpu.lsr_value(v);
    Rol => |cpu, v| cpu.rol_value(v);
    Ror => |cpu, v| cpu.ror_value(v);
    Inc => |cpu, v| { let r = v.wrapping_add(1); cpu.set_nz(r); r };
    Dec => |cpu, v| { let r = v.wrapping_sub(1); cpu.set_nz(r); r };
    Slo => |cpu, v| { let r = cpu.asl_value(v); cpu.load_a(cpu.a | r); r };
    Rla => |cpu, v| { let r = cpu.rol_value(v); cpu.load_a(cpu.a & r); r };
    Sre => |cpu, v| { let r = cpu.lsr_value(v); cpu.load_a(cpu.a ^ r); r };
    Rra => |cpu, v| { let r = cpu.ror_value(v); cpu.adc(r); r };
    Dcp => |cpu, v| { let r = v.wrapping_sub(1); cpu.compare(cpu.a, r); r };
    Isc => |cpu, v| { let r = v.wrapping_add(1); cpu.sbc(r); r };
}

macro_rules! implied_ops {
    ($($name:ident => |$cpu:ident, $bus:ident| $body:expr;)*) => {$(
        impl Execute<Implied> for $name {
            fn execute<B: Bus>($cpu: &mut Cpu, $bus: &mut B) {
                $cpu.dummy_read_pc($bus);
                $body
            }
        }
    )*};
}

implied_ops! {
    Clc => |cpu, bus| cpu.c = false;
    Sec => |cpu, bus| cpu.c = true;
    Cli => |cpu, bus| cpu.i = false;
    Sei => |cpu, bus| cpu.i = true;
    Cld => |cpu, bus| cpu.d = false;
    Sed => |cpu, bus| cpu.d = true;
    Clv => |cpu, bus| cpu.v = false;
    Tax => |cpu, bus| { cpu.x = cpu.a; cpu.set_nz(cpu.x) };
    Tay => |cpu, bus| { cpu.y = cpu.a; cpu.set_nz(cpu.y) };
    Txa => |cpu, bus| cpu.load_a(cpu.x);
    Tya => |cpu, bus| cpu.load_a(cpu.y);
    Tsx => |cpu, bus| { cpu.x = cpu.s; cpu.set_nz(cpu.x) };
    Txs => |cpu, bus| cpu.s = cpu.x;
    Inx => |cpu, bus| { cpu.x = cpu.x.wrapping_add(1); cpu.set_nz(cpu.x) };
    Iny => |cpu, bus| { cpu.y = cpu.y.wrapping_add(1); cpu.set_nz(cpu.y) };
    Dex => |cpu, bus| { cpu.x = cpu.x.wrapping_sub(1); cpu.set_nz(cpu.x) };
    Dey => |cpu, bus| { cpu.y = cpu.y.wrapping_sub(1); cpu.set_nz(cpu.y) };
    Nop => |_cpu, bus| {};
    Pha => |cpu, bus| { let a = cpu.a; cpu.push(bus, a) };
    Php => |cpu, bus| { let p = cpu.status() | flag::B; cpu.push(bus, p) };
    Pla => |cpu, bus| { cpu.dummy_read_stack(bus); let v = cpu.pull(bus); cpu.load_a(v) };
    Plp => |cpu, bus| { cpu.dummy_read_stack(bus); let p = cpu.pull(bus); cpu.set_status(p) };
    Rts => |cpu, bus| {
        cpu.dummy_read_stack(bus);
        let lo = cpu.pull(bus);
        let hi = cpu.pull(bus);
        cpu.pc = u16::from_le_bytes([lo, hi]);
        cpu.fetch(bus);
    };
    Rti => |cpu, bus| {
        cpu.dummy_read_stack(bus);
        let p = cpu.pull(bus);
        cpu.set_status(p);
        let lo = cpu.pull(bus);
        let hi = cpu.pull(bus);
        cpu.pc = u16::from_le_bytes([lo, hi]);
    };
}

impl Execute<Implied> for Brk {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        cpu.fetch(bus);
        cpu.interrupt(bus, true);
    }
}

impl Execute<Implied> for Jam {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        cpu.dummy_read_pc(bus);
        cpu.jam = Some(0);
    }
}

macro_rules! branches {
    ($($name:ident => |$cpu:ident| $taken:expr;)*) => {$(
        impl Execute<Relative> for $name {
            fn execute<B: Bus>($cpu: &mut Cpu, bus: &mut B) {
                let taken = $taken;
                $cpu.branch(bus, taken);
            }
        }
    )*};
}

branches! {
    Bpl => |cpu| !cpu.n;
    Bmi => |cpu| cpu.n;
    Bvc => |cpu| !cpu.v;
    Bvs => |cpu| cpu.v;
    Bcc => |cpu| !cpu.c;
    Bcs => |cpu| cpu.c;
    Bne => |cpu| !cpu.z;
    Beq => |cpu| cpu.z;
}

impl Execute<Absolute> for Jmp {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        cpu.pc = cpu.fetch_word(bus);
    }
}

impl Execute<Indirect> for Jmp {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let pointer = cpu.fetch_word(bus);
        let lo = cpu.read(bus, pointer);
        // The high byte comes from the same page: the pointer's carry is lost.
        let hi = cpu.read(bus, (pointer & 0xFF00) | (pointer.wrapping_add(1) & 0x00FF));
        cpu.pc = u16::from_le_bytes([lo, hi]);
    }
}

impl Execute<Absolute> for Jsr {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let lo = cpu.fetch(bus);
        cpu.dummy_read_stack(bus);
        let [pcl, pch] = cpu.pc.to_le_bytes();
        cpu.push(bus, pch);
        cpu.push(bus, pcl);
        let hi = cpu.read(bus, cpu.pc);
        cpu.pc = u16::from_le_bytes([lo, hi]);
    }
}

impl Execute<AbsoluteY> for Sha {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let base = cpu.fetch_word(bus);
        let (index, value) = (cpu.y, cpu.a & cpu.x);
        cpu.store_and_high(bus, base, index, value);
    }
}

impl Execute<IndirectIndexed> for Sha {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let at = cpu.fetch(bus);
        let base = zero_page_pointer(cpu, bus, at);
        let (index, value) = (cpu.y, cpu.a & cpu.x);
        cpu.store_and_high(bus, base, index, value);
    }
}

impl Execute<AbsoluteY> for Shx {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let base = cpu.fetch_word(bus);
        let (index, value) = (cpu.y, cpu.x);
        cpu.store_and_high(bus, base, index, value);
    }
}

impl Execute<AbsoluteX> for Shy {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let base = cpu.fetch_word(bus);
        let (index, value) = (cpu.x, cpu.y);
        cpu.store_and_high(bus, base, index, value);
    }
}

impl Execute<AbsoluteY> for Tas {
    fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
        let base = cpu.fetch_word(bus);
        cpu.s = cpu.a & cpu.x;
        let (index, value) = (cpu.y, cpu.s);
        cpu.store_and_high(bus, base, index, value);
    }
}

impl Cpu {
    /// The `SHA`/`SHX`/`SHY`/`TAS` store: the value is masked with the base
    /// address's high byte plus one, and when indexing crosses a page that
    /// same value replaces the target's high byte.
    fn store_and_high<B: Bus>(&mut self, bus: &mut B, base: u16, index: u8, value: u8) {
        let (target, uncorrected) = indexed(base, index);
        self.read(bus, uncorrected);
        let [_, base_high] = base.to_le_bytes();
        let data = value & base_high.wrapping_add(1);
        let addr = if target == uncorrected {
            target
        } else {
            u16::from_le_bytes([target.to_le_bytes()[0], data])
        };
        self.write(bus, addr, data);
    }
}

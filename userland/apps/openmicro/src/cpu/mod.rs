//! The NMOS 6502, cycle by cycle.
//!
//! Every instruction performs its exact sequence of bus cycles through [`Bus`],
//! dummy accesses included, so a machine that times its devices by those
//! cycles sees what the silicon would. Interrupt lines are sampled at the end
//! of every cycle and acted on from the sample taken on an instruction's
//! penultimate cycle, which is what gives `CLI`, `SEI`, `PLP` and taken
//! branches their one-instruction delays.

pub mod disasm;
pub mod isa;
pub mod modes;
pub mod ops;
#[cfg(not(freestanding))]
pub mod vectors;

#[cfg(test)]
mod tests;

/// What the CPU sees of the machine around it.
pub trait Bus {
    /// Perform one bus cycle reading `addr`.
    fn read(&mut self, addr: u16) -> u8;
    /// Perform one bus cycle writing `data` to `addr`.
    fn write(&mut self, addr: u16, data: u8);
    /// Whether IRQ is asserted at the end of the cycle just performed.
    fn irq(&self) -> bool;
    /// Whether NMI is asserted at the end of the cycle just performed.
    fn nmi(&self) -> bool;
}

/// Bits of the processor status register.
pub mod flag {
    /// Carry.
    pub const C: u8 = 0x01;
    /// Zero.
    pub const Z: u8 = 0x02;
    /// Interrupt disable.
    pub const I: u8 = 0x04;
    /// Decimal mode.
    pub const D: u8 = 0x08;
    /// Break: set in the copy `BRK` and `PHP` push, clear in an interrupt's.
    pub const B: u8 = 0x10;
    /// Unused; always set in a pushed copy.
    pub const U: u8 = 0x20;
    /// Overflow.
    pub const V: u8 = 0x40;
    /// Negative.
    pub const N: u8 = 0x80;
}

/// How many bytes the processor can address.
pub const ADDRESS_SPACE: usize = 0x1_0000;

/// The vector an NMI takes.
pub const NMI_VECTOR: u16 = 0xFFFA;
/// The vector a reset takes.
pub const RESET_VECTOR: u16 = 0xFFFC;
/// The vector IRQ and `BRK` take.
pub const IRQ_VECTOR: u16 = 0xFFFE;

/// The page the stack occupies.
pub const STACK_PAGE: u16 = 0x0100;

/// The programmer-visible registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    /// The accumulator.
    pub a: u8,
    /// Index register X.
    pub x: u8,
    /// Index register Y.
    pub y: u8,
    /// The stack pointer, within page one.
    pub s: u8,
    /// The status register; `B` and `U` are not stored.
    pub p: u8,
    /// The program counter.
    pub pc: u16,
}

/// Everything the processor holds between cycles, as a snapshot records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent latch of the silicon"
)]
pub struct CpuState {
    /// The registers.
    pub registers: Registers,
    /// Halted by a `JAM` opcode until reset, with the idle cycles performed
    /// since.
    pub jam: Option<u8>,
    /// A reset sequence is to run before the next instruction.
    pub reset_pending: bool,
    /// An interrupt sequence is to run before the next instruction.
    pub interrupt_due: bool,
    /// NMI's level at the last sampled cycle.
    pub nmi_line: bool,
    /// An NMI edge seen and not yet serviced.
    pub nmi_latched: bool,
    /// The latched NMI as sampled at the last cycle and the one before.
    pub nmi_samples: [bool; 2],
    /// Unmasked IRQ as sampled at the last cycle and the one before.
    pub irq_samples: [bool; 2],
}

/// The processor.
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the status flags and interrupt latches are independent bits of the silicon"
)]
pub struct Cpu {
    a: u8,
    x: u8,
    y: u8,
    s: u8,
    pc: u16,
    n: bool,
    v: bool,
    d: bool,
    i: bool,
    z: bool,
    c: bool,
    jam: Option<u8>,
    reset_pending: bool,
    interrupt_due: bool,
    nmi_line: bool,
    nmi_latched: bool,
    nmi_now: bool,
    nmi_prev: bool,
    irq_now: bool,
    irq_prev: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A processor at power-on: registers clear, interrupts masked, and the
    /// reset sequence due before anything else runs.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            s: 0,
            pc: 0,
            n: false,
            v: false,
            d: false,
            i: true,
            z: false,
            c: false,
            jam: None,
            reset_pending: true,
            interrupt_due: false,
            nmi_line: false,
            nmi_latched: false,
            nmi_now: false,
            nmi_prev: false,
            irq_now: false,
            irq_prev: false,
        }
    }

    /// Run one instruction, or the reset or interrupt sequence due before it.
    /// A jammed processor performs one idle cycle.
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.reset_pending {
            self.reset_sequence(bus);
            return;
        }
        if let Some(idle) = self.jam {
            // A jammed bus reads &FFFF, &FFFE twice, then &FFFF for ever.
            let addr = if matches!(idle, 1 | 2) {
                0xFFFE
            } else {
                0xFFFF
            };
            self.jam = Some(idle.saturating_add(1));
            self.read(bus, addr);
            return;
        }
        if self.interrupt_due {
            self.dummy_read_pc(bus);
            self.dummy_read_pc(bus);
            self.interrupt(bus, false);
            // The handler's first instruction always runs before the next
            // interrupt is taken.
            self.interrupt_due = false;
            return;
        }
        let opcode = self.fetch(bus);
        isa::execute(self, bus, opcode);
        self.interrupt_due = self.nmi_prev || self.irq_prev;
    }

    /// Assert reset: the reset sequence runs at the next [`Cpu::step`],
    /// abandoning nothing, since a step never stops inside an instruction.
    pub fn reset(&mut self) {
        self.reset_pending = true;
    }

    /// The program counter.
    #[must_use]
    pub const fn pc(&self) -> u16 {
        self.pc
    }

    /// Whether a `JAM` opcode has halted the processor.
    #[must_use]
    pub const fn is_jammed(&self) -> bool {
        self.jam.is_some()
    }

    /// The registers.
    #[must_use]
    pub const fn registers(&self) -> Registers {
        Registers {
            a: self.a,
            x: self.x,
            y: self.y,
            s: self.s,
            p: self.status(),
            pc: self.pc,
        }
    }

    /// Replace the registers.
    pub fn set_registers(&mut self, registers: Registers) {
        self.a = registers.a;
        self.x = registers.x;
        self.y = registers.y;
        self.s = registers.s;
        self.pc = registers.pc;
        self.set_status(registers.p);
    }

    /// Everything the processor holds between cycles.
    #[must_use]
    pub const fn state(&self) -> CpuState {
        CpuState {
            registers: self.registers(),
            jam: self.jam,
            reset_pending: self.reset_pending,
            interrupt_due: self.interrupt_due,
            nmi_line: self.nmi_line,
            nmi_latched: self.nmi_latched,
            nmi_samples: [self.nmi_now, self.nmi_prev],
            irq_samples: [self.irq_now, self.irq_prev],
        }
    }

    /// Restore everything the processor holds between cycles.
    pub fn restore(&mut self, state: &CpuState) {
        self.set_registers(state.registers);
        self.jam = state.jam;
        self.reset_pending = state.reset_pending;
        self.interrupt_due = state.interrupt_due;
        self.nmi_line = state.nmi_line;
        self.nmi_latched = state.nmi_latched;
        [self.nmi_now, self.nmi_prev] = state.nmi_samples;
        [self.irq_now, self.irq_prev] = state.irq_samples;
    }

    /// The status register as it is pushed by an interrupt: `U` set, `B` clear.
    const fn status(&self) -> u8 {
        let mut p = flag::U;
        if self.n {
            p |= flag::N;
        }
        if self.v {
            p |= flag::V;
        }
        if self.d {
            p |= flag::D;
        }
        if self.i {
            p |= flag::I;
        }
        if self.z {
            p |= flag::Z;
        }
        if self.c {
            p |= flag::C;
        }
        p
    }

    fn set_status(&mut self, p: u8) {
        self.n = p & flag::N != 0;
        self.v = p & flag::V != 0;
        self.d = p & flag::D != 0;
        self.i = p & flag::I != 0;
        self.z = p & flag::Z != 0;
        self.c = p & flag::C != 0;
    }

    /// Latch the interrupt lines at the end of a cycle.
    fn sample<B: Bus>(&mut self, bus: &B) {
        let line = bus.nmi();
        self.nmi_latched |= line && !self.nmi_line;
        self.nmi_line = line;
        self.nmi_prev = self.nmi_now;
        self.nmi_now = self.nmi_latched;
        self.irq_prev = self.irq_now;
        self.irq_now = bus.irq() && !self.i;
    }

    fn read<B: Bus>(&mut self, bus: &mut B, addr: u16) -> u8 {
        let data = bus.read(addr);
        self.sample(bus);
        data
    }

    fn write<B: Bus>(&mut self, bus: &mut B, addr: u16, data: u8) {
        bus.write(addr, data);
        self.sample(bus);
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let data = self.read(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        data
    }

    fn fetch_word<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from_le_bytes([lo, hi])
    }

    /// The cycle an instruction with no operand spends reading the next byte.
    fn dummy_read_pc<B: Bus>(&mut self, bus: &mut B) {
        self.read(bus, self.pc);
    }

    /// The cycle a pull spends reading the stack before incrementing it.
    fn dummy_read_stack<B: Bus>(&mut self, bus: &mut B) {
        self.read(bus, STACK_PAGE | u16::from(self.s));
    }

    fn push<B: Bus>(&mut self, bus: &mut B, data: u8) {
        self.write(bus, STACK_PAGE | u16::from(self.s), data);
        self.s = self.s.wrapping_sub(1);
    }

    fn pull<B: Bus>(&mut self, bus: &mut B) -> u8 {
        self.s = self.s.wrapping_add(1);
        self.read(bus, STACK_PAGE | u16::from(self.s))
    }

    /// Push the return state and take a vector: the tail shared by `BRK`, IRQ
    /// and NMI. An NMI latched before the vector is fetched takes it over.
    fn interrupt<B: Bus>(&mut self, bus: &mut B, brk: bool) {
        let [pcl, pch] = self.pc.to_le_bytes();
        self.push(bus, pch);
        self.push(bus, pcl);
        let p = if brk {
            self.status() | flag::B
        } else {
            self.status()
        };
        self.push(bus, p);
        self.i = true;
        let vector = if self.nmi_latched {
            self.nmi_latched = false;
            NMI_VECTOR
        } else {
            IRQ_VECTOR
        };
        let lo = self.read(bus, vector);
        let hi = self.read(bus, vector.wrapping_add(1));
        self.pc = u16::from_le_bytes([lo, hi]);
    }

    /// The reset sequence: an interrupt whose stack writes are suppressed.
    fn reset_sequence<B: Bus>(&mut self, bus: &mut B) {
        self.dummy_read_pc(bus);
        self.dummy_read_pc(bus);
        for _ in 0..3 {
            self.dummy_read_stack(bus);
            self.s = self.s.wrapping_sub(1);
        }
        self.i = true;
        self.jam = None;
        self.reset_pending = false;
        self.interrupt_due = false;
        self.nmi_latched = false;
        let lo = self.read(bus, RESET_VECTOR);
        let hi = self.read(bus, RESET_VECTOR.wrapping_add(1));
        self.pc = u16::from_le_bytes([lo, hi]);
    }

    /// A conditional branch, after its opcode fetch.
    fn branch<B: Bus>(&mut self, bus: &mut B, taken: bool) {
        let offset = self.fetch(bus).cast_signed();
        if !taken {
            return;
        }
        // A taken branch that stays in its page does not see an interrupt that
        // first arrived on its operand cycle.
        if self.irq_now && !self.irq_prev {
            self.irq_now = false;
        }
        if self.nmi_now && !self.nmi_prev {
            self.nmi_now = false;
        }
        self.dummy_read_pc(bus);
        let target = self.pc.wrapping_add_signed(i16::from(offset));
        let uncorrected = (self.pc & 0xFF00) | (target & 0x00FF);
        if uncorrected != target {
            self.read(bus, uncorrected);
        }
        self.pc = target;
    }

    fn set_nz(&mut self, value: u8) {
        self.n = value & flag::N != 0;
        self.z = value == 0;
    }

    fn load_a(&mut self, value: u8) {
        self.a = value;
        self.set_nz(value);
    }

    fn compare(&mut self, register: u8, value: u8) {
        let (difference, borrow) = register.overflowing_sub(value);
        self.c = !borrow;
        self.set_nz(difference);
    }

    fn asl_value(&mut self, value: u8) -> u8 {
        self.c = value & 0x80 != 0;
        let result = value << 1;
        self.set_nz(result);
        result
    }

    fn lsr_value(&mut self, value: u8) -> u8 {
        self.c = value & 0x01 != 0;
        let result = value >> 1;
        self.set_nz(result);
        result
    }

    fn rol_value(&mut self, value: u8) -> u8 {
        let result = (value << 1) | u8::from(self.c);
        self.c = value & 0x80 != 0;
        self.set_nz(result);
        result
    }

    fn ror_value(&mut self, value: u8) -> u8 {
        let result = (value >> 1) | (u8::from(self.c) << 7);
        self.c = value & 0x01 != 0;
        self.set_nz(result);
        result
    }

    /// `ADC`, binary or NMOS decimal. Decimal mode follows the NMOS sequences
    /// of Bruce Clark's *Decimal Mode* tutorial: the accumulator and carry from
    /// the adjusted sum, N and V from the signed sum before the high digit is
    /// adjusted, and Z from the binary sum.
    fn adc(&mut self, value: u8) {
        let a = u16::from(self.a);
        let m = u16::from(value);
        let carry = u16::from(self.c);
        let binary = a + m + carry;
        if !self.d {
            let result = binary.to_le_bytes()[0];
            self.v = (!(self.a ^ value) & (self.a ^ result) & 0x80) != 0;
            self.c = binary > 0xFF;
            self.load_a(result);
            return;
        }
        let mut low = (a & 0x0F) + (m & 0x0F) + carry;
        if low >= 0x0A {
            low = ((low + 0x06) & 0x0F) + 0x10;
        }
        let signed = i16::from((self.a & 0xF0).cast_signed())
            + i16::from((value & 0xF0).cast_signed())
            + low.cast_signed();
        self.n = signed & 0x80 != 0;
        self.v = !(-128..=127).contains(&signed);
        self.z = binary.to_le_bytes()[0] == 0;
        let mut sum = (a & 0xF0) + (m & 0xF0) + low;
        if sum >= 0xA0 {
            sum += 0x60;
        }
        self.c = sum >= 0x100;
        self.a = sum.to_le_bytes()[0];
    }

    /// `SBC`, binary or NMOS decimal. NMOS decimal subtraction sets every flag
    /// from the binary difference and adjusts only the accumulator.
    fn sbc(&mut self, value: u8) {
        let borrow = i16::from(!self.c);
        let a = i16::from(self.a);
        let m = i16::from(value);
        let difference = a - m - borrow;
        let result = difference.to_le_bytes()[0];
        self.v = ((self.a ^ value) & (self.a ^ result) & 0x80) != 0;
        self.c = difference >= 0;
        self.set_nz(result);
        if !self.d {
            self.a = result;
            return;
        }
        let mut low = (a & 0x0F) - (m & 0x0F) - borrow;
        if low < 0 {
            low = ((low - 0x06) & 0x0F) - 0x10;
        }
        let mut full = (a & 0xF0) - (m & 0xF0) + low;
        if full < 0 {
            full -= 0x60;
        }
        self.a = full.to_le_bytes()[0];
    }

    /// `ARR`: AND, then rotate right, with flags of its own and, in decimal
    /// mode, a BCD correction of each digit.
    fn arr(&mut self, value: u8) {
        let anded = self.a & value;
        let rotated = (anded >> 1) | (u8::from(self.c) << 7);
        if !self.d {
            self.load_a(rotated);
            self.c = rotated & 0x40 != 0;
            self.v = ((rotated >> 6) ^ (rotated >> 5)) & 0x01 != 0;
            return;
        }
        self.set_nz(rotated);
        self.v = (anded ^ rotated) & 0x40 != 0;
        let mut result = rotated;
        if (anded & 0x0F) + (anded & 0x01) > 0x05 {
            result = (result & 0xF0) | (result.wrapping_add(0x06) & 0x0F);
        }
        self.c = u16::from(anded & 0xF0) + u16::from(anded & 0x10) > 0x50;
        if self.c {
            result = result.wrapping_add(0x60);
        }
        self.a = result;
    }
}

crate::snapshot::fields!(
    Registers { a, x, y, s, p, pc },
    check | r | r.p & (flag::B | flag::U) == flag::U
);
crate::snapshot::fields!(CpuState {
    registers,
    jam,
    reset_pending,
    interrupt_due,
    nmi_line,
    nmi_latched,
    nmi_samples,
    irq_samples,
});

//! What the CPU sees of the machine around it.

use super::Cpu;

/// What a read nothing answers returns: the data bus floats high.
pub const OPEN_BUS: u8 = 0xFF;

/// Why the processor enters an interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The code it runs: an INT, a fault, the single-step trap.
    Program,
    /// A pin: INTR or NMI.
    External,
}

/// An ESC instruction as the coprocessor takes it from the bus, the CPU
/// having computed its operand's address and made the dummy read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Escape {
    /// The eleven opcode bits: the ESC opcode's low three, then `ModRM`.
    pub opcode: u16,
    /// The physical address of the ESC opcode, any prefix not counted.
    pub address: u32,
    /// The memory operand's physical address, and the word the dummy read
    /// carried from it.
    pub operand: Option<(u32, u16)>,
}

/// The CPU's bus: memory, I/O ports, the interrupt pins and the ROM the
/// emulator owns.
///
/// Addresses are physical and already wrapped to 20 bits. `at` is the CPU's
/// cycle count when the access happens, so a device can catch up to it.
pub trait Bus {
    /// Reads a byte of memory.
    fn read(&mut self, addr: u32, at: u64) -> u8;

    /// Writes a byte of memory.
    fn write(&mut self, addr: u32, value: u8, at: u64);

    /// Reads a byte of code, as the bus status lines tell a code fetch from
    /// a read of data.
    fn fetch(&mut self, addr: u32, at: u64) -> u8 {
        self.read(addr, at)
    }

    /// The byte memory holds at `addr`, read without a bus cycle: no wait
    /// state and no device side effect. The prefetch queue's model uses it
    /// for bytes the BIU fetched in a cycle already counted.
    fn peek(&self, addr: u32) -> u8;

    /// Reads an I/O port.
    fn input(&mut self, port: u16, at: u64) -> u8;

    /// Writes an I/O port.
    fn output(&mut self, port: u16, value: u8, at: u64);

    /// Reads the I/O word at `port`. An 8-bit device sees the two byte
    /// cycles a word transfer is on its bus; a 16-bit device answers once.
    fn input_word(&mut self, port: u16, at: u64) -> u16 {
        let low = self.input(port, at);
        crate::bits::word(low, self.input(port.wrapping_add(1), at))
    }

    /// Writes the I/O word at `port`, as [`Bus::input_word`] reads one.
    fn output_word(&mut self, port: u16, value: u16, at: u64) {
        self.output(port, crate::bits::lo(value), at);
        self.output(port.wrapping_add(1), crate::bits::hi(value), at);
    }

    /// Whether the INTR pin is asserted.
    fn intr(&self) -> bool;

    /// Runs the interrupt-acknowledge cycles and returns the vector the
    /// interrupt controller drives.
    fn acknowledge(&mut self) -> u8;

    /// Whether an NMI edge is latched and waiting.
    fn nmi(&self) -> bool;

    /// Clears the latched NMI edge as the CPU takes it.
    fn acknowledge_nmi(&mut self);

    /// An IRET completed, which ends the 80186's DMA halt.
    fn interrupt_return(&mut self) {}

    /// Whether the code at `addr` is the emulator's own ROM, where the
    /// host-call encoding stops the CPU instead of executing.
    fn host_code(&self, addr: u32) -> bool;

    /// Whether ESC instructions trap to interrupt 7, as the 80186's
    /// relocation register asks when its ET bit is set.
    fn escape_traps(&self) -> bool {
        false
    }

    /// Hands a coprocessor the ESC instruction the CPU executed at cycle
    /// `at`. A bus with no coprocessor ignores it.
    fn escape(&mut self, _escape: Escape, _at: u64) {}

    /// The first cycle from `at` at which the TEST pin may read low, or
    /// `u64::MAX` where nothing on the bus will lower it. A bus with no
    /// coprocessor holds TEST low.
    fn test_low_at(&mut self, at: u64) -> u64 {
        at
    }

    /// Cycles the bus took from the CPU since the last call, up to cycle
    /// `at`: wait states, and bus cycles DMA used.
    fn take_wait_states(&mut self, _at: u64) -> u32 {
        0
    }

    /// Whether a device was reprogrammed since the caller of [`Cpu::run`]
    /// chose its cycle limit, so it may need to act before that limit. The
    /// CPU then returns for the limit to be chosen again.
    ///
    /// [`Cpu::run`]: super::Cpu::run
    fn schedule_changed(&self) -> bool {
        false
    }

    /// The cycle from which [`Bus::take_wait_states`] next has refresh to
    /// charge: before it, the call returns only the waits already taken.
    fn next_refresh(&self) -> u64 {
        u64::MAX
    }

    /// Adds `cycles` to the waits [`Bus::take_wait_states`] next returns:
    /// those of accesses a translated block made without the bus.
    fn steal(&mut self, cycles: u32);

    /// Whether [`Bus::take_wait_states`] has waits to return already, which
    /// the next instruction is charged: a translated block is entered only
    /// where there are none, as it charges only its own.
    fn waiting(&self) -> bool {
        false
    }

    /// `cpu` is about to begin an instruction, the interrupts due at this
    /// boundary taken, as the queue status lines tell an in-circuit
    /// emulator: whether it stops there instead ([`Stop::Break`]).
    ///
    /// [`Stop::Break`]: super::Stop::Break
    fn begins(&mut self, _cpu: &Cpu) -> bool {
        false
    }

    /// `cpu` enters interrupt `vector` for `cause`, as it stands before the
    /// entry.
    fn interrupts(&mut self, _vector: u8, _cpu: &Cpu, _cause: Cause) {}

    /// Whether something watches each instruction begin, which a translated
    /// block runs without.
    fn watches_instructions(&self) -> bool {
        false
    }
}

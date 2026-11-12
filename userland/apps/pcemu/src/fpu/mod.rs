//! The 8087 numeric data processor (`plans/PCEMU.md` §1.8).
//!
//! The chip sits beside the CPU on the local bus. The CPU decodes an ESC,
//! makes the dummy read of its operand and hands the instruction over
//! ([`Fpu::escape`]); the 8087 reads the rest of its operand as bus master
//! at once, and its numeric execution unit computes the instruction then,
//! but commits it — registers, status, the memory it stores, the interrupt
//! it raises — only when the instruction ends ([`Fpu::complete`]), the
//! board treating that instant as an event. The control unit's
//! instructions act at once.

pub mod arith;
pub(crate) mod exec;
pub mod real;
mod timing;
mod trans;

use crate::cpu::bus::Escape;
use crate::snapshot::{state, In, Out, State, Torn};
use exec::Insn;
use real::Real;

/// The two-bit tags the tag word holds for each physical register.
pub mod tag {
    /// A finite nonzero value: normal, unnormal or pseudo-zero.
    pub const VALID: u16 = 0;
    /// A true zero.
    pub const ZERO: u16 = 1;
    /// An infinity, NaN or denormal.
    pub const SPECIAL: u16 = 2;
    /// Nothing.
    pub const EMPTY: u16 = 3;
}

/// The status word's bits the chip derives rather than stores.
mod derived {
    /// IR: an unmasked exception is flagged.
    pub const IR: u16 = 0x0080;
    /// B: busy.
    pub const BUSY: u16 = 0x8000;
}

/// The control word as FINIT leaves it: every exception and the interrupt
/// masked, 64-bit precision, round to nearest, projective infinity.
pub const CONTROL_INIT: u16 = 0x03FF;
/// The control word's interrupt enable mask.
pub const IEM: u16 = 0x0080;
/// The bytes FSAVE stores.
pub const STATE_BYTES: usize = 94;

/// The 8087's state, all FSAVE stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    /// The physical data registers.
    pub data: [Real; 8],
    /// Each physical register's tag, register 0's in the low two bits.
    pub tags: u16,
    /// The control word.
    pub control: u16,
    /// The status word but for IR and B: the flags, condition codes and TOP.
    pub status: u16,
    /// The last numeric instruction's physical address.
    pub ip: u32,
    /// Its eleven opcode bits.
    pub opcode: u16,
    /// The physical address of the last memory operand.
    pub dp: u32,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            data: [Real::ZERO; 8],
            tags: 0xFFFF,
            control: CONTROL_INIT,
            status: 0,
            ip: 0,
            opcode: 0,
            dp: 0,
        }
    }
}

impl Registers {
    /// Whether an unmasked exception is flagged.
    #[must_use]
    pub const fn interrupt_request(&self) -> bool {
        self.status & !self.control & arith::flag::ALL != 0
    }

    /// The register at the top of the stack.
    fn top(&self) -> u8 {
        crate::bits::lo((self.status >> 11) & 7)
    }

    fn set_top(&mut self, top: u8) {
        self.status = (self.status & !0x3800) | (u16::from(top & 7) << 11);
    }

    /// The physical register ST(`i`) is.
    pub(crate) fn physical(&self, i: u8) -> usize {
        usize::from(self.top().wrapping_add(i) & 7)
    }

    /// The tag of physical register `physical`: valid, zero, special or
    /// empty.
    pub(crate) fn tag(&self, physical: usize) -> u16 {
        (self.tags >> (2 * physical)) & 3
    }

    fn set_tag(&mut self, physical: usize, tag: u16) {
        let shift = 2 * physical;
        self.tags = (self.tags & !(3 << shift)) | ((tag & 3) << shift);
    }

    /// FINIT's state: control word initialised, flags clear, TOP zero,
    /// every register empty. The 8087 keeps the condition codes, the data
    /// and the exception pointers.
    fn initialise(&mut self) {
        self.control = CONTROL_INIT;
        self.status &= 0x4700;
        self.tags = 0xFFFF;
    }
}

/// A store the 8087 makes as bus master.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Store {
    /// Where the first byte goes.
    pub addr: u32,
    /// How many bytes.
    pub len: u8,
    /// The bytes.
    pub bytes: [u8; STATE_BYTES],
}

impl Default for Store {
    fn default() -> Self {
        Self {
            addr: 0,
            len: 0,
            bytes: [0; STATE_BYTES],
        }
    }
}

impl Store {
    fn new(addr: u32, data: &[u8]) -> Self {
        let mut store = Self {
            addr,
            len: u8::try_from(data.len().min(STATE_BYTES)).unwrap_or(0),
            ..Self::default()
        };
        let n = usize::from(store.len);
        store.bytes[..n].copy_from_slice(&data[..n]);
        store
    }

    /// The bytes stored.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// The memory the 8087 reads and writes as bus master, its 20-bit addresses
/// wrapping.
pub trait Memory {
    /// Reads the byte at `addr`.
    fn read(&mut self, addr: u32) -> u8;
    /// Writes the byte at `addr`.
    fn write(&mut self, addr: u32, value: u8);
}

/// The numeric execution unit's instruction in flight: when it ends and
/// what it then commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Flight {
    /// The CPU cycle it ends at.
    ends: u64,
    /// The state it leaves.
    next: Registers,
    /// The exception flags it raised, which survive an FNCLEX made while it
    /// runs.
    raised: u16,
    /// What it stores.
    store: Option<Store>,
}

/// An 8087.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fpu {
    now: Registers,
    flight: Option<Flight>,
}

impl Fpu {
    /// An 8087 as RESET leaves it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// RESET: the instruction in flight abandoned, and FINIT's state.
    pub fn reset(&mut self) {
        self.flight = None;
        self.now.initialise();
    }

    /// The state committed so far.
    #[must_use]
    pub const fn registers(&self) -> &Registers {
        &self.now
    }

    /// The status word as FSTSW reads it.
    #[must_use]
    pub fn status(&self) -> u16 {
        let mut status = self.now.status & !(derived::IR | derived::BUSY);
        if self.now.interrupt_request() {
            status |= derived::IR;
        }
        if self.busy() {
            status |= derived::BUSY;
        }
        status
    }

    /// Whether the INT pin is raised: an unmasked exception, interrupts
    /// enabled.
    #[must_use]
    pub const fn interrupting(&self) -> bool {
        self.now.interrupt_request() && self.now.control & IEM == 0
    }

    /// Whether BUSY is raised: an instruction in flight, or an interrupt
    /// waiting to be cleared.
    #[must_use]
    pub const fn busy(&self) -> bool {
        self.flight.is_some() || self.interrupting()
    }

    /// When the instruction in flight ends, if one is.
    #[must_use]
    pub fn ends(&self) -> Option<u64> {
        self.flight.map(|flight| flight.ends)
    }

    /// The first cycle from `at` at which BUSY may read low: `at` where it
    /// is low now, the end of the instruction in flight, or never while an
    /// interrupt holds it.
    #[must_use]
    pub fn ready_at(&self, at: u64) -> u64 {
        match self.flight {
            Some(flight) => flight.ends.max(at),
            None if self.interrupting() => u64::MAX,
            None => at,
        }
    }

    /// Commits the instruction in flight, whatever its end: its state, and
    /// its store through `memory`.
    pub fn complete<M: Memory>(&mut self, memory: &mut M) {
        let Some(flight) = self.flight.take() else {
            return;
        };
        self.now = flight.next;
        if let Some(store) = flight.store {
            let mut addr = store.addr;
            for &byte in store.data() {
                memory.write(addr, byte);
                addr = (addr + 1) & 0xF_FFFF;
            }
        }
    }

    /// Takes ESC instruction `escape`, issued at CPU cycle `at`, reading
    /// its operand's remaining bytes through `memory`.
    pub fn escape<M: Memory>(&mut self, escape: Escape, at: u64, memory: &mut M) {
        let insn = exec::decode(escape.opcode, escape.operand.is_some());
        if insn.control_unit() {
            self.control(insn, escape, memory);
            return;
        }
        // The execution unit runs one instruction at a time: one issued
        // before the last has ended ends it early.
        self.complete(memory);
        let mut operand = [0; STATE_BYTES];
        let loaded = insn.loads().map_or(0, exec::Format::bytes);
        if let Some((addr, word)) = escape.operand {
            let [first, second] = word.to_le_bytes();
            let mut next = (addr + 2) & 0xF_FFFF;
            for (n, byte) in operand.iter_mut().enumerate().take(loaded) {
                *byte = match n {
                    0 => first,
                    1 => second,
                    _ => {
                        let byte = memory.read(next);
                        next = (next + 1) & 0xF_FFFF;
                        byte
                    }
                };
            }
        }
        let mut next = self.now;
        if insn.numeric() {
            next.ip = escape.address;
            next.opcode = escape.opcode;
            if let Some((addr, _)) = escape.operand {
                next.dp = addr;
            }
        }
        let address = escape.operand.map(|(addr, _)| addr);
        let done = exec::execute(insn, &mut next, &operand[..loaded], address);
        self.flight = Some(Flight {
            ends: at + u64::from(done.clocks),
            next,
            raised: done.raised,
            store: done.store,
        });
    }

    /// A control-unit instruction: it acts at once, alongside any
    /// instruction in flight, and holds no BUSY.
    fn control<M: Memory>(&mut self, insn: Insn, escape: Escape, memory: &mut M) {
        let flight = self.flight.as_mut();
        match insn {
            Insn::Initialise => {
                self.flight = None;
                self.now.initialise();
            }
            Insn::ClearExceptions => {
                self.now.status &= !arith::flag::ALL;
                if let Some(flight) = flight {
                    flight.next.status = (flight.next.status & !arith::flag::ALL)
                        | (flight.raised & arith::flag::ALL);
                }
            }
            Insn::EnableInterrupts | Insn::DisableInterrupts => {
                let set = |control: &mut u16| {
                    if insn == Insn::EnableInterrupts {
                        *control &= !IEM;
                    } else {
                        *control |= IEM;
                    }
                };
                set(&mut self.now.control);
                if let Some(flight) = flight {
                    set(&mut flight.next.control);
                }
            }
            Insn::StoreControl | Insn::StoreStatus => {
                let word = if insn == Insn::StoreControl {
                    self.now.control
                } else {
                    self.status()
                };
                if let Some((addr, _)) = escape.operand {
                    let [low, high] = word.to_le_bytes();
                    memory.write(addr, low);
                    memory.write((addr + 1) & 0xF_FFFF, high);
                }
            }
            _ => {}
        }
    }
}

impl State for Real {
    fn save(&self, out: &mut Out) {
        self.sign_exp.save(out);
        self.sig.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.sign_exp.restore(input)?;
        self.sig.restore(input)
    }
}

impl State for Store {
    fn save(&self, out: &mut Out) {
        self.addr.save(out);
        self.len.save(out);
        crate::snapshot::save_bytes(&self.bytes, out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.addr.restore(input)?;
        self.len.restore(input)?;
        crate::snapshot::restore_bytes(&mut self.bytes, input)?;
        crate::snapshot::check(usize::from(self.len) <= STATE_BYTES && self.addr <= 0xF_FFFF)
    }
}

state!(Registers {
    data,
    tags,
    control,
    status,
    ip,
    opcode,
    dp
});

state!(Flight {
    ends,
    next,
    raised,
    store
});

state!(Fpu { now, flight });

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

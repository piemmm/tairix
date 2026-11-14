//! Single-instruction conformance vectors, and the runner that holds the CPU
//! to one.
//!
//! A vector is one case of the `SingleStepTests` 65x02 suite (MIT; its notice is
//! kept beside the committed sample): registers and memory before and after
//! one instruction, and every bus cycle it performs. The compact encoding here
//! is what `cargo xtask openmicro --vectors <dir> --write-sample` writes and the
//! crate's tests read, so the sample and the full suite are checked by the one
//! runner.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use super::{flag, Bus, Cpu, Registers, ADDRESS_SPACE};

/// The encoding's magic number and version.
pub const MAGIC: [u8; 8] = *b"BEEBSST1";

/// One bus cycle a vector records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cycle {
    /// The address on the bus.
    pub addr: u16,
    /// The byte on the data bus.
    pub data: u8,
    /// Whether the CPU drove it.
    pub write: bool,
}

/// Processor registers and the memory a vector names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The registers.
    pub registers: Registers,
    /// The addressed bytes, in the order the suite lists them.
    pub ram: Vec<(u16, u8)>,
}

/// One case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vector {
    /// The state the instruction starts from.
    pub before: Snapshot,
    /// The state it must leave.
    pub after: Snapshot,
    /// The cycles it must perform.
    pub cycles: Vec<Cycle>,
}

/// Why an encoded sample could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The magic number is not this encoding's.
    BadMagic,
    /// The bytes end inside a vector.
    Truncated,
    /// A cycle is neither a read nor a write.
    BadCycleKind,
}

/// A vector too large for the encoding: more than 255 addresses or cycles,
/// which no single instruction touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong;

/// How a run departed from its vector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// The cycle at this index differs, or one side has no such cycle.
    Cycle {
        /// Which cycle.
        index: usize,
        /// The vector's.
        expected: Option<Cycle>,
        /// The CPU's.
        actual: Option<Cycle>,
    },
    /// The registers afterwards differ.
    Registers {
        /// The vector's.
        expected: Registers,
        /// The CPU's.
        actual: Registers,
    },
    /// A byte of memory afterwards differs.
    Memory {
        /// Where.
        addr: u16,
        /// The vector's.
        expected: u8,
        /// The CPU's.
        actual: u8,
    },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle {
                index,
                expected,
                actual,
            } => write!(f, "cycle {index}: expected {expected:?}, got {actual:?}"),
            Self::Registers { expected, actual } => {
                write!(f, "registers: expected {expected:?}, got {actual:?}")
            }
            Self::Memory {
                addr,
                expected,
                actual,
            } => write!(
                f,
                "memory &{addr:04X}: expected &{expected:02X}, got &{actual:02X}"
            ),
        }
    }
}

/// The status bits a register comparison ignores: `B` and `U` are not stored.
const UNSTORED: u8 = flag::B | flag::U;

/// A flat memory that records every cycle, with both interrupt lines idle.
struct RecordingBus {
    memory: Box<[u8]>,
    cycles: Vec<Cycle>,
}

impl Bus for RecordingBus {
    fn read(&mut self, addr: u16) -> u8 {
        let data = self.memory[usize::from(addr)];
        self.cycles.push(Cycle {
            addr,
            data,
            write: false,
        });
        data
    }

    fn write(&mut self, addr: u16, data: u8) {
        self.memory[usize::from(addr)] = data;
        self.cycles.push(Cycle {
            addr,
            data,
            write: true,
        });
    }

    fn irq(&self) -> bool {
        false
    }

    fn nmi(&self) -> bool {
        false
    }
}

/// Runs vectors one after another over one memory, restoring only what each
/// touched, so a suite of millions costs no more than its cycles.
pub struct Runner {
    bus: RecordingBus,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    /// A runner over zeroed memory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bus: RecordingBus {
                memory: alloc::vec![0; ADDRESS_SPACE].into_boxed_slice(),
                cycles: Vec::new(),
            },
        }
    }

    /// Run `vector`'s instruction and compare everything it records.
    pub fn check(&mut self, vector: &Vector) -> Result<(), Mismatch> {
        self.bus.cycles.clear();
        for &(addr, data) in &vector.before.ram {
            self.bus.memory[usize::from(addr)] = data;
        }
        let mut cpu = Cpu::new();
        cpu.reset_pending = false;
        cpu.set_registers(vector.before.registers);
        cpu.step(&mut self.bus);
        // A jammed processor's idle cycles belong to the halting instruction.
        while cpu.is_jammed() && self.bus.cycles.len() < vector.cycles.len() {
            cpu.step(&mut self.bus);
        }
        let outcome = compare(vector, &cpu, &self.bus);
        for &(addr, _) in &vector.before.ram {
            self.bus.memory[usize::from(addr)] = 0;
        }
        for cycle in &self.bus.cycles {
            self.bus.memory[usize::from(cycle.addr)] = 0;
        }
        outcome
    }
}

fn compare(vector: &Vector, cpu: &Cpu, bus: &RecordingBus) -> Result<(), Mismatch> {
    for index in 0..bus.cycles.len().max(vector.cycles.len()) {
        let expected = vector.cycles.get(index).copied();
        let actual = bus.cycles.get(index).copied();
        if expected != actual {
            return Err(Mismatch::Cycle {
                index,
                expected,
                actual,
            });
        }
    }
    let mut expected = vector.after.registers;
    let mut actual = cpu.registers();
    expected.p &= !UNSTORED;
    actual.p &= !UNSTORED;
    if expected != actual {
        return Err(Mismatch::Registers { expected, actual });
    }
    for &(addr, expected) in &vector.after.ram {
        let actual = bus.memory[usize::from(addr)];
        if actual != expected {
            return Err(Mismatch::Memory {
                addr,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

/// Encode vectors in the committed sample's format.
pub fn encode(vectors: &[Vector]) -> Result<Vec<u8>, TooLong> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    for vector in vectors {
        encode_snapshot(&mut out, &vector.before)?;
        encode_snapshot(&mut out, &vector.after)?;
        out.push(u8::try_from(vector.cycles.len()).map_err(|_| TooLong)?);
        for cycle in &vector.cycles {
            out.extend_from_slice(&cycle.addr.to_le_bytes());
            out.push(cycle.data);
            out.push(u8::from(cycle.write));
        }
    }
    Ok(out)
}

fn encode_snapshot(out: &mut Vec<u8>, snapshot: &Snapshot) -> Result<(), TooLong> {
    let r = snapshot.registers;
    out.extend_from_slice(&r.pc.to_le_bytes());
    out.extend_from_slice(&[r.s, r.a, r.x, r.y, r.p]);
    out.push(u8::try_from(snapshot.ram.len()).map_err(|_| TooLong)?);
    for &(addr, data) in &snapshot.ram {
        out.extend_from_slice(&addr.to_le_bytes());
        out.push(data);
    }
    Ok(())
}

/// Decode a committed sample.
pub fn decode(bytes: &[u8]) -> Result<Vec<Vector>, DecodeError> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(MAGIC.len())? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let mut vectors = Vec::new();
    while reader.at < bytes.len() {
        let before = reader.snapshot()?;
        let after = reader.snapshot()?;
        let count = reader.byte()?;
        let mut cycles = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let addr = reader.word()?;
            let data = reader.byte()?;
            let write = match reader.byte()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::BadCycleKind),
            };
            cycles.push(Cycle { addr, data, write });
        }
        vectors.push(Vector {
            before,
            after,
            cycles,
        });
    }
    Ok(vectors)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at.checked_add(len).ok_or(DecodeError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(DecodeError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn word(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn snapshot(&mut self) -> Result<Snapshot, DecodeError> {
        let pc = self.word()?;
        let registers = Registers {
            pc,
            s: self.byte()?,
            a: self.byte()?,
            x: self.byte()?,
            y: self.byte()?,
            p: self.byte()?,
        };
        let count = self.byte()?;
        let mut ram = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let addr = self.word()?;
            ram.push((addr, self.byte()?));
        }
        Ok(Snapshot { registers, ram })
    }
}

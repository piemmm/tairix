//! The hardware-captured CPU conformance vectors: their compact record
//! format, written by `cargo xtask pcemu-vectors` from the `SingleStepTests`
//! suites and read by the CPU's tests.
//!
//! Each vector is one instruction executed on a physical 8088, 8086 or V20:
//! its bytes, the registers, queue and memory before, and the registers and
//! memory it changed. Registers are in the upstream order (AX, BX, CX, DX,
//! CS, SS, DS, ES, SP, BP, SI, DI, IP, FLAGS).

use alloc::string::String;
use alloc::vec::Vec;

/// The file's leading magic and format version.
pub const MAGIC: [u8; 8] = *b"PCVEC\x00\x00\x01";

/// The registers a vector names, in the upstream order.
pub const REGISTERS: usize = 14;

/// How the upstream metadata classes an opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// A documented instruction.
    Normal,
    /// A duplicate of another opcode through imprecise decoding.
    Alias,
    /// Well-defined undocumented behaviour.
    Undocumented,
    /// Erratic undocumented behaviour.
    Undefined,
    /// A coprocessor escape.
    Fpu,
    /// A NEC extended operation.
    Extension,
}

impl Status {
    /// The byte the format stores.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Alias => 1,
            Self::Undocumented => 2,
            Self::Undefined => 3,
            Self::Fpu => 4,
            Self::Extension => 5,
        }
    }

    /// The status a stored byte names.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Normal,
            1 => Self::Alias,
            2 => Self::Undocumented,
            3 => Self::Undefined,
            4 => Self::Fpu,
            5 => Self::Extension,
            _ => return None,
        })
    }

    /// The status an upstream metadata string names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "normal" => Self::Normal,
            "alias" => Self::Alias,
            "undocumented" => Self::Undocumented,
            "undefined" => Self::Undefined,
            "fpu" => Self::Fpu,
            "extension" => Self::Extension,
            _ => return None,
        })
    }
}

/// One captured instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vector {
    /// The instruction's bytes, prefixes included.
    pub bytes: Vec<u8>,
    /// Every register before.
    pub regs: [u16; REGISTERS],
    /// The prefetch queue's bytes before; empty when it started empty.
    pub queue: Vec<u8>,
    /// Memory before: physical address and byte.
    pub ram: Vec<(u32, u8)>,
    /// Registers after; `None` where the instruction left one unchanged.
    pub final_regs: [Option<u16>; REGISTERS],
    /// Memory after, for every address the vector names.
    pub final_ram: Vec<(u32, u8)>,
    /// The bus cycles the instruction took on the hardware.
    pub cycles: u16,
}

/// The vectors for one opcode (or opcode and `ModRM` `reg` field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// The upstream file stem: `"00"`, `"D0.6"`, `"0F10"`.
    pub name: String,
    /// The opcode's class.
    pub status: Status,
    /// FLAGS bits the upstream calls undefined are clear in this mask.
    pub flags_mask: u16,
    /// The vectors.
    pub vectors: Vec<Vector>,
}

/// A suite for one processor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suite {
    /// The processor captured, as the upstream names it.
    pub cpu: String,
    /// Its opcode groups.
    pub groups: Vec<Group>,
}

/// Why a vector file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The magic or version is not this format's.
    BadMagic,
    /// The file ends inside a record.
    Truncated,
    /// A status byte names no status.
    BadStatus,
    /// A name is not UTF-8.
    BadName,
    /// Bytes follow the last group.
    Trailing,
}

/// Encodes a suite.
///
/// # Errors
///
/// A field too long for its length prefix.
pub fn encode(suite: &Suite) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    put_str(&mut out, &suite.cpu)?;
    put_u16(&mut out, suite.groups.len())?;
    for group in &suite.groups {
        put_str(&mut out, &group.name)?;
        out.push(group.status.code());
        out.extend_from_slice(&group.flags_mask.to_le_bytes());
        put_u16(&mut out, group.vectors.len())?;
        for v in &group.vectors {
            put_bytes(&mut out, &v.bytes)?;
            for r in v.regs {
                out.extend_from_slice(&r.to_le_bytes());
            }
            put_bytes(&mut out, &v.queue)?;
            put_ram(&mut out, &v.ram)?;
            let mask = v
                .final_regs
                .iter()
                .enumerate()
                .filter(|(_, r)| r.is_some())
                .fold(0u16, |m, (i, _)| m | (1 << i));
            out.extend_from_slice(&mask.to_le_bytes());
            for r in v.final_regs.iter().flatten() {
                out.extend_from_slice(&r.to_le_bytes());
            }
            put_ram(&mut out, &v.final_ram)?;
            out.extend_from_slice(&v.cycles.to_le_bytes());
        }
    }
    Ok(out)
}

fn put_u16(out: &mut Vec<u8>, n: usize) -> Result<(), &'static str> {
    let n = u16::try_from(n).map_err(|_| "count over 65535")?;
    out.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), &'static str> {
    out.push(u8::try_from(bytes.len()).map_err(|_| "byte run over 255")?);
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_str(out: &mut Vec<u8>, s: &str) -> Result<(), &'static str> {
    put_bytes(out, s.as_bytes())
}

fn put_ram(out: &mut Vec<u8>, ram: &[(u32, u8)]) -> Result<(), &'static str> {
    put_u16(out, ram.len())?;
    for &(addr, value) in ram {
        out.extend_from_slice(&addr.to_le_bytes());
        out.push(value);
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn run(&mut self) -> Result<Vec<u8>, Error> {
        let n = usize::from(self.u8()?);
        Ok(self.take(n)?.to_vec())
    }

    fn name(&mut self) -> Result<String, Error> {
        String::from_utf8(self.run()?).map_err(|_| Error::BadName)
    }

    fn ram(&mut self) -> Result<Vec<(u32, u8)>, Error> {
        let n = usize::from(self.u16()?);
        let mut ram = Vec::with_capacity(n);
        for _ in 0..n {
            let addr = self.u32()?;
            ram.push((addr, self.u8()?));
        }
        Ok(ram)
    }
}

/// Decodes a suite, refusing anything that is not exactly this format.
///
/// # Errors
///
/// [`Error`] naming the first defect.
pub fn decode(bytes: &[u8]) -> Result<Suite, Error> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return Err(Error::BadMagic);
    }
    let cpu = r.name()?;
    let groups_len = r.u16()?;
    let mut groups = Vec::with_capacity(usize::from(groups_len));
    for _ in 0..groups_len {
        let name = r.name()?;
        let status = Status::from_code(r.u8()?).ok_or(Error::BadStatus)?;
        let flags_mask = r.u16()?;
        let count = r.u16()?;
        let mut vectors = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let insn = r.run()?;
            let mut regs = [0u16; REGISTERS];
            for reg in &mut regs {
                *reg = r.u16()?;
            }
            let queue = r.run()?;
            let ram = r.ram()?;
            let mask = r.u16()?;
            let mut final_regs = [None; REGISTERS];
            for (i, slot) in final_regs.iter_mut().enumerate() {
                if mask & (1 << i) != 0 {
                    *slot = Some(r.u16()?);
                }
            }
            let final_ram = r.ram()?;
            let cycles = r.u16()?;
            vectors.push(Vector {
                bytes: insn,
                regs,
                queue,
                ram,
                final_regs,
                final_ram,
                cycles,
            });
        }
        groups.push(Group {
            name,
            status,
            flags_mask,
            vectors,
        });
    }
    if r.at != bytes.len() {
        return Err(Error::Trailing);
    }
    Ok(Suite { cpu, groups })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample() -> Suite {
        Suite {
            cpu: String::from("8088"),
            groups: vec![Group {
                name: String::from("D0.6"),
                status: Status::Undocumented,
                flags_mask: 0xF7FF,
                vectors: vec![Vector {
                    bytes: vec![0xD0, 0xF6],
                    regs: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 0xF002],
                    queue: vec![0xD0, 0xF6, 0x90, 0x90],
                    ram: vec![(0x0_0010, 0xD0), (0x0_0011, 0xF6)],
                    final_regs: [
                        None,
                        None,
                        None,
                        Some(0xFF04),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(15),
                        Some(0xF086),
                    ],
                    final_ram: vec![],
                    cycles: 8,
                }],
            }],
        }
    }

    #[test]
    fn a_suite_round_trips() {
        let suite = sample();
        let bytes = encode(&suite).expect("encodes");
        assert_eq!(decode(&bytes), Ok(suite));
    }

    #[test]
    fn every_truncation_is_refused() {
        let bytes = encode(&sample()).expect("encodes");
        for len in 0..bytes.len() {
            assert!(
                decode(&bytes[..len]).is_err(),
                "prefix of {len} bytes accepted"
            );
        }
    }

    #[test]
    fn trailing_bytes_and_bad_magic_are_refused() {
        let mut bytes = encode(&sample()).expect("encodes");
        bytes.push(0);
        assert_eq!(decode(&bytes), Err(Error::Trailing));
        assert_eq!(decode(b"PCVEC\0\0\x02"), Err(Error::BadMagic));
    }
}

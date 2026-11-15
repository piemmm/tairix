//! Single-instruction conformance vectors, and the runner that holds a
//! processor to one (`plans/RISCOS-EMULATOR.md` §2.6).
//!
//! A vector is the registers before and after one instruction and the data
//! transfers it makes in between. `cargo xtask riscos-vectors` writes this
//! encoding from a public vector set and the crate's tests read it back, so
//! the committed sample and the full set are checked by the one runner.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::bus::{Abort, Bus, Fetch, Width};
use crate::cpu::{Cpu, Exception, State, Stop};
use crate::profile::Profile;

/// The encoding's magic number and version.
pub const MAGIC: [u8; 8] = *b"ARMSST01";

/// The words of a [`State`], in the order the encoding stores them.
const STATE_WORDS: usize = 37;

/// One data transfer a vector records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    /// Whether the processor stored.
    pub write: bool,
    /// The width.
    pub width: Width,
    /// The address, aligned to the width.
    pub address: u32,
    /// The value, in the width's low bits.
    pub data: u32,
}

/// One case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vector {
    /// The instruction: an A32 word, or in Thumb state its halfwords in the
    /// order they are fetched, the first in the low half.
    pub word: u32,
    /// The registers it starts from; the PC is the instruction's address.
    pub before: State,
    /// The registers it must leave.
    pub after: State,
    /// The CPSR and SPSR bits the result is held to: the rest the
    /// architecture leaves to the implementation.
    pub psr: u32,
    /// The transfers it must make, in order.
    pub transfers: Vec<Transfer>,
}

/// Why an encoded sample could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The magic number is not this encoding's.
    BadMagic,
    /// The bytes end inside a vector, or run past the last.
    Truncated,
    /// A transfer's width is not a byte, halfword or word.
    BadWidth,
}

/// A vector with more transfers than the encoding counts: more than 255,
/// which no single instruction makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong;

/// How a run departed from its vector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// The transfer at this index differs, or one side has none there.
    Transfer {
        /// Which transfer.
        index: usize,
        /// The vector's.
        expected: Option<Transfer>,
        /// The processor's.
        actual: Option<Transfer>,
    },
    /// The registers afterwards differ, under the vector's PSR bits.
    Registers {
        /// The vector's.
        expected: Box<State>,
        /// The processor's.
        actual: Box<State>,
    },
    /// The processor stopped rather than executing the instruction.
    Stopped(Stop),
}

fn words(state: &State) -> [u32; STATE_WORDS] {
    let mut words = [0; STATE_WORDS];
    let (user, rest) = words.split_at_mut(15);
    user.copy_from_slice(&state.user);
    let (fiq, rest) = rest.split_at_mut(7);
    fiq.copy_from_slice(&state.fiq);
    let (banked, rest) = rest.split_at_mut(8);
    for (pair, slot) in state.banked.iter().zip(banked.as_chunks_mut::<2>().0) {
        *slot = *pair;
    }
    rest[0] = state.pc;
    rest[1] = state.cpsr;
    rest[2..].copy_from_slice(&state.spsr);
    words
}

fn state(words: &[u32; STATE_WORDS]) -> State {
    let mut state = State::default();
    state.user.copy_from_slice(&words[..15]);
    state.fiq.copy_from_slice(&words[15..22]);
    for (pair, slot) in state
        .banked
        .iter_mut()
        .zip(words[22..30].as_chunks::<2>().0)
    {
        *pair = *slot;
    }
    state.pc = words[30];
    state.cpsr = words[31];
    state.spsr.copy_from_slice(&words[32..]);
    state
}

const fn width_code(width: Width) -> u8 {
    match width {
        Width::Byte => 0,
        Width::Half => 1,
        Width::Word => 2,
    }
}

/// The encoding of `vectors`.
pub fn encode(vectors: &[Vector]) -> Result<Vec<u8>, TooLong> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(
        &u32::try_from(vectors.len())
            .map_err(|_| TooLong)?
            .to_le_bytes(),
    );
    for vector in vectors {
        out.extend_from_slice(&vector.word.to_le_bytes());
        out.extend_from_slice(&vector.psr.to_le_bytes());
        let before = words(&vector.before);
        let after = words(&vector.after);
        for word in before {
            out.extend_from_slice(&word.to_le_bytes());
        }
        let changed = (0..STATE_WORDS)
            .filter(|&n| before[n] != after[n])
            .fold(0u64, |mask, n| mask | 1 << n);
        out.extend_from_slice(&changed.to_le_bytes());
        for (n, word) in after.iter().enumerate() {
            if changed & (1 << n) != 0 {
                out.extend_from_slice(&word.to_le_bytes());
            }
        }
        out.push(u8::try_from(vector.transfers.len()).map_err(|_| TooLong)?);
        for transfer in &vector.transfers {
            out.push(u8::from(transfer.write) | width_code(transfer.width) << 1);
            out.extend_from_slice(&transfer.address.to_le_bytes());
            out.extend_from_slice(&transfer.data.to_le_bytes());
        }
    }
    Ok(out)
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(DecodeError::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.bytes().map(u32::from_le_bytes)
    }
}

/// The vectors `bytes` encodes.
pub fn decode(bytes: &[u8]) -> Result<Vec<Vector>, DecodeError> {
    let mut reader = Reader(bytes);
    if reader.bytes::<8>()? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let count = reader.u32()?;
    let mut vectors = Vec::new();
    for _ in 0..count {
        let word = reader.u32()?;
        let psr = reader.u32()?;
        let mut before = [0; STATE_WORDS];
        for slot in &mut before {
            *slot = reader.u32()?;
        }
        let changed = u64::from_le_bytes(reader.bytes()?);
        let mut after = before;
        for (n, slot) in after.iter_mut().enumerate() {
            if changed & (1 << n) != 0 {
                *slot = reader.u32()?;
            }
        }
        let [count] = reader.bytes()?;
        let mut transfers = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let [kind] = reader.bytes()?;
            let width = match kind >> 1 {
                0 => Width::Byte,
                1 => Width::Half,
                2 => Width::Word,
                _ => return Err(DecodeError::BadWidth),
            };
            transfers.push(Transfer {
                write: kind & 1 != 0,
                width,
                address: reader.u32()?,
                data: reader.u32()?,
            });
        }
        vectors.push(Vector {
            word,
            before: state(&before),
            after: state(&after),
            psr,
            transfers,
        });
    }
    if reader.0.is_empty() {
        Ok(vectors)
    } else {
        Err(DecodeError::Truncated)
    }
}

/// The bus a vector runs on: the instruction at its address, and each data
/// read answered from the vector's own transfers.
struct Replay<'a> {
    vector: &'a Vector,
    made: Vec<Transfer>,
}

const fn mask(width: Width) -> u32 {
    match width {
        Width::Byte => 0xFF,
        Width::Half => 0xFFFF,
        Width::Word => u32::MAX,
    }
}

impl Bus for Replay<'_> {
    /// The instruction's bytes where they lie in the word at `address`; a
    /// word holding none of them is unmapped.
    fn fetch(&mut self, address: u32, _privileged: bool) -> Result<Fetch, Abort> {
        let start = self.vector.before.pc;
        let instruction = self.vector.word.to_le_bytes();
        let mut bytes = [0; 4];
        let mut mapped = false;
        for (n, byte) in (0u32..4).zip(&mut bytes) {
            let offset = address.wrapping_add(n).wrapping_sub(start);
            if let Some(&value) = usize::try_from(offset)
                .ok()
                .and_then(|n| instruction.get(n))
            {
                *byte = value;
                mapped = true;
            }
        }
        if mapped {
            Ok(Fetch {
                word: u32::from_le_bytes(bytes),
                host: false,
            })
        } else {
            Err(Abort::Translation)
        }
    }

    fn read(&mut self, address: u32, width: Width, _privileged: bool) -> Result<u32, Abort> {
        let expected = self.vector.transfers.get(self.made.len());
        let data = match expected {
            Some(transfer)
                if !transfer.write && transfer.address == address && transfer.width == width =>
            {
                transfer.data
            }
            _ => 0,
        };
        self.made.push(Transfer {
            write: false,
            width,
            address,
            data,
        });
        Ok(data)
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        _privileged: bool,
    ) -> Result<(), Abort> {
        self.made.push(Transfer {
            write: true,
            width,
            address,
            data: value & mask(width),
        });
        Ok(())
    }
}

/// Runs `vector`'s instruction on a processor of `profile` and compares
/// what it did.
pub fn check(profile: &'static Profile, vector: &Vector) -> Result<(), Mismatch> {
    let mut cpu = Cpu::new(profile, 0);
    let unrepresentable = |_| Mismatch::Registers {
        expected: Box::new(vector.after),
        actual: Box::new(vector.before),
    };
    cpu.set_state(&vector.before).map_err(unrepresentable)?;
    let mut bus = Replay {
        vector,
        made: Vec::new(),
    };
    // A SWI stops for the session; the vector records the exception the
    // architecture takes, which the session enters the same way.
    match cpu.run(&mut bus, 1).stop {
        Stop::Budget => {}
        Stop::Swi { .. } => cpu.enter(Exception::Swi, cpu.pc()),
        stop => return Err(Mismatch::Stopped(stop)),
    }
    let expected = &vector.transfers;
    let made = &bus.made;
    for index in 0..expected.len().max(made.len()) {
        let (want, got) = (expected.get(index), made.get(index));
        let same = match (want, got) {
            (Some(want), Some(got)) => {
                want.write == got.write
                    && want.width == got.width
                    && want.address == got.address
                    && want.data & mask(want.width) == got.data & mask(got.width)
            }
            _ => false,
        };
        if !same {
            return Err(Mismatch::Transfer {
                index,
                expected: want.copied(),
                actual: got.copied(),
            });
        }
    }
    let held = |state: &State| {
        let mut state = *state;
        state.cpsr &= vector.psr;
        for spsr in &mut state.spsr {
            *spsr &= vector.psr;
        }
        state
    };
    let actual = held(&cpu.state());
    let expected = held(&vector.after);
    if actual == expected {
        Ok(())
    } else {
        Err(Mismatch::Registers {
            expected: Box::new(expected),
            actual: Box::new(actual),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{BIT26, BIT32};
    use crate::psr::{self, Mode};

    fn vector() -> Vector {
        let mut before = State {
            pc: 0x8000,
            cpsr: Mode::User.bits(),
            ..State::default()
        };
        before.user[1] = 0x4000;
        let mut after = before;
        after.pc = 0x8004;
        after.user[0] = 0x1234;
        Vector {
            word: 0xE591_0000,
            before,
            after,
            psr: psr::NZCV | psr::I | psr::F | psr::M,
            transfers: alloc::vec![Transfer {
                write: false,
                width: Width::Word,
                address: 0x4000,
                data: 0x1234,
            }],
        }
    }

    #[test]
    fn a_vector_round_trips_through_the_encoding() {
        let vectors = alloc::vec![vector(), vector()];
        let encoded = encode(&vectors).expect("short");
        assert_eq!(decode(&encoded), Ok(vectors));
        assert_eq!(
            decode(&encoded[..encoded.len() - 1]),
            Err(DecodeError::Truncated)
        );
        let mut longer = encoded.clone();
        longer.push(0);
        assert_eq!(decode(&longer), Err(DecodeError::Truncated));
        let mut wrong = encoded;
        wrong[0] ^= 1;
        assert_eq!(decode(&wrong), Err(DecodeError::BadMagic));
    }

    #[test]
    fn a_vector_at_the_top_of_the_address_space_wraps_past_it() {
        for (word, cpsr, pc) in [
            (0xE1A0_0000, Mode::User.bits(), 0xFFFF_FFFC),
            (0x0000_BF00, Mode::User.bits() | psr::T, 0xFFFF_FFFE),
        ] {
            let before = State {
                pc,
                cpsr,
                ..State::default()
            };
            let after = State { pc: 0, ..before };
            let vector = Vector {
                word,
                before,
                after,
                psr: psr::NZCV | psr::T | psr::M,
                transfers: alloc::vec::Vec::new(),
            };
            assert_eq!(check(&BIT32, &vector), Ok(()), "{word:08X}");
        }
    }

    #[test]
    fn the_runner_holds_the_processor_to_the_vector() {
        let good = vector();
        assert_eq!(check(&BIT26, &good), Ok(()));
        let mut wrong_result = good.clone();
        wrong_result.after.user[0] = 0x4321;
        assert!(matches!(
            check(&BIT26, &wrong_result),
            Err(Mismatch::Registers { .. })
        ));
        let mut wrong_address = good;
        wrong_address.transfers[0].address = 0x4004;
        assert!(matches!(
            check(&BIT26, &wrong_address),
            Err(Mismatch::Transfer { index: 0, .. })
        ));
    }
}

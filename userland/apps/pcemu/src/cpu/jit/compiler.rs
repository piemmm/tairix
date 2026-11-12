//! The compiler sandbox's work (`plans/PCEMU.md` PC18.4): a block's bytes,
//! where they lie and the processor they run on, translated and compiled
//! for the host. The front-end sends a [`Compile`] and reads a [`Compiled`]
//! back; the sandbox stages the code it answers with, and the front-end
//! installs it. The bytes are the guest's, so every path here is total over
//! them.

use alloc::vec::Vec;

use tairix_codegen::Target;
use tairix_sandbox::wire::{Reader, WireError, Writer};

use super::cache::{byte_of, Key};
use super::{translate, Code, MOST_BLOCK_BYTES, MOST_CODE_BYTES};
use crate::cpu::model::CpuModel;
use crate::link::{nth, number};

/// What the front-end asks a compiler for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Compile<'a> {
    /// The processor the code runs on: the host's.
    pub target: Target,
    /// The processor the block's instructions are for.
    pub model: CpuModel,
    /// Where the block begins.
    pub key: Key,
    /// Its bytes from there: at most [`MOST_BLOCK_BYTES`].
    pub bytes: &'a [u8],
}

impl Code for Compile<'_> {
    fn byte(&self, addr: u32) -> Option<u8> {
        byte_of(self.key, self.bytes, addr)
    }
}

impl<'a> Compile<'a> {
    /// The request as it crosses; none where the heap refuses the room.
    #[must_use]
    pub fn encode(&self) -> Option<Vec<u8>> {
        let mut w = Writer::refusable();
        w.u8(number(&Target::ALL, &self.target));
        w.u8(number(&CpuModel::ALL, &self.model));
        w.u16(self.key.cs);
        w.u16(self.key.ip);
        w.bytes(self.bytes);
        w.finish()
    }

    /// The request `payload` holds.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a payload that is not a whole request, or one whose
    /// bytes are none or more than any block holds.
    pub fn decode(payload: &'a [u8]) -> Result<Self, WireError> {
        Reader::new(payload).whole(|r| {
            let target = nth(&Target::ALL, r.u8()?)?;
            let model = nth(&CpuModel::ALL, r.u8()?)?;
            let key = Key {
                cs: r.u16()?,
                ip: r.u16()?,
            };
            let bytes = r.bytes(MOST_BLOCK_BYTES)?;
            if bytes.is_empty() {
                return Err(WireError::Malformed);
            }
            Ok(Self {
                target,
                model,
                key,
                bytes,
            })
        })
    }

    /// The code the block compiles to: none where it does not translate, or
    /// compiles to more than [`MOST_CODE_BYTES`].
    #[must_use]
    pub fn code(&self) -> Option<Vec<u8>> {
        let block = translate(self.model, self.key.cs, self.key.ip, self)?;
        let code = tairix_codegen::compile(&block.func, self.target).ok()?;
        (code.len() <= MOST_CODE_BYTES).then_some(code)
    }
}

/// What a compiler answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compiled {
    /// The code, `len` bytes, staged at `base` in the compiler's own memory
    /// for its parent to install.
    Staged {
        /// Where.
        base: u64,
        /// How many: at most [`MOST_CODE_BYTES`].
        len: u64,
    },
    /// No code: the block does not translate or compile, or the compiler had
    /// no room to stage it.
    Refused,
}

impl Compiled {
    /// The answer as it crosses.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::Staged { base, len } => {
                w.u8(1);
                w.u64(base);
                w.u64(len);
            }
            Self::Refused => w.u8(0),
        }
        w.finish()
    }

    /// The answer `payload` holds.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a payload that is not a whole answer, or names no
    /// code or more than any block compiles to.
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        Reader::new(payload).whole(|r| match r.u8()? {
            0 => Ok(Self::Refused),
            1 => {
                let (base, len) = (r.u64()?, r.u64()?);
                if (1..=MOST_CODE_BYTES as u64).contains(&len) {
                    Ok(Self::Staged { base, len })
                } else {
                    Err(WireError::Malformed)
                }
            }
            _ => Err(WireError::Malformed),
        })
    }
}

/// The compiler's answer to request `payload`, the code staged by `stage`,
/// which answers where it put it.
pub fn answer(payload: &[u8], stage: impl FnOnce(&[u8]) -> Option<u64>) -> Vec<u8> {
    let staged = Compile::decode(payload).ok().and_then(|compile| {
        let code = compile.code()?;
        let len = u64::try_from(code.len()).ok()?;
        Some(Compiled::Staged {
            base: stage(&code)?,
            len,
        })
    });
    staged.unwrap_or(Compiled::Refused).encode()
}

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;

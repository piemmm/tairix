//! Module SWIs (`plans/RISCOS-EMULATOR.md` §4.1): a chunk's SWIs performed
//! by the module that registered for it last, entered with R11 the SWI's
//! offset and R12 its preferred incarnation's private word, and named by the
//! module's decoding table, its decoding code, or its title.

use alloc::vec::Vec;

use super::ModuleKey;
use crate::os::{Entry, Fail, Os, Regs, Target};
use crate::swi::{CHUNK, X};

/// The longest name in a decoding table the kernel reads.
const NAME_MOST: usize = 256;

/// How the SWIs of a chunk are named.
pub(crate) enum ChunkNames {
    /// No module has the chunk.
    User,
    /// The module's decoding table, as it holds it: its prefix, then its
    /// names from offset 0, each terminated, then an empty one.
    Table(Vec<u8>),
    /// Code the module provides, entered with R12 its private word.
    Code {
        /// The code.
        target: Target,
        /// The preferred incarnation's private word.
        private: u32,
    },
    /// The module's title.
    Title(Vec<u8>),
}

impl Os<'_> {
    /// SWI `number`, performed by the module registered for its chunk if
    /// that module has an incarnation to perform it.
    pub(crate) fn module_swi(&mut self, number: u32, regs: &mut Regs) -> Option<Result<(), Fail>> {
        let base = number & !X;
        let key = self.world.modules.chunk_owner(base & !(CHUNK - 1))?;
        let module = self.world.modules.get(key)?;
        let private = module.incarnations.first()?.private();
        let shape = module.shape;
        let offset = base & (CHUNK - 1);
        regs.r[11] = offset;
        regs.r[12] = private;
        let swis = shape
            .origin
            .builtin()
            .and_then(|place| self.world.modules.builtin(place))
            .and_then(|builtin| builtin.swis.as_ref());
        if let Some(swis) = swis {
            return Some((swis.handler)(self, offset, regs));
        }
        let (_, handler) = shape.header.swis()?;
        let target = Target {
            address: shape.code.wrapping_add(handler),
            bits: shape.bits,
        };
        regs.r[10] = target.address;
        Some(match self.call(target, regs, Entry::Supervisor) {
            Ok(_) if regs.flag(tairix_riscos_arm::psr::V) => Err(Fail::Error(regs.r[0])),
            Ok(_) => Ok(()),
            Err(unwind) => Err(Fail::Unwind(unwind)),
        })
    }

    /// How the SWIs of `chunk` are named.
    pub(crate) fn chunk_names(&mut self, chunk: u32) -> Result<ChunkNames, Fail> {
        match self.world.modules.chunk_owner(chunk) {
            Some(key) => self.names_of(key),
            None => Ok(ChunkNames::User),
        }
    }

    /// How `key` names its SWIs.
    fn names_of(&mut self, key: ModuleKey) -> Result<ChunkNames, Fail> {
        let Some(module) = self.world.modules.get(key) else {
            return Ok(ChunkNames::User);
        };
        let shape = module.shape;
        let private = module
            .incarnations
            .first()
            .copied()
            .map(super::Incarnation::private);
        if let Some(table) = shape.header.swi_table() {
            return Ok(ChunkNames::Table(
                self.decoding_table(shape.code.wrapping_add(table))?,
            ));
        }
        if let (Some(code), Some(private)) = (shape.header.swi_code(), private) {
            let target = Target {
                address: shape.code.wrapping_add(code),
                bits: shape.bits,
            };
            return Ok(ChunkNames::Code { target, private });
        }
        Ok(ChunkNames::Title(self.title_of(key)?))
    }

    /// The decoding table at `at`: the prefix and at most a chunk's names,
    /// to the empty name that ends it or the first that does not end.
    fn decoding_table(&mut self, at: u32) -> Result<Vec<u8>, Fail> {
        let mut table = Vec::new();
        let mut at = at;
        for read in 0..=CHUNK {
            let (text, ended) = self.read_until(at, NAME_MOST, |b| b == 0)?;
            if !ended || (read > 0 && text.is_empty()) {
                break;
            }
            if table.try_reserve(text.len() + 1).is_err() {
                return Err(self.no_memory());
            }
            table.extend_from_slice(&text);
            table.push(0);
            at = at.wrapping_add(crate::os::len32(text.len()) + 1);
        }
        Ok(table)
    }

    /// The SWI a module of the chain names `name` — without its `X` — as
    /// `OS_SWINumberFromString` finds it: each module that registered SWIs,
    /// in the chain's order, by its table, its code, or its title and a
    /// number. `at` is the name in guest memory, for the code.
    pub(crate) fn module_swi_named(&mut self, name: &[u8], at: u32) -> Result<Option<u32>, Fail> {
        let mut index = 0;
        while let Some(key) = self.world.modules.nth(index) {
            index += 1;
            let Some((chunk, _)) = self
                .world
                .modules
                .get(key)
                .and_then(|m| m.shape.header.swis())
            else {
                continue;
            };
            let offset = match self.names_of(key)? {
                ChunkNames::User => None,
                ChunkNames::Table(table) => crate::swi::names::offset_from_string(
                    name,
                    crate::swi::names::Naming::Table(&table),
                ),
                ChunkNames::Code { target, private } => {
                    let mut regs = Regs::default();
                    regs.r[0] = u32::MAX;
                    regs.r[1] = at;
                    regs.r[12] = private;
                    self.call(target, &mut regs, Entry::Supervisor)?;
                    (regs.r[0].cast_signed() >= 0).then_some(regs.r[0] & (CHUNK - 1))
                }
                ChunkNames::Title(title) => crate::swi::names::offset_from_string(
                    name,
                    crate::swi::names::Naming::Title(&title),
                ),
            };
            if let Some(offset) = offset {
                return Ok(Some(chunk | offset));
            }
            self.yield_point()?;
        }
        Ok(None)
    }

    /// `OS_SWINumberToString` for SWI `number` of a chunk its module names
    /// with code: an `X`, then whatever the code writes into R1's buffer of
    /// R3 bytes, then the terminator; R2 the bytes written with it.
    pub(crate) fn name_by_code(
        &mut self,
        number: u32,
        target: Target,
        private: u32,
        regs: &mut Regs,
    ) -> Result<(), Fail> {
        let (buffer, size) = (regs.r[1], regs.r[2]);
        let mut written = 0;
        if number & X != 0 && size > 0 {
            self.write_byte(buffer, b'X')?;
            written = 1;
        }
        if written >= size {
            return Err(self.buffer_overflow());
        }
        let mut call = Regs::default();
        call.r[..4].copy_from_slice(&[number & (CHUNK - 1), buffer, written, size]);
        call.r[12] = private;
        self.call(target, &mut call, Entry::Supervisor)?;
        let end = call.r[2];
        if call.flag(tairix_riscos_arm::psr::V) || end >= size {
            return Err(self.buffer_overflow());
        }
        self.write_byte(buffer.wrapping_add(end), 0)?;
        regs.r[2] = end + 1;
        Ok(())
    }
}

//! A module's header: the thirteen words at its start, held to the bytes the
//! module occupies before anything of it runs.
//!
//! A module that declares itself 32-bit is checked as RISC OS 5 checks one.
//! One that does not is a 26-bit module, admitted only where 26-bit code is:
//! its header may be as short as RISC OS 2's, so the words past the command
//! table count only where they describe what they would — a valid SWI
//! registration, a word-aligned message file — as RISC OS 3 took them.

use crate::error::Error;
use crate::memory::Bits;
use crate::os::{Fail, Os};
use crate::swi::X;

/// The header's words, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    Start,
    Init,
    Final,
    Service,
    Title,
    Help,
    Commands,
    SwiChunk,
    SwiHandler,
    SwiTable,
    SwiCode,
    Messages,
    Flags,
}

impl Field {
    /// The field's offset in the header.
    pub(crate) const fn offset(self) -> u32 {
        4 * self as u32
    }
}

/// The words in a header.
pub(crate) const WORDS: u32 = 13;

/// The flags word's bit declaring a module 32-bit.
pub(crate) const FLAG_32BIT: u32 = 1;

/// An initialisation offset's bit marking a squeezed module, and a
/// finalisation offset's marking one `*RMClear` leaves.
pub(crate) const TOP: u32 = 0x8000_0000;

/// "A module header field is out of range".
pub(crate) const BAD_FIELD: Error = Error {
    number: 0x10E,
    token: "BadModuleHeader",
};

/// "Module '…' cannot run in 32-bit mode".
const NOT_32BIT: Error = Error {
    number: 0x10E,
    token: "ModuleNot32Bit",
};

/// The longest title the kernel reads.
pub(crate) const TITLE_MOST: usize = 256;

/// A module's header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Header {
    words: [u32; WORDS as usize],
    /// The bytes every offset falls within.
    size: u32,
}

/// Whether `offset` falls within `size` bytes, and when `aligned` is a
/// multiple of four; zero, meaning none, always does.
const fn fits(offset: u32, size: u32, aligned: bool) -> bool {
    offset < size && (!aligned || offset.is_multiple_of(4))
}

impl Header {
    /// The word at `field`.
    pub(crate) const fn get(&self, field: Field) -> u32 {
        self.words[field as usize]
    }

    /// The bytes every offset falls within.
    pub(crate) const fn size(&self) -> u32 {
        self.size
    }

    /// The finalisation entry's offset; zero for none.
    pub(crate) const fn final_entry(&self) -> u32 {
        self.get(Field::Final) & !TOP
    }

    /// Whether `*RMClear` leaves the module.
    pub(crate) const fn invincible(&self) -> bool {
        self.get(Field::Final) & TOP != 0
    }

    /// The length of a squeezed module's image, if it is one.
    pub(crate) const fn squeezed(&self) -> Option<u32> {
        let init = self.get(Field::Init);
        if init & TOP == 0 {
            None
        } else {
            Some(init & !TOP)
        }
    }

    /// The module's SWI chunk and its handler's offset, where both are fit
    /// to register: a chunk of 64 in the first sixteen million, X aside, and
    /// a handler within the module.
    pub(crate) const fn swis(&self) -> Option<(u32, u32)> {
        let chunk = self.get(Field::SwiChunk) & !X;
        let handler = self.get(Field::SwiHandler);
        let chunk_fits = chunk != 0 && chunk & 0xFF00_003F == 0;
        if chunk_fits && handler != 0 && fits(handler, self.size, true) {
            Some((chunk, handler))
        } else {
            None
        }
    }

    /// The SWI decoding table's offset, if the module has one.
    pub(crate) const fn swi_table(&self) -> Option<u32> {
        let table = self.get(Field::SwiTable);
        if table != 0 && fits(table, self.size, false) {
            Some(table)
        } else {
            None
        }
    }

    /// The SWI decoding code's offset, if the module has it.
    pub(crate) const fn swi_code(&self) -> Option<u32> {
        let code = self.get(Field::SwiCode);
        if code != 0 && fits(code, self.size, true) {
            Some(code)
        } else {
            None
        }
    }
}

impl Os<'_> {
    /// The header of the module at `code`, `size` bytes long, as checked
    /// before anything of it runs, and the profile it runs in.
    pub(crate) fn checked_header(&mut self, code: u32, size: u32) -> Result<(Header, Bits), Fail> {
        let mut words = [0; WORDS as usize];
        for (at, word) in (code..).step_by(4).zip(words.iter_mut()) {
            *word = self.read_word(at)?;
        }
        let header = Header { words, size };
        let get = |field| header.get(field);
        let early = get(Field::Commands) < size
            && get(Field::Help) < size
            && get(Field::Title) < size
            && fits(get(Field::Service), size, true)
            && fits(header.final_entry(), size, true)
            && (header.squeezed().is_some() || fits(get(Field::Init), size, true));
        if !early {
            return Err(self.error(BAD_FIELD));
        }
        let flags = get(Field::Flags);
        let declared = flags != 0
            && fits(flags, size, true)
            && self.read_word(code.wrapping_add(flags))? & FLAG_32BIT != 0;
        if !declared {
            if self.memory.layout().admits_26bit {
                return Ok((header, Bits::Bit26));
            }
            return Err(self.not_32bit(code, &header));
        }
        let late = get(Field::SwiChunk) & 0xFF00_003F == 0
            && fits(get(Field::SwiHandler), size, true)
            && get(Field::SwiTable) < size
            && fits(get(Field::SwiCode), size, true)
            && fits(get(Field::Messages), size, true);
        if late {
            Ok((header, Bits::Bit32))
        } else {
            Err(self.not_32bit(code, &header))
        }
    }

    /// "Module '…' cannot run in 32-bit mode", naming the module at `code`.
    fn not_32bit(&mut self, code: u32, header: &Header) -> Fail {
        let title = code.wrapping_add(header.get(Field::Title));
        match self.read_until(title, TITLE_MOST, |b| b <= b' ') {
            Ok((title, _)) => self.error_with(NOT_32BIT, &[&title]),
            Err(fail) => fail,
        }
    }
}

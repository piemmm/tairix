//! RISC OS errors as the kernel raises them: a number and the token its text
//! is found under in the kernel's message file. Once MessageTrans has the
//! file open, an error is looked up through it into its buffers; before then,
//! or where MessageTrans gives back some other error, the text is the
//! kernel's own copy's, written into the next of the kernel's ring of buffers
//! in guest memory, where it lasts until the ring comes round again.
//!
//! A number is the one genuine RISC OS 5 was recorded giving. Each condition
//! it was never seen to report takes in turn the lowest number in the PRM's
//! range for its family that no recorded error uses.

use alloc::vec::Vec;

use tairix_riscos_arm::fpa::Exception;

use crate::msgtrans::scan;
use crate::os::{len32, Ended, Fail, Os, Regs, Unwind};

/// An error the kernel raises.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Error {
    /// Its number.
    pub number: u32,
    /// The token the kernel's message file holds its text under.
    pub token: &'static str,
}

/// The error FPEmulator raises for an FPA exception its trap is enabled
/// for.
#[must_use]
pub const fn floating_point(exception: Exception) -> Error {
    let (offset, token) = match exception {
        Exception::InvalidOperation => (0, "FloatingInvalidOperation"),
        Exception::Overflow => (1, "FloatingOverflow"),
        Exception::DivisionByZero => (2, "FloatingDivideByZero"),
        Exception::Underflow => (3, "FloatingUnderflow"),
        Exception::Inexact => (4, "FloatingInexact"),
    };
    Error {
        number: FLOATING_POINT + offset,
        token,
    }
}

/// The floating-point errors' first number.
const FLOATING_POINT: u32 = 0x8000_0200;

/// The kernel's message file, its texts the emulator's own.
pub(crate) const MESSAGES: &[u8] = include_bytes!("../resources/Kernel/Messages");

/// Error buffers in the ring: enough that a block a caller holds survives
/// the errors a few nested calls raise after it.
pub(crate) const BUFFERS: u32 = 16;
/// Each buffer's size: the number, and the longest text an error block has.
pub(crate) const BUFFER_SIZE: u32 = 256;
/// The longest text, with room for its terminator.
pub(crate) const TEXT: usize = (BUFFER_SIZE - 4 - 1) as usize;

/// The ring of error buffers, made when the system heap is.
pub(crate) struct Ring {
    base: u32,
    next: u32,
}

impl Ring {
    /// The ring at `base`, [`BUFFERS`] buffers long.
    pub(crate) const fn at(base: u32) -> Self {
        Self { base, next: 0 }
    }

    /// The buffer to write next, moving the ring on.
    const fn take(&mut self) -> u32 {
        let buffer = self.base + self.next * BUFFER_SIZE;
        self.next = (self.next + 1) % BUFFERS;
        buffer
    }
}

/// The kernel's own copy's value of `token`, or the token itself where it
/// has none.
fn own_value(token: &[u8]) -> &[u8] {
    scan::find(MESSAGES, token)
        .and_then(|at| Some(&MESSAGES[at..at + scan::value_length(MESSAGES, at)?]))
        .unwrap_or(token)
}

/// `value` with `parameters` for `%0` onwards, into `out`: its length.
fn substituted(value: &[u8], parameters: &[&[u8]], out: &mut [u8]) -> usize {
    let mut given = [None; 4];
    for (slot, &parameter) in given.iter_mut().zip(parameters) {
        *slot = Some(parameter);
    }
    scan::Expansion {
        parameters: given,
        dictionary: &[],
    }
    .expand(value, out)
}

impl Os<'_> {
    /// Fails with `error`.
    pub fn error(&mut self, error: Error) -> Fail {
        self.error_with(error, &[])
    }

    /// Fails with `error`, `%0` onwards in its text replaced by `parameters`.
    pub fn error_with(&mut self, error: Error, parameters: &[&[u8]]) -> Fail {
        if let Some(fail) = self.translated_error(error, parameters) {
            return fail;
        }
        let mut bytes = [0; TEXT];
        let length = substituted(own_value(error.token.as_bytes()), parameters, &mut bytes);
        let buffer = self.world.errors.take();
        self.error_block(buffer, error.number, &bytes[..length])
    }

    /// `error` looked up through MessageTrans in the kernel's file: none while
    /// it is not open or one is being looked up, or where the answer is some
    /// other error — MessageTrans gone, say.
    fn translated_error(&mut self, error: Error, parameters: &[&[u8]]) -> Option<Fail> {
        if !self.translatable() {
            return None;
        }
        let file = self.world.kernel_messages;
        self.world.translating = true;
        let answer = self.request_error(file.request(), file.descriptor(), error, parameters);
        self.world.translating = false;
        match answer {
            Fail::Error(block) if self.peek_word(block) == Some(error.number) => Some(answer),
            Fail::Error(_) => None,
            Fail::Unwind(_) => Some(answer),
        }
    }

    /// Fails with a block of `number` and `text`, cut to an error block's
    /// longest: an error the guest made, kept where its own memory cannot
    /// take it away.
    pub(crate) fn raw_error(&mut self, number: u32, text: &[u8]) -> Fail {
        let buffer = self.world.errors.take();
        self.error_block(buffer, number, &text[..text.len().min(TEXT)])
    }

    /// Writes an error block of `number` and `text` into the ring's
    /// `buffer`, which the kernel's own memory holds whatever the guest has
    /// done.
    fn error_block(&mut self, buffer: u32, number: u32, text: &[u8]) -> Fail {
        let mut block = [0; BUFFER_SIZE as usize];
        block[..4].copy_from_slice(&number.to_le_bytes());
        block[4..4 + text.len()].copy_from_slice(text);
        let words = (4 + text.len() + 1).div_ceil(4);
        for (at, word) in (buffer..)
            .step_by(4)
            .zip(block.as_chunks::<4>().0)
            .take(words)
        {
            if self.poke_word(at, u32::from_le_bytes(*word)).is_none() {
                return Fail::Unwind(Unwind::Ended(Ended::Fault));
            }
        }
        Fail::Error(buffer)
    }

    /// The kernel's message `token`, `%0` onward replaced by `parameters`:
    /// text the kernel prints, looked up as its errors are.
    pub(crate) fn message_with(
        &mut self,
        token: &[u8],
        parameters: &[&[u8]],
    ) -> Result<Vec<u8>, Fail> {
        let value = if let Some(value) = self.translated_message(token)? {
            value
        } else {
            let value = own_value(token);
            let mut owned = Vec::new();
            owned
                .try_reserve_exact(value.len())
                .map_err(|_| self.no_memory())?;
            owned.extend_from_slice(value);
            owned
        };
        let room = value.len() + parameters.iter().map(|p| p.len()).sum::<usize>() * 2;
        let mut out = Vec::new();
        out.try_reserve_exact(room).map_err(|_| self.no_memory())?;
        out.resize(room, 0);
        let length = substituted(&value, parameters, &mut out);
        out.truncate(length);
        Ok(out)
    }

    /// The value of `token` in the kernel's file through MessageTrans, as it
    /// stands in the file; none where it cannot be looked up there.
    fn translated_message(&mut self, token: &[u8]) -> Result<Option<Vec<u8>>, Fail> {
        if !self.translatable() {
            return Ok(None);
        }
        let file = self.world.kernel_messages;
        self.world.translating = true;
        let found = self.write_bytes(file.request(), token).and_then(|()| {
            self.write_byte(file.request().wrapping_add(len32(token.len())), 0)?;
            let mut regs = Regs::default();
            regs.r[..3].copy_from_slice(&[file.descriptor(), file.request(), 0]);
            self.perform(crate::swi::X | crate::msgtrans::LOOKUP, &mut regs)?;
            let mut value = Vec::new();
            value
                .try_reserve_exact(regs.r[3] as usize)
                .map_err(|_| self.no_memory())?;
            value.resize(regs.r[3] as usize, 0);
            self.read_bytes(regs.r[2], &mut value)?;
            Ok(value)
        });
        self.world.translating = false;
        match found {
            Ok(value) => Ok(Some(value)),
            Err(Fail::Error(_)) => Ok(None),
            Err(unwind) => Err(unwind),
        }
    }

    /// Whether the kernel's file is open, a MessageTrans answers its SWIs —
    /// asking an absent one would bring back its own "SWI not known" — and
    /// no lookup is under way.
    fn translatable(&self) -> bool {
        self.world.kernel_messages.is_open()
            && !self.world.translating
            && self
                .world
                .modules
                .chunk_owner(crate::msgtrans::CHUNK)
                .is_some()
    }

    /// "Buffer overflow".
    pub fn buffer_overflow(&mut self) -> Fail {
        self.error(BUFFER_OVERFLOW)
    }

    /// The error a request for host memory the kernel was refused gives.
    pub fn no_memory(&mut self) -> Fail {
        self.error(SYSTEM_HEAP_FULL)
    }

    /// The syntax error a command gives for parameters it cannot read: its
    /// `syntax` message.
    pub fn syntax_failure(&mut self, syntax: &str) -> Fail {
        self.raw_error(SYNTAX, syntax.as_bytes())
    }
}

/// "Buffer overflow".
pub const BUFFER_OVERFLOW: Error = obey_error(tairix_obey::Error::BufferOverflow);

/// A command line too long for the command line interpreter.
pub const LINE_TOO_LONG: Error = obey_error(tairix_obey::Error::LineTooLong);

/// "No room left in the system heap".
pub const SYSTEM_HEAP_FULL: Error = obey_error(tairix_obey::Error::SystemHeapFull);

/// "Invalid parameters".
pub const BAD_PARAMETERS: Error = obey_error(tairix_obey::Error::BadParameters);

/// A string that cannot be read: what RISC OS gives for a name no pointer
/// points at.
pub const BAD_STRING: Error = obey_error(tairix_obey::Error::BadString);

/// "Bad number".
pub const BAD_NUMBER: Error = obey_error(tairix_obey::Error::BadNumber);

/// The number every syntax error has, whatever its text.
pub(crate) const SYNTAX: u32 = 0xDC;

/// "Unknown command".
pub const BAD_COMMAND: Error = Error {
    number: 0xFE,
    token: "UnknownCommand",
};

/// "Invalid address".
pub(crate) const BAD_ADDRESS: Error = Error {
    number: 0xFC,
    token: "InvalidAddress",
};

/// `lib/obey`'s error as the kernel's: the number `lib/obey`'s replay of
/// genuine RISC OS 5 pins, or for its two bounds on hostile text and a full
/// variable store, the kernel's own.
#[must_use]
pub const fn obey_error(error: tairix_obey::Error) -> Error {
    use tairix_obey::Error as E;
    let (number, token) = match error {
        E::BadString => (0xFD, "UntranslatableString"),
        E::BadMacroValue => (0x120, "BadMacroValue"),
        E::BadVariableName => (0x121, "BadVariableName"),
        E::NoRoomForVariable => (0x123, "NoRoomForVariable"),
        E::MismatchedBrackets => (0x162, "UnbalancedBrackets"),
        E::ExpressionStackOverflow => (0x163, "ExpressionTooDeep"),
        E::MissingOperand => (0x164, "MissingOperand"),
        E::MissingOperator => (0x165, "MissingOperator"),
        E::NotAnInteger => (0x166, "NotAnInteger"),
        E::StringTooLong => (0x167, "StringResultTooLong"),
        E::UnknownOperand => (0x168, "UnknownOperand"),
        E::DivisionByZero => (0x169, "DivideByZero"),
        E::BadBase => (0x16A, "BadNumberBase"),
        E::BadNumber => (0x16B, "BadNumber"),
        E::NumberTooBig => (0x16C, "NumberTooBig"),
        E::LineTooLong => (0x1E0, "CommandLineTooLong"),
        E::BufferOverflow => (0x1E4, "BufferOverflow"),
        E::BadParameters => (0x1EA, "InvalidParameters"),
        E::ArgumentRepeated => (0x1EB, "ArgumentGivenTwice"),
        E::Syntax => (SYNTAX, "WrongNumberOfParameters"),
        E::ExpansionTooComplex => (0x1E1, "ExpansionTooComplex"),
        E::SystemHeapFull => (0x1E3, "SystemHeapFull"),
    };
    Error { number, token }
}

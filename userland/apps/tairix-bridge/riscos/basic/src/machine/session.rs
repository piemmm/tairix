//! The prompt and what is typed at it, the error path, and `RND`.

use tairix_openbasic::list::number_field;
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::Dialect;
use tairix_riscos_kernel::os::{Fail, Unwind};
use tairix_riscos_kernel::swi::os::GENERATE_ERROR;

use super::functions::{decimal, Guest};
use super::next::Next;
use super::{flag, len32, Leave, Machine, Mode, Step, Trap};
use crate::messages::Error;
use crate::stack::Kind;
use crate::value::{Text, Value};
use crate::workspace::{self as ws, reg};

/// The prompt.
const PROMPT: &[u8] = b">";

impl Machine<'_, '_> {
    /// The prompt given and a line read from User mode, a newline first
    /// where the VDU drivers report the text cursor mid-line, whatever
    /// `COUNT` says. Under `AUTO` the prompt is the line's number, as `LIST`
    /// shows it.
    pub(crate) fn prompt(&mut self) -> Step<Leave> {
        self.flags &= !flag::RUNNING;
        self.line = 0;
        let mid_line = match self.cursor() {
            Ok((column, _)) => column != 0,
            // No VDU drivers to answer: no cursor left mid-line.
            Err(Trap::Os(_)) => false,
            Err(trap) => return Err(trap),
        };
        if mid_line {
            self.newline()?;
        }
        match self.auto_line()? {
            Some(number) => self.output(&number_field(number))?,
            None => self.output(PROMPT)?,
        }
        self.sp = self.himem;
        self.push(Kind::Guest, Guest::Prompt as u8, &[])?;
        self.mode = Mode::Away;
        let mut regs = [0; 10];
        regs[..5].copy_from_slice(&[ws::TYPED.at(), ws::LINE_ROOM - 1, 0, 0xFF, 0]);
        Ok(Leave::Swi {
            number: super::statements::READ_LINE,
            regs,
            flags: 0,
        })
    }

    /// A line typed at the prompt: a program line stored, or an immediate
    /// line run.
    pub(crate) fn typed(&mut self, length: u32, escape: bool) -> Step<Next> {
        self.os.write_word(ws::TALLY.at(), 0)?;
        if escape {
            self.newline()?;
            return Err(Error::Escape.into());
        }
        let length = length.min(ws::LINE_ROOM - 1) as usize;
        let mut typed = [0; ws::LINE_ROOM as usize];
        self.os.read_bytes(ws::TYPED.at(), &mut typed[..length])?;
        let typed = &typed[..length];
        if let Some(number) = self.auto_line()? {
            return self.auto_typed(number, typed);
        }
        let numbered = typed
            .iter()
            .find(|&&b| b != b' ')
            .is_some_and(u8::is_ascii_digit);
        let mut tokens = alloc::vec::Vec::new();
        let tokenised = tokenise(
            tairix_openbasic::Dialect::BasicV,
            typed,
            Start::Line,
            &mut tokens,
        )
        .map_err(|_| Error::LineTooLong)?;
        if numbered {
            self.warn(tokenised.warnings)?;
            self.enter_line(&tokens)?;
            self.mode = Mode::Prompt;
            return Ok(Next::Moved);
        }
        if tokens.len() >= ws::LINE_ROOM as usize {
            return Err(Error::LineTooLong.into());
        }
        let at = ws::IMMEDIATE.at();
        self.os.write_bytes(at, &tokens)?;
        self.os.write_byte(at + len32(tokens.len()), b'\r')?;
        self.ptr = at;
        self.line = 0;
        self.mode = Mode::Statement;
        Ok(Next::Moved)
    }

    /// The line `AUTO` numbers next, while it is on.
    fn auto_line(&mut self) -> Step<Option<u16>> {
        if self.os.read_word(reg::AUTO_STEP.at())? == 0 {
            return Ok(None);
        }
        Ok(u16::try_from(self.os.read_word(reg::AUTO.at())?).ok())
    }

    /// A line typed under `AUTO`: stored as line `number`, or that line
    /// deleted for an empty one; then the next numbered, or `AUTO` ended past
    /// the last line.
    fn auto_typed(&mut self, number: u16, typed: &[u8]) -> Step<Next> {
        let mut tokens = alloc::vec::Vec::new();
        let tokenised = tokenise(Dialect::BasicV, typed, Start::Statement, &mut tokens)
            .map_err(|_| Error::LineTooLong)?;
        self.warn(tokenised.warnings)?;
        self.enter_text(number, &tokens)?;
        let next = u32::from(number).wrapping_add(self.os.read_word(reg::AUTO_STEP.at())?);
        if next > super::editor::LAST_LINE {
            self.os.write_word(reg::AUTO_STEP.at(), 0)?;
        } else {
            self.os.write_word(reg::AUTO.at(), next)?;
        }
        self.mode = Mode::Prompt;
        Ok(Next::Moved)
    }

    /// The warnings a typed program line's tokenising gave, each on a line.
    fn warn(&mut self, warnings: tairix_openbasic::tokenise::Warnings) -> Step {
        for (given, text) in [
            (warnings.quote, WARN_QUOTE),
            (warnings.brackets, WARN_BRACKETS),
            (warnings.line_number, WARN_LINE_NUMBER),
        ] {
            if given {
                self.output(text)?;
                self.newline()?;
            }
        }
        Ok(())
    }

    /// The error `trap` met: `ERR`, `ERL` and `REPORT$` set, then the
    /// `ON ERROR` handler run, or the message written and the prompt given.
    /// A floating-point exception is raised through the OS instead, as the
    /// floating-point emulator raises it, so that `ErrorV` and the kernel
    /// meet it before BASIC's error handler does: the SWI left for.
    pub(crate) fn error(&mut self, trap: Trap) -> Result<Option<Leave>, Unwind> {
        if let Trap::Floating(exception) = trap {
            match self.raise_through_os(exception) {
                Ok(leave) => return Ok(Some(leave)),
                Err(Trap::Unwind(unwind)) => return Err(unwind),
                // No room to keep its block: met here instead.
                Err(_) => {}
            }
        }
        match self.raise(trap) {
            Ok(()) | Err(Trap::Basic(_) | Trap::Os(_) | Trap::Floating(_)) => Ok(None),
            Err(Trap::Unwind(unwind)) => Err(unwind),
        }
    }

    /// `OS_GenerateError` left for with `exception`'s error.
    fn raise_through_os(&mut self, exception: crate::Exception) -> Step<Leave> {
        let block = match self
            .os
            .error(tairix_riscos_kernel::error::floating_point(exception))
        {
            Fail::Error(block) => block,
            Fail::Unwind(unwind) => return Err(Trap::Unwind(unwind)),
        };
        self.raise_block_through_os(block)
    }

    /// `OS_GenerateError` left for with the error `block`, as a call that
    /// met it outside its X form would raise it.
    pub(crate) fn raise_block_through_os(&mut self, block: u32) -> Step<Leave> {
        self.push(Kind::Guest, Guest::Raise as u8, &[block])?;
        self.mode = Mode::Away;
        let mut regs = [0; 10];
        regs[0] = block;
        Ok(Leave::Swi {
            number: GENERATE_ERROR,
            regs,
            flags: 0,
        })
    }

    fn raise(&mut self, trap: Trap) -> Step {
        self.flags &= !(flag::ASSEMBLING | flag::LABEL_FIELD);
        self.os.write_word(reg::AUTO_STEP.at(), 0)?;
        let (number, text, from_os) = self.describe(trap)?;
        let erl = self.line_number()?;
        self.os.write_word(reg::ERR.at(), number)?;
        self.os.write_word(reg::ERL.at(), erl)?;
        let block = reg::ERROR.at();
        self.os.write_word(block, number)?;
        self.os.write_bytes(block + 4, text.as_bytes())?;
        self.os.write_byte(block + 4 + u32::from(text.len()), 0)?;
        if from_os {
            self.flags |= flag::OS_ERROR;
        } else {
            self.flags &= !flag::OS_ERROR;
        }
        let handler = self.os.read_word(reg::ON_ERROR.at())?;
        if handler != 0 && trappable(number, from_os) {
            let sp = self.os.read_word(reg::ON_ERROR_SP.at())?;
            self.unwind_to(sp)?;
            self.ptr = handler;
            self.line = self.os.read_word(reg::ON_ERROR_LINE.at())?;
            if self.line != 0 {
                self.flags |= flag::RUNNING;
            }
            self.mode = Mode::Statement;
            return Ok(());
        }
        self.sp = self.himem;
        // As recorded: an immediate line's report opens a line of its own,
        // a program's follows whatever it printed last.
        if erl == 0 {
            self.newline()?;
        }
        self.output(text.as_bytes())?;
        if erl != 0 {
            self.output(b" at line ")?;
            let digits = decimal(erl.cast_signed());
            self.output(digits.as_bytes())?;
        }
        self.newline()?;
        self.flags &= !flag::RUNNING;
        self.mode = if self.flags & flag::QUIT_AT_END != 0 {
            Mode::Quit
        } else {
            Mode::Prompt
        };
        Ok(())
    }

    /// The number and text `trap` stands for, and whether it came from
    /// outside BASIC.
    fn describe(&mut self, trap: Trap) -> Step<(u32, Text, bool)> {
        match trap {
            Trap::Basic(error) => Ok((
                error.number(),
                Text::of(error.text().as_bytes()).unwrap_or(Text::EMPTY),
                false,
            )),
            Trap::Os(block) => self.os_error(block).map(|(n, t)| (n, t, true)),
            Trap::Floating(exception) => {
                let error = tairix_riscos_kernel::error::floating_point(exception);
                match self.os.error(error) {
                    Fail::Error(block) => self.os_error(block).map(|(n, t)| (n, t, true)),
                    Fail::Unwind(unwind) => Err(Trap::Unwind(unwind)),
                }
            }
            Trap::Unwind(unwind) => Err(Trap::Unwind(unwind)),
        }
    }

    /// The number and text of the error block at `block`.
    fn os_error(&mut self, block: u32) -> Step<(u32, Text)> {
        let number = self.os.read_word(block)?;
        let mut text = Text::EMPTY;
        for i in 0..crate::value::STRING_MOST {
            let b = self.os.read_byte(block + 4 + len32(i))?;
            if b == 0 {
                break;
            }
            text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
        }
        Ok((number, text))
    }

    /// The stack unwound to `sp` for an `ON ERROR`: every value `LOCAL`
    /// saved above it put back, everything else dropped.
    fn unwind_to(&mut self, sp: u32) -> Step {
        while self.sp < sp {
            let header = self.top_header()?.ok_or(Error::StackCorrupt)?;
            match header.kind {
                Kind::Local if header.aux == 1 => self.restore_local()?,
                Kind::LocalError => self.restore_local_error()?,
                _ => {
                    self.drop_top()?;
                }
            }
        }
        if self.sp != sp {
            self.sp = sp.min(self.himem);
        }
        Ok(())
    }

    // RND.

    /// `RND`: the next 32-bit number of BASIC's generator.
    pub(crate) fn random(&mut self) -> Step<i32> {
        let seed = stepped(self.seed()?);
        self.set_seed(seed)?;
        Ok(((seed & LOW_WORD) as u32).cast_signed())
    }

    /// `RND(n)`: a seed set by a negative `n`, the generator's fraction again
    /// for nought, the next fraction for one, an integer from one to `n` above
    /// it, scaled from that fraction exactly.
    pub(crate) fn rnd(&mut self, n: i32) -> Step<Value> {
        if n < 0 {
            self.set_seed(u64::from(n.cast_unsigned()))?;
            return Ok(Value::Int(n));
        }
        if n > 0 {
            self.random()?;
        }
        let fraction = fraction(self.seed()?);
        if n <= 1 {
            return Ok(Value::Real(self.arith.unit_fraction(fraction)));
        }
        let scaled = (u64::from(fraction) * u64::from(n.cast_unsigned())) >> 32;
        Ok(Value::Int(i32::try_from(scaled + 1).unwrap_or(n)))
    }

    /// The generator's seed, its thirty-three bits.
    fn seed(&mut self) -> Step<u64> {
        let mut bytes = [0; 8];
        self.os.read_bytes(reg::SEED.at(), &mut bytes[..5])?;
        Ok(u64::from_le_bytes(bytes) & SEED_MASK)
    }

    fn set_seed(&mut self, seed: u64) -> Step {
        let bytes = (seed & SEED_MASK).to_le_bytes();
        Ok(self.os.write_bytes(reg::SEED.at(), &bytes[..5])?)
    }
}

/// The generator's thirty-three bits.
const SEED_MASK: u64 = (1 << 33) - 1;
/// The word of them a draw gives.
const LOW_WORD: u64 = (1 << 32) - 1;

/// `seed` after one draw: thirty-two shifts of the register, each feeding in
/// its bits 32 and 19 exclusive-ored.
const fn stepped(seed: u64) -> u64 {
    let mut seed = seed;
    let mut shifts = 0;
    while shifts < 32 {
        let bit = ((seed >> 32) ^ (seed >> 19)) & 1;
        seed = ((seed << 1) | bit) & SEED_MASK;
        shifts += 1;
    }
    seed
}

/// The fraction, in 2^-32 units, `RND(1)` makes of `seed`'s low word: each
/// byte exclusive-ored with those below it and its top bit inverted, as
/// genuine BASIC's are recorded.
const fn fraction(seed: u64) -> u32 {
    let low = (seed & LOW_WORD) as u32;
    low ^ (low << 8) ^ (low << 16) ^ (low << 24) ^ 0x8080_8080
}

/// Whether `ON ERROR` traps an error of `number`.
const fn trappable(number: u32, from_os: bool) -> bool {
    number != 0 || from_os
}

/// The warnings a typed line gives, in TAIRiX Bridge's own words.
const WARN_QUOTE: &[u8] = b"Warning: unmatched quotes";
const WARN_BRACKETS: &[u8] = b"Warning: unmatched brackets";
const WARN_LINE_NUMBER: &[u8] = b"Warning: line number too big";

#[cfg(test)]
mod tests {
    use super::*;

    /// What `RND(-1)` seeds the generator with.
    const MINUS_ONE: u64 = 0xFFFF_FFFF;

    #[test]
    fn draws_follow_genuine_basic_s_from_a_reseed() {
        let first = stepped(MINUS_ONE);
        let second = stepped(first);
        assert_eq!(
            (first & LOW_WORD, second & LOW_WORD),
            (0x8000_07FF, 0xC07F_FFF8)
        );
        assert_eq!(stepped(0x8000_0001) & LOW_WORD, 0x4000_1400);
    }

    #[test]
    fn fractions_are_the_recorded_ones() {
        assert_eq!(fraction(MINUS_ONE), 0x807F_807F, "RND(0) after RND(-1)");
        assert_eq!(fraction(stepped(MINUS_ONE)), 0xF878_787F);
        assert_eq!(fraction(stepped(stepped(MINUS_ONE))), 0x38F8_8778);
        assert_eq!(fraction(stepped(0x8000_0000)), 0xC484_8480);
        assert_eq!(fraction(stepped(0x8000_0008)), 0x4000_0084);
    }
}

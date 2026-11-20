//! Output: characters through `OS_WriteC`, `COUNT` kept as BASIC V keeps
//! it, `WIDTH`'s wrap, and numbers laid out under `@%` for `PRINT`.

use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{os as swi, X};

use super::{Machine, Step};
use crate::text::Format;
use crate::value::{Text, Value};
use crate::workspace as ws;

impl Machine<'_, '_> {
    /// `bytes` written as characters, `COUNT` moved on and `WIDTH`'s line
    /// wrapped.
    pub(crate) fn output(&mut self, bytes: &[u8]) -> Step {
        let width = self.os.read_word(ws::WIDTHLOC.at())?;
        let mut count = self.os.read_word(ws::TALLY.at())?;
        for &b in bytes {
            if width != u32::MAX && count > width && b >= b' ' {
                self.write_raw(b"\n\r")?;
                count = 0;
            }
            self.write_raw(&[b])?;
            count = match b {
                b'\r' => 0,
                b'\n' => count,
                _ => count.wrapping_add(1),
            };
        }
        Ok(self.os.write_word(ws::TALLY.at(), count)?)
    }

    /// A newline as `PRINT` writes one: a line feed and a carriage return.
    pub(crate) fn newline(&mut self) -> Step {
        self.output(b"\n\r")
    }

    /// `bytes` to `OS_WriteC`, `COUNT` untouched: VDU codes.
    pub(crate) fn write_raw(&mut self, bytes: &[u8]) -> Step {
        for &b in bytes {
            let mut regs = Regs::default();
            regs.r[0] = u32::from(b);
            self.os.perform(X | swi::WRITE_C, &mut regs)?;
        }
        Ok(())
    }

    /// The VDU codes `codes` written.
    pub(crate) fn vdu_codes(&mut self, codes: &[u8]) -> Step {
        self.write_raw(codes)
    }

    /// `COUNT`.
    pub(crate) fn count(&mut self) -> Step<u32> {
        Ok(self.os.read_word(ws::TALLY.at())?)
    }

    /// A number as `PRINT` writes it under `@%`, right-justified in its
    /// field where `padded`; in hexadecimal where `hex`.
    pub(crate) fn print_number(&mut self, value: &Value, padded: bool, hex: bool) -> Step {
        let at = self.os.read_word(ws::AT_PERCENT.at())?;
        let text = if hex {
            let n = self.int_value(value)?;
            super::functions::hex_text(n.cast_unsigned())
        } else {
            self.number_text(value, Format::of(at))?
        };
        let width = (at & 0xFF) as usize;
        if padded && text.as_bytes().len() < width {
            let pad = width - text.as_bytes().len();
            self.output_spaces(pad)?;
        }
        self.output(text.as_bytes())
    }

    /// `n` spaces written.
    pub(crate) fn output_spaces(&mut self, n: usize) -> Step {
        let spaces = [b' '; 16];
        let mut left = n;
        while left > 0 {
            let chunk = left.min(spaces.len());
            self.output(&spaces[..chunk])?;
            left -= chunk;
        }
        Ok(())
    }

    /// A string written.
    pub(crate) fn print_text(&mut self, text: &Text) -> Step {
        self.output(text.as_bytes())
    }
}

//! Character output (`plans/RISCOS-EMULATOR.md` §4.5): `OS_WriteC` and
//! its kin through `WrchV`, whose default sends each character to the streams
//! `OS_Byte` 3 selects — the VDU drivers, the spool file, the serial port
//! and the printer.

use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;

use crate::byte::var;
use crate::os::{Entry, Fail, Os, Regs, Unwind};
use crate::vectors::{self, Carry};

/// `OS_Byte` 3's bits: what each turns off or on.
mod destination {
    /// Send to the serial port.
    pub(super) const SERIAL: u8 = 1 << 0;
    /// Keep the VDU drivers out.
    pub(super) const NO_VDU: u8 = 1 << 1;
    /// Keep the printer out.
    pub(super) const NO_PRINTER: u8 = 1 << 2;
    /// Keep the printer out but for a character sent by `VDU 1`.
    pub(super) const NO_PRINTER_BUT_VDU_1: u8 = 1 << 6;
    /// Print every character, whatever the VDU drivers say.
    pub(super) const PRINT_ALL: u8 = 1 << 3;
    /// Keep the spool file out.
    pub(super) const NO_SPOOL: u8 = 1 << 4;
    /// Send the VDU stream through VDUXV instead.
    pub(super) const VDUX: u8 = 1 << 5;
}

/// An OS SWI the VDU drivers perform, by number.
pub type VduSwi = for<'a, 'b, 'c> fn(&'a mut Os<'b>, u32, &'c mut Regs) -> Result<(), Fail>;

/// A reason of `OS_Byte` or `OS_Word` the VDU drivers answer: none if the
/// reason is not theirs.
pub type VduReason = for<'a, 'b, 'c> fn(&'a mut Os<'b>, &'c mut Regs) -> Option<Result<(), Fail>>;

/// The VDU drivers, as the kernel reaches them: the part of the session
/// that has them gives these when the session is made.
#[derive(Clone, Copy, Debug)]
pub struct VduDrivers {
    /// Takes a byte of the VDU stream.
    pub write: for<'a, 'b> fn(&'a mut Os<'b>, u8) -> Result<(), Fail>,
    /// Whether the stream is part way through a control sequence's
    /// parameters, where `OS_ReadLine` prints a key rather than storing it.
    pub queued: for<'a, 'b> fn(&'a mut Os<'b>) -> bool,
    /// Whether VDU 2 has turned the printer on.
    pub printing: for<'a, 'b> fn(&'a mut Os<'b>) -> bool,
    /// The OS SWIs the VDU drivers perform, by number.
    pub swi: Option<VduSwi>,
    /// Their reasons of `OS_Byte`.
    pub byte: Option<VduReason>,
    /// Their reasons of `OS_Word`.
    pub word: Option<VduReason>,
}

/// The name the serial stream opens.
const SERIAL_STREAM: &[u8] = b"Serial#Buffer2:\0";

impl Os<'_> {
    /// `OS_WriteC`: R0's character through `WrchV`.
    pub(crate) fn write_c_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::WRCH, regs, Entry::SupervisorIrq, Carry::Caller)
    }

    /// Writes `c` as `OS_WriteC` does.
    pub(crate) fn write_c(&mut self, c: u8) -> Result<(), Fail> {
        let mut regs = Regs::new([u32::from(c), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.vectored(
            vectors::WRCH,
            &mut regs,
            Entry::SupervisorIrq,
            Carry::Caller,
        )
    }

    /// Writes each of `text`.
    pub(crate) fn write_text(&mut self, text: &[u8]) -> Result<(), Fail> {
        for &c in text {
            self.write_c(c)?;
        }
        Ok(())
    }

    /// Writes `text` and a newline.
    pub(crate) fn write_line(&mut self, text: &[u8]) -> Result<(), Fail> {
        self.write_text(text)?;
        self.write_text(b"\n\r")
    }

    /// Writes the string at `start` to its zero, a byte at a time so a
    /// long one costs no host memory: the address past the zero.
    fn write_until_zero(&mut self, start: u32) -> Result<u32, Fail> {
        let mut at = start;
        loop {
            let c = self.read_byte(at)?;
            at = at.wrapping_add(1);
            if c == 0 {
                return Ok(at);
            }
            self.write_c(c)?;
            self.yield_point()?;
        }
    }

    /// `OS_WriteS`: the string after the `SWI`, to its zero; the caller
    /// resumes at the next word past it.
    pub(crate) fn write_s(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        let start = self.world.processor.cpu().pc();
        let after = self.write_until_zero(start)?;
        let cpu = self.world.processor.cpu();
        let resume = if cpu.cpsr() & psr::T != 0 {
            (after + 1) & !1
        } else {
            (after + 3) & !3
        };
        cpu.set_reg(Reg::PC, resume);
        Ok(())
    }

    /// `OS_Write0`: R0's string to its zero; R0 past the zero.
    pub(crate) fn write_0(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        regs.r[0] = self.write_until_zero(regs.r[0])?;
        Ok(())
    }

    /// `OS_NewLine`: a line feed and a carriage return.
    pub(crate) fn new_line(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        self.write_text(b"\n\r")
    }

    /// `OS_WriteN`: R1 bytes from R0.
    pub(crate) fn write_n(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        for i in 0..regs.r[1] {
            let c = self.read_byte(regs.r[0].wrapping_add(i))?;
            self.write_c(c)?;
            self.yield_point()?;
        }
        Ok(())
    }

    /// `WrchV`'s default: the character to each stream `OS_Byte` 3 selects.
    pub(crate) fn default_wrch(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let c = regs.byte(0);
        let selected = self.byte_var(var::WRCH_DEST);
        let spool = self.byte_var(var::SPOOL_HANDLE);
        if spool != 0 && selected & destination::NO_SPOOL == 0 {
            let mut put = Regs::new([
                u32::from(c),
                u32::from(spool),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            self.perform(crate::swi::X | crate::swi::os::BPUT, &mut put)?;
        }
        if selected & destination::SERIAL != 0 {
            self.serial_out(c)?;
        }
        if selected & destination::NO_VDU == 0 {
            if selected & destination::VDUX != 0 {
                let mut vdux = Regs::new([u32::from(c), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.call_vector(vectors::VDUX, &mut vdux, Entry::Supervisor)?;
            } else if let Some(vdu) = self.world.vdu {
                (vdu.write)(self, c)?;
            }
        }
        let printing = self.world.vdu.is_some_and(|vdu| (vdu.printing)(self));
        if self.printer_selected() && (printing || selected & destination::PRINT_ALL != 0) {
            self.printer_out(c, false)?;
        }
        Ok(())
    }

    /// Whether `OS_Byte` 3 lets characters reach the printer.
    fn printer_selected(&mut self) -> bool {
        let off = destination::NO_PRINTER | destination::NO_PRINTER_BUT_VDU_1;
        self.byte_var(var::WRCH_DEST) & off == 0
    }

    /// The serial stream: opened on the first character, and turned off if
    /// it cannot be.
    fn serial_out(&mut self, c: u8) -> Result<(), Fail> {
        if self.world.serial_out == 0 {
            let name = self.scratch_text(SERIAL_STREAM)?;
            let mut open = Regs::new([
                crate::swi::os::find::OUTPUT,
                name,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            let opened = self.perform(crate::swi::X | crate::swi::os::FIND, &mut open);
            if opened.is_err() || open.r[0] == 0 {
                let selected = self.byte_var(var::WRCH_DEST);
                self.set_byte_var(var::WRCH_DEST, selected & !destination::SERIAL);
                return opened;
            }
            self.world.serial_out = open.r[0];
        }
        let mut put = Regs::new([
            u32::from(c),
            self.world.serial_out,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        self.perform(crate::swi::X | crate::swi::os::BPUT, &mut put)
    }

    /// The printer stream: the file `PrinterType$n` names for the
    /// configured printer type, opened on the first character; nothing if
    /// the variable is unset. The ignore character is held back unless
    /// `forced`.
    fn printer_out(&mut self, c: u8, forced: bool) -> Result<(), Fail> {
        let ignore = self.byte_var(var::PRINTER_IGNORE);
        let ignoring = self.byte_var(var::NO_IGNORE) & 0x80 == 0;
        if !forced && ignoring && c == ignore {
            return Ok(());
        }
        if self.world.printer == 0 {
            let printer_type = self.byte_var(var::PRINTER_TYPE);
            let mut variable = *b"PrinterType$\0\0\0\0";
            let digits = crate::convert::Decimal::of(i32::from(printer_type));
            let digits = digits.as_bytes();
            variable[12..12 + digits.len()].copy_from_slice(digits);
            let Some(file) = self.variable_text(&variable[..12 + digits.len()])? else {
                return Ok(());
            };
            let mut name = file;
            if name.try_reserve(1).is_err() {
                return Err(self.no_memory());
            }
            name.push(0);
            let at = self.scratch_text(&name)?;
            let mut open = Regs::new([
                crate::swi::os::find::OUTPUT,
                at,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            if let Err(fail) = self.perform(crate::swi::X | crate::swi::os::FIND, &mut open) {
                if let Fail::Error(_) = fail {
                    self.stop_printing()?;
                }
                return Err(fail);
            }
            if open.r[0] == 0 {
                return Ok(());
            }
            self.world.printer = open.r[0];
        }
        let mut put = Regs::new([
            u32::from(c),
            self.world.printer,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        if let Err(fail) = self.perform(crate::swi::X | crate::swi::os::BPUT, &mut put) {
            if let Fail::Error(_) = fail {
                self.stop_printing()?;
            }
            return Err(fail);
        }
        Ok(())
    }

    /// Printing stops after an error: the printer stream closed and
    /// `OS_Byte` 3's printing of every character turned off.
    fn stop_printing(&mut self) -> Result<(), Unwind> {
        let selected = self.byte_var(var::WRCH_DEST);
        self.set_byte_var(var::WRCH_DEST, selected & !destination::PRINT_ALL);
        if self.world.printer != 0 {
            let mut close = Regs::new([0, self.world.printer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.world.printer = 0;
            self.perform_quietly(crate::swi::X | crate::swi::os::FIND, &mut close)?;
        }
        Ok(())
    }

    /// `OS_PrintChar`: R0's character to the printer stream, the ignore
    /// character too, where `OS_Byte` 3 lets the printer be used.
    pub(crate) fn print_char(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if !self.printer_selected() {
            return Ok(());
        }
        self.printer_out(regs.byte(0), true)
    }

    /// Closes the printer stream (`OS_Byte` 5 and 123).
    pub(crate) fn close_printer(&mut self) -> Result<(), Fail> {
        if self.world.printer != 0 {
            let handle = core::mem::take(&mut self.world.printer);
            let mut close = Regs::new([0, handle, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.perform(crate::swi::X | crate::swi::os::FIND, &mut close)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

//! `OS_Word` (`plans/RISCOS-EMULATOR.md` §4.5): every reason RISC OS 5
//! implements, through `WordV`; the VDU drivers' own reasons go to them, and a
//! reason no one implements is offered as `Service_UKWord`, unclaimed
//! leaving every register as it was.

use tairix_riscos_arm::psr;

use crate::os::{len32, Entry, Fail, Os, Regs};
use crate::territory::{
    CONVERT_DATE_AND_TIME, ORDINALS_TO_TIME, READ_CALENDAR_INFORMATION, STRING_TO_ORDINALS,
    TIME_TO_ORDINALS,
};
use crate::vectors::{self, Carry};

/// The territory `-1` names: the current one.
const CURRENT: u32 = u32::MAX;
/// `OS_Word` 14's string form.
const LONG_FORMAT: &[u8] = b"%w3,%dy %m3 %ce%yr.%24:%mi:%se\0";

impl Os<'_> {
    /// `OS_Word`, through `WordV` with interrupts disabled; C and V as the
    /// vector leaves them.
    pub(crate) fn word(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::WORD, regs, Entry::SupervisorNoIrq, Carry::Vector)
    }

    /// `WordV`'s default.
    pub(crate) fn default_word(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if let Some(handled) = self
            .world
            .vdu
            .and_then(|vdu| vdu.word)
            .and_then(|word| word(self, regs))
        {
            return handled;
        }
        let reason = regs.r[0];
        let block = regs.r[1];
        let handled = match reason {
            0 => {
                self.word_read_line(regs)?;
                true
            }
            1 => {
                let time = self.time();
                self.write_five(block, time)?;
                true
            }
            2 => {
                let time = self.read_five(block)?;
                self.set_time(time);
                true
            }
            3 => {
                let value = self.interval_timer();
                self.write_five(block, value)?;
                true
            }
            4 => {
                let value = self.read_five(block)?;
                self.set_interval_timer(value);
                true
            }
            7 => {
                let channel = self.read_word(block)?;
                let pitch = self.read_word(block + 4)?;
                let mut sound = Regs::new([channel, pitch, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.perform_quietly(
                    crate::swi::X | crate::swi::sound::CONTROL_PACKED,
                    &mut sound,
                )?;
                true
            }
            14 => self.read_clock(block)?,
            15 => self.write_clock(block)?,
            21 => {
                let reason = self.read_byte(block)?;
                self.mouse_word(reason, block)?
            }
            _ => false,
        };
        if handled {
            return Ok(());
        }
        let mut service = Regs::new([
            0,
            crate::service::UK_WORD,
            regs.r[0],
            regs.r[1],
            regs.r[2],
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        self.service(&mut service)?;
        if service.r[1] == 0 {
            regs.r[0] = service.r[2];
            regs.r[1] = service.r[3];
            regs.r[2] = service.r[4];
        }
        Ok(())
    }

    /// The five-byte value at `at`.
    pub(crate) fn read_five(&mut self, at: u32) -> Result<u64, Fail> {
        let mut bytes = [0; 8];
        self.read_bytes(at, &mut bytes[..5])?;
        Ok(u64::from_le_bytes(bytes))
    }

    /// Writes the five-byte `value` at `at`.
    pub(crate) fn write_five(&mut self, at: u32, value: u64) -> Result<(), Fail> {
        self.write_bytes(at, &value.to_le_bytes()[..5])
    }

    /// `OS_Word` 0: a line read as `OS_ReadLine32` reads one, into the
    /// buffer the block's sixteen-bit address names.
    fn word_read_line(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let block = regs.r[1];
        let mut address = [0; 2];
        self.read_bytes(block, &mut address)?;
        let mut limits = [0; 3];
        self.read_bytes(block + 2, &mut limits)?;
        let mut line = Regs::new([
            u32::from(u16::from_le_bytes(address)),
            u32::from(limits[0]),
            u32::from(limits[1]),
            u32::from(limits[2]),
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
        line.flags = regs.flags;
        self.read_line32(&mut line)?;
        regs.r[2] = line.r[1];
        regs.set_flag(psr::C, line.flag(psr::C));
        Ok(())
    }

    /// `OS_Word` 14: the real-time clock read as a string, as BCD, a BCD
    /// time turned into a string, or the five-byte UTC time. A sub-reason
    /// past those is not the kernel's.
    fn read_clock(&mut self, block: u32) -> Result<bool, Fail> {
        let sub = self.read_byte(block)?;
        match sub {
            0 => {
                let now = self.real_time();
                self.clock_string(block, now)?;
            }
            1 => {
                let now = self.real_time();
                let ordinals = self.ordinals(now)?;
                let [_, second, minute, hour, day, month, year, weekday, _] = ordinals;
                let bcd = |n: u32| -> u8 {
                    let n = (n % 100) as u8;
                    ((n / 10) << 4) | (n % 10)
                };
                let bytes = [
                    bcd(year),
                    bcd(month),
                    bcd(day),
                    bcd(weekday),
                    bcd(hour),
                    bcd(minute),
                    bcd(second),
                ];
                self.write_bytes(block, &bytes)?;
            }
            2 => {
                let mut bcd = [0; 8];
                self.read_bytes(block + 1, &mut bcd[..7])?;
                let decimal = |b: u8| u32::from(b >> 4) * 10 + u32::from(b & 0xF);
                let ordinals = [
                    0,
                    decimal(bcd[6]),
                    decimal(bcd[5]),
                    decimal(bcd[4]),
                    decimal(bcd[2]),
                    decimal(bcd[1]),
                    1900 + decimal(bcd[0]),
                ];
                let time = self.time_of_ordinals(&ordinals)?;
                self.clock_string(block, time)?;
            }
            3 => {
                let now = self.real_time();
                self.write_five(block, now)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Writes `time` as `OS_Word` 14's 25-byte string at `block`, ending in
    /// a carriage return.
    fn clock_string(&mut self, block: u32, time: u64) -> Result<(), Fail> {
        let scratch = self.world.scratch;
        self.write_five(scratch, time)?;
        let format = scratch + 8;
        self.write_bytes(format, LONG_FORMAT)?;
        let out = format + len32(LONG_FORMAT.len());
        let mut convert = Regs::new([CURRENT, scratch, out, 32, format, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform(crate::swi::X | CONVERT_DATE_AND_TIME, &mut convert)?;
        let mut text = [0; 24];
        self.read_bytes(out, &mut text)?;
        self.write_bytes(block, &text)?;
        self.write_byte(block + 24, b'\r')
    }

    /// `time`'s ordinals in the current territory: centiseconds, seconds,
    /// minutes, hours, day, month, year, day of week and day of year.
    fn ordinals(&mut self, time: u64) -> Result<[u32; 9], Fail> {
        let scratch = self.world.scratch;
        self.write_five(scratch, time)?;
        let out = scratch + 8;
        let mut convert = Regs::new([CURRENT, scratch, out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform(crate::swi::X | TIME_TO_ORDINALS, &mut convert)?;
        let mut ordinals = [0; 9];
        for (i, slot) in (0..).zip(ordinals.iter_mut()) {
            *slot = self.read_word(out + 4 * i)?;
        }
        Ok(ordinals)
    }

    /// The five-byte time of local ordinals (centiseconds to year).
    fn time_of_ordinals(&mut self, ordinals: &[u32; 7]) -> Result<u64, Fail> {
        let scratch = self.world.scratch;
        for (i, &value) in (0..).zip(ordinals) {
            self.write_word(scratch + 8 + 4 * i, value)?;
        }
        let mut convert = Regs::new([CURRENT, scratch, scratch + 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform(crate::swi::X | ORDINALS_TO_TIME, &mut convert)?;
        self.read_five(scratch)
    }

    /// `OS_Word` 15: the real-time clock set from five bytes of UTC, or from
    /// a local time, date or both as a string. A string the territory cannot
    /// read changes nothing.
    fn write_clock(&mut self, block: u32) -> Result<bool, Fail> {
        let sub = self.read_byte(block)?;
        let time = match sub {
            5 => self.read_five(block.wrapping_add(1))?,
            8 | 15 | 24 => {
                let length = self.clock_string_length(sub)?;
                let (text, _) = self.read_until(block.wrapping_add(1), length, |_| false)?;
                match self.time_from_string(sub, &text) {
                    Ok(time) => time,
                    Err(Fail::Error(_)) => return Ok(true),
                    Err(unwind) => return Err(unwind),
                }
            }
            _ => return Ok(false),
        };
        self.set_real_time(time);
        Ok(true)
    }

    /// How long `OS_Word` 15's string of reason `sub` is: eight for a time;
    /// for a date, seven and the longest weekday, day and month the territory
    /// writes, and nine more with a time.
    fn clock_string_length(&mut self, sub: u8) -> Result<usize, Fail> {
        if sub == 8 {
            return Ok(8);
        }
        let scratch = self.world.scratch;
        let now = self.real_time();
        self.write_five(scratch, now)?;
        let info = scratch + 8;
        let mut calendar = Regs::new([CURRENT, scratch, info, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform(crate::swi::X | READ_CALENDAR_INFORMATION, &mut calendar)?;
        let mut longest = 0;
        for word in [24, 28, 40] {
            longest += self.read_word(info + word)? as usize;
        }
        Ok(if sub == 15 { 7 } else { 16 } + longest)
    }

    /// The time a local time, date or date and time string names, the
    /// missing half the current time's.
    fn time_from_string(&mut self, sub: u8, text: &[u8]) -> Result<u64, Fail> {
        let scratch = self.world.scratch;
        let string = scratch + 40;
        self.write_terminated(string, text, 0, text.len() + 1)?;
        let which = match sub {
            8 => 1,
            15 => 2,
            _ => 3,
        };
        let mut parse = Regs::new([
            CURRENT,
            which,
            string,
            scratch + 8,
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
        self.perform(crate::swi::X | STRING_TO_ORDINALS, &mut parse)?;
        let mut parsed = [0; 7];
        for (i, slot) in (0..).zip(parsed.iter_mut()) {
            *slot = self.read_word(scratch + 8 + 4 * i)?;
        }
        if sub != 24 {
            let now = self.real_time();
            let current = self.ordinals(now)?;
            let (keep, from) = if sub == 8 { (4..7, 4..7) } else { (0..4, 0..4) };
            for (slot, value) in parsed[keep].iter_mut().zip(&current[from]) {
                *slot = *value;
            }
        }
        self.time_of_ordinals(&parsed)
    }
}

//! `OS_Byte` (`plans/RISCOS-EMULATOR.md` §4.5): the reasons the PRM
//! documents, through `ByteV`, and the `OS_Byte` variables, kept in zero page
//! where `OS_Byte` 166 and 167 tell software they are.
//!
//! A reason concerning hardware the emulator lacks answers as though that
//! hardware were absent. One the kernel does not know is offered as
//! `Service_UKByte` and, unclaimed, is "Unknown command".

use tairix_riscos_arm::psr;

use crate::error::{Error, BAD_COMMAND};
use crate::input::{Continuation, KEYBOARD, MOUSE};
use crate::memory::layout::HIGH_ZERO_PAGE;
use crate::os::{Entry, Fail, Os, Regs, Unwind};
use crate::vectors::{self, Carry};

/// The `OS_Byte` variables used by name.
pub(crate) mod var {
    /// The first: what `OS_Byte` 166 reports the table from.
    pub(crate) const FIRST: u8 = 0xA6;
    /// The input stream: 0 the keyboard, 1 the serial port.
    pub(crate) const INPUT_STREAM: u8 = 0xB1;
    /// The serial port's raw input mode.
    pub(crate) const RS423_MODE: u8 = 0xB5;
    /// Whether the printer ignore character is honoured: bit 7 clear.
    pub(crate) const NO_IGNORE: u8 = 0xB6;
    /// The auto-repeat delay and rate.
    pub(crate) const REPEAT_DELAY: u8 = 0xC4;
    pub(crate) const REPEAT_RATE: u8 = 0xC5;
    /// The `*Exec` file's handle.
    pub(crate) const EXEC_HANDLE: u8 = 0xC6;
    /// The `*Spool` file's handle.
    pub(crate) const SPOOL_HANDLE: u8 = 0xC7;
    /// Escape's effect on Break: bit 0 ignores the escape character.
    pub(crate) const ESCAPE_BREAK: u8 = 0xC8;
    /// Keyboard input disabled.
    pub(crate) const KEYBOARD_DISABLE: u8 = 0xC9;
    /// The keyboard's status.
    pub(crate) const KEYBOARD_STATUS: u8 = 0xCA;
    /// What is left of a function key's expansion.
    pub(crate) const SOFT_KEY_LEN: u8 = 0xD8;
    /// The page-mode line count.
    pub(crate) const PAGE_LINES: u8 = 0xD9;
    /// The VDU queue's count.
    pub(crate) const VDU_QUEUE: u8 = 0xDA;
    /// The code Tab gives.
    pub(crate) const TAB_CHAR: u8 = 0xDB;
    /// The escape character.
    pub(crate) const ESCAPE_CHAR: u8 = 0xDC;
    /// What Break does with each of its modifiers.
    pub(crate) const BREAK_ACTION: u8 = 0xF7;
    /// The actions of the buffer codes &C0 to &FF, a variable each sixteen.
    pub(crate) const BUFFER_CODE_BASE: u8 = 0xDD;
    /// The actions of the function keys &80 to &BF, likewise.
    pub(crate) const RED_KEY_BASE: u8 = 0xE1;
    /// The escape character is an ordinary character.
    pub(crate) const ESCAPE_ACTION: u8 = 0xE5;
    /// Acknowledging escape only clears it.
    pub(crate) const ESCAPE_EFFECT: u8 = 0xE6;
    /// `OS_Byte` 3's output streams.
    pub(crate) const WRCH_DEST: u8 = 0xEC;
    /// `OS_Byte` 4's cursor key mode.
    pub(crate) const CURSOR_EDIT: u8 = 0xED;
    /// What the keypad's codes are counted from, in place of `0`.
    pub(crate) const KEYPAD_BASE: u8 = 0xEE;
    /// The configured country.
    pub(crate) const COUNTRY: u8 = 0xF0;
    /// The printer type.
    pub(crate) const PRINTER_TYPE: u8 = 0xF5;
    /// The printer ignore character.
    pub(crate) const PRINTER_IGNORE: u8 = 0xF6;
    /// Whether Shift and Ctrl leave the keypad's codes from &80 alone:
    /// nought moves them.
    pub(crate) const KEYPAD_SHIFTS: u8 = 0xFE;
    /// Start-up options.
    pub(crate) const START_OPTIONS: u8 = 0xFF;
}

/// Where the `OS_Byte` variables are in zero page: variable &A6 here, so
/// variable n is at &8FA + n, the address `OS_Byte` 166 and 167 give, as
/// genuine RISC OS 5 keeps them.
const TABLE: u32 = 0x9A0;
/// The variables at reset, from &A6 to &FF, and the serial input handle
/// beside &FF, before the configuration's are read from CMOS.
const INITIAL: [u8; 0x5B] = [
    0, 0, // &A6, &A7: read as the table's address
    0, 0, 0, 0, 0, 0, 0, 0, // &A8–&AF
    0, 0, 0xFF, 0, 0x80, 1, 0, 0, // &B0–&B7
    0, 0, 0, 0xFF, 4, 4, 0, 0xFF, // &B8–&BF
    0x42, 0x19, 0x19, 0x19, 0x32, 8, 0, 0, // &C0–&C7
    0, 0, 0x34, 0x11, 0, 0, 0, 0, // &C8–&CF
    0, 0, 0, 1, 0x90, 0x64, 6, 0x81, // &D0–&D7
    0, 0, 0, 9, 0x1B, 1, 0xD0, 0xE0, // &D8–&DF
    0xF0, 1, 0x80, 0x90, 0, 0, 0, 0, // &E0–&E7
    0, 0, 0, 0, 0, 0, 0x30, 1, // &E8–&EF
    0, 0, 0x64, 5, 0xFF, 1, 0x0A, 1, // &F0–&F7
    0, 0, 0, 0, 0xFF, 1, 0x0F, 8, // &F8–&FF
    0, // the serial input handle
];

/// The OS identifier `OS_Byte` 129 gives: RISC OS 5's.
pub(crate) const OS_IDENTIFIER: u32 = 0xAA;
/// The version number `OS_Byte` 0 with R1 non-zero gives.
const OS_VERSION: u32 = 6;
/// `Service_UKByte`.
const UK_BYTE: u32 = 0x07;

/// `OS_Byte` 0's version error: the number RISC OS gives it, the text
/// naming the release and the emulator.
const VERSION: Error = Error {
    number: 0xF7,
    token: "SystemVersion",
};

impl Os<'_> {
    /// The address of `OS_Byte` variable `number` in high zero page.
    const fn byte_address(number: u8) -> u32 {
        HIGH_ZERO_PAGE + TABLE + (number - var::FIRST) as u32
    }

    /// `OS_Byte` variable `number`.
    pub(crate) fn byte_var(&mut self, number: u8) -> u8 {
        self.peek_byte(Self::byte_address(number)).unwrap_or(0)
    }

    /// Sets `OS_Byte` variable `number`.
    pub(crate) fn set_byte_var(&mut self, number: u8, value: u8) {
        let _ = self.poke_byte(Self::byte_address(number), value);
    }

    /// `OS_Byte` `number` called with R1, as a module calls it: its R1.
    pub(crate) fn os_byte(&mut self, number: u32, r1: u32) -> Result<u32, Fail> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[number, r1]);
        self.perform(crate::swi::X | crate::swi::os::BYTE, &mut regs)?;
        Ok(regs.r[1])
    }

    /// The variables at reset, then those the CMOS configures.
    pub(crate) fn reset_byte_vars(&mut self) {
        for (offset, &value) in (0..).zip(INITIAL.iter()) {
            let _ = self.poke_byte(HIGH_ZERO_PAGE + TABLE + offset, value);
        }
        let cmos = &self.world.cmos;
        let ignore = if cmos.read(crate::cmos::PRINT_SERIAL) & 2 != 0 {
            0x80
        } else {
            0
        };
        let delay = cmos.read(crate::cmos::KEY_DELAY);
        let rate = cmos.read(crate::cmos::KEY_REPEAT);
        let start = cmos.read(crate::cmos::START);
        let dbtb = cmos.read(crate::cmos::DBTB);
        let country = cmos.read(crate::cmos::COUNTRY);
        let printer = cmos.read(crate::cmos::PRINT_SERIAL) >> 5;
        let pig = cmos.read(crate::cmos::PRINTER_IGNORE);
        self.set_byte_var(var::NO_IGNORE, ignore);
        self.set_byte_var(var::REPEAT_DELAY, delay);
        self.set_byte_var(var::REPEAT_RATE, rate);
        let locks = if start & (1 << 5) != 0 {
            0x20
        } else if start & (1 << 4) != 0 {
            0x30
        } else {
            0xA0
        };
        let num_lock = if start & 0x80 != 0 { 0x04 } else { 0 };
        self.set_byte_var(var::KEYBOARD_STATUS, locks | num_lock);
        self.set_byte_var(0xD4, if dbtb & 2 != 0 { 0x90 } else { 0xD0 });
        self.set_byte_var(var::COUNTRY, country);
        self.set_byte_var(var::PRINTER_TYPE, printer);
        self.set_byte_var(var::PRINTER_IGNORE, pig);
        let boot = if dbtb & (1 << 4) != 0 { 0 } else { 8 };
        self.set_byte_var(var::START_OPTIONS, boot);
    }

    /// `OS_Byte`, through `ByteV` with interrupts disabled; the C and V the
    /// vector leaves.
    pub(crate) fn byte(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::BYTE, regs, Entry::SupervisorNoIrq, Carry::Vector)
    }

    /// `ByteV`'s default: the reason in R0's low byte, R0 and R3 returned as
    /// they came.
    pub(crate) fn default_byte(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let reason = regs.byte(0);
        if reason < 0x6A {
            regs.r[2] = 0;
        }
        let (r0, r3) = (regs.r[0], regs.r[3]);
        let result = self.byte_reason(reason, regs);
        regs.r[0] = r0;
        regs.r[3] = r3;
        result
    }

    #[expect(clippy::too_many_lines, reason = "one arm a reason")]
    fn byte_reason(&mut self, reason: u8, regs: &mut Regs) -> Result<(), Fail> {
        if let Some(handled) = self
            .world
            .vdu
            .and_then(|vdu| vdu.byte)
            .and_then(|byte| byte(self, regs))
        {
            return handled;
        }
        match reason {
            0 => {
                if regs.r[1] != 0 {
                    regs.r[1] = OS_VERSION;
                    regs.r[2] = 0;
                    return Ok(());
                }
                let release = crate::release::os_byte_0();
                return Err(self.error_with(VERSION, &[&release]));
            }
            1 => self.write_variable(0xF1, regs),
            2 => self.select_input(regs)?,
            3 => self.write_variable(var::WRCH_DEST, regs),
            4 => self.write_variable(var::CURSOR_EDIT, regs),
            5 => {
                if let Err(Fail::Unwind(unwind)) = self.close_printer() {
                    return Err(Fail::Unwind(unwind));
                }
                if !self.escape_set() {
                    self.write_variable(var::PRINTER_TYPE, regs);
                }
            }
            6 => {
                self.set_byte_var(var::NO_IGNORE, 0);
                self.write_variable(var::PRINTER_IGNORE, regs);
            }
            7 | 8 => {
                let mut serial = Regs::new([
                    5 + u32::from(reason - 7),
                    regs.r[1],
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
                self.call_vector(vectors::SERIAL, &mut serial, Entry::SupervisorNoIrq)?;
                regs.r[1] = serial.r[1];
                regs.r[2] = serial.r[2];
            }
            11 => self.write_variable(var::REPEAT_DELAY, regs),
            12 => {
                if regs.r[1] != 0 {
                    self.write_variable(var::REPEAT_RATE, regs);
                } else {
                    let delay = self.world.cmos.read(crate::cmos::KEY_DELAY);
                    let rate = self.world.cmos.read(crate::cmos::KEY_REPEAT);
                    self.set_byte_var(var::REPEAT_DELAY, delay);
                    self.set_byte_var(var::REPEAT_RATE, rate);
                    regs.r[1] = 0;
                    regs.r[2] = 0;
                }
            }
            13 | 14 => {
                self.byte_event(reason == 14, regs);
                return Ok(());
            }
            15 => {
                if regs.r[1] == 0 {
                    for buffer in 0..=MOUSE {
                        self.flush(buffer)?;
                    }
                } else {
                    let input = u32::from(self.byte_var(var::INPUT_STREAM) & 1);
                    self.flush(input)?;
                }
            }
            18 => self.clear_soft_keys()?,
            21 => self.flush(regs.r[1])?,
            70 => self.set_country(regs)?,
            71 => self.alphabet_or_keyboard(regs)?,
            118 => self.refresh_leds(true)?,
            120 => self.write_keys_down(regs)?,
            121 | 122 => {
                // 122 scans past Shift, Ctrl, Alt and the mouse: from 16.
                if reason == 122 {
                    regs.r[1] = 16;
                }
                self.key_scan(regs)?;
            }
            123 => self.close_printer()?,
            124 => self.set_escape(false)?,
            125 => self.set_escape(true)?,
            126 => self.acknowledge_escape(regs)?,
            127 => {
                let mut args = Regs::new([5, regs.r[1], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.perform(crate::swi::X | crate::swi::os::ARGS, &mut args)?;
                regs.r[1] = args.r[2];
            }
            128 => return self.adval(regs),
            129 => return self.inkey(regs),
            130 => {
                regs.r[1] = 0xFF;
                regs.r[2] = 0xFF;
            }
            131 => {
                regs.r[1] = 0;
                regs.r[2] = u32::from(self.byte_var(0xB4));
            }
            138 => {
                let full = !self.insert(regs.r[1], regs.byte(2))?;
                regs.set_flag(psr::C, full);
            }
            139 => {
                let mut opt = Regs::new([0, regs.r[1], regs.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.perform(crate::swi::X | crate::swi::os::FS_CONTROL, &mut opt)?;
            }
            143 => {
                let mut service =
                    Regs::new([0, regs.r[1], regs.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.service(&mut service)?;
                regs.r[1] = service.r[1];
                regs.r[2] = service.r[2];
            }
            145 | 152 => match self.remove(regs.r[1], reason == 152)? {
                Some(byte) => {
                    regs.r[2] = u32::from(byte);
                    regs.set_flag(psr::C, false);
                }
                None => regs.set_flag(psr::C, true),
            },
            153 => {
                let full = !self.insert_checked(regs.r[1], regs.byte(2))?;
                regs.set_flag(psr::C, full);
            }
            156 => {
                let mut serial = Regs::new([0, regs.r[1], regs.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.call_vector(vectors::SERIAL, &mut serial, Entry::SupervisorNoIrq)?;
                regs.r[1] = serial.r[1];
                regs.r[2] = serial.r[2];
            }
            157 => {
                let mut put = Regs::new([regs.r[1], regs.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.perform(crate::swi::X | crate::swi::os::BPUT, &mut put)?;
            }
            161 => self.byte_read_cmos(regs),
            162 => self.byte_write_cmos(regs),
            166 | 167 => {
                // Two bytes hold it, so it is the place in zero page, which
                // is at nought for a 26-bit task.
                let address = TABLE - u32::from(var::FIRST);
                if reason == 166 {
                    regs.r[1] = address & 0xFF;
                    regs.r[2] = address >> 8;
                } else {
                    regs.r[1] = address >> 8;
                    regs.r[2] = u32::from(self.byte_var(0xA8));
                }
            }
            0xA8..=0xFF => self.variable_rule(reason, regs),
            _ => return self.unknown_byte(regs),
        }
        Ok(())
    }

    /// The country a setting of nought means: the configured one, else the
    /// keyboard's own where it names one.
    fn default_country(&mut self, number: u32) -> u32 {
        if number != 0 {
            return number;
        }
        let configured = u32::from(self.world.cmos.read(crate::cmos::COUNTRY));
        if configured != 0 {
            return configured;
        }
        self.world
            .keyboard
            .id
            .filter(|&id| id < 0x20)
            .map_or(0, u32::from)
    }

    /// `Service_International` `reason` with R3 to R5: R4 its answer, if a
    /// module claimed it.
    fn international(
        &mut self,
        reason: u32,
        r3: u32,
        r4: u32,
        r5: u32,
    ) -> Result<Option<u32>, Unwind> {
        let mut service = Regs::default();
        service.r[1..6].copy_from_slice(&[crate::international::SERVICE, reason, r3, r4, r5]);
        self.service(&mut service)?;
        Ok((service.r[1] == 0).then_some(service.r[4]))
    }

    /// Every character of `alphabet` defined by `Service_International` 5:
    /// whether a module knows the alphabet.
    pub(crate) fn define_alphabet_fully(&mut self, alphabet: u32) -> Result<bool, Unwind> {
        let define = crate::international::reason::DEFINE;
        Ok(self.international(define, alphabet, 32, 255)?.is_some())
    }

    /// The keyboard selected, and the modules told.
    fn select_keyboard(&mut self, keyboard: u32, alphabet: u32) -> Result<(), Unwind> {
        self.world.country.keyboard = keyboard;
        self.world.country.key_alphabet = alphabet;
        self.international(
            crate::international::reason::KEYBOARD,
            keyboard,
            alphabet,
            0,
        )?;
        Ok(())
    }

    /// `OS_Byte` 70: the country read for 127, else set — its alphabet and
    /// keyboard with it, the font redefined — R1 the old one, nought if no
    /// module knows the new one.
    fn set_country(&mut self, regs: &mut Regs) -> Result<(), Unwind> {
        if regs.r[1] == 127 {
            regs.r[1] = u32::from(self.byte_var(var::COUNTRY));
            return Ok(());
        }
        let country = self.default_country(regs.r[1]);
        let reason = crate::international::reason::COUNTRY_ALPHABET;
        regs.r[2] = reason;
        let Some(alphabet) = self.international(reason, country, 0, 0)? else {
            regs.r[1] = 0;
            return Ok(());
        };
        let old = u32::from(self.byte_var(var::COUNTRY));
        self.set_byte_var(var::COUNTRY, u8::try_from(country).unwrap_or(0));
        self.world.country.alphabet = alphabet;
        self.select_keyboard(country, alphabet)?;
        let define = crate::international::reason::DEFINE;
        self.international(define, alphabet, 32, 255)?;
        regs.r[1] = old;
        regs.r[2] = define;
        Ok(())
    }

    /// `OS_Byte` 71: the alphabet read for 127 and the keyboard for 255; an
    /// alphabet set for 0 to 126, a country naming its own, and a keyboard
    /// for 128 more than its number; R1 the old one, nought if none answers.
    fn alphabet_or_keyboard(&mut self, regs: &mut Regs) -> Result<(), Unwind> {
        let country_alphabet = crate::international::reason::COUNTRY_ALPHABET;
        match regs.r[1] {
            127 => regs.r[1] = self.world.country.alphabet,
            255 => regs.r[1] = self.world.country.keyboard,
            asked @ 0..=126 => {
                let number = self.default_country(asked);
                let alphabet = self
                    .international(country_alphabet, number, 0, 0)?
                    .unwrap_or(number);
                regs.r[2] = crate::international::reason::DEFINE;
                if !self.define_alphabet_fully(alphabet)? {
                    regs.r[1] = 0;
                    return Ok(());
                }
                regs.r[1] = self.world.country.alphabet;
                self.world.country.alphabet = alphabet;
            }
            asked => {
                let keyboard = self.default_country(asked - 128);
                regs.r[2] = country_alphabet;
                let Some(alphabet) = self.international(country_alphabet, keyboard, 0, 0)? else {
                    regs.r[1] = 0;
                    return Ok(());
                };
                regs.r[1] = self.world.country.keyboard;
                self.select_keyboard(keyboard, alphabet)?;
            }
        }
        Ok(())
    }

    /// The keyboard and alphabet the kernel starts with, then the country
    /// CMOS configures, selected once the modules have started.
    pub(crate) fn start_country(&mut self) -> Result<(), Unwind> {
        self.world.country = Country::new();
        let mut regs = Regs::default();
        regs.r[1] = u32::from(self.byte_var(var::COUNTRY));
        self.set_country(&mut regs)
    }

    /// The reasons that write one variable: R1 its old value, R2 the next.
    fn write_variable(&mut self, number: u8, regs: &mut Regs) {
        let old = self.byte_var(number);
        self.set_byte_var(number, regs.byte(1));
        regs.r[1] = u32::from(old);
        regs.r[2] = if number == 0xFF {
            u32::from(self.peek_byte(Self::byte_address(0xFF) + 1).unwrap_or(0))
        } else {
            u32::from(self.byte_var(number + 1))
        };
    }

    /// `OS_Byte` &A8 to &FF: the variable becomes (old AND R2) EOR R1, R1
    /// its old value and R2 the next.
    fn variable_rule(&mut self, number: u8, regs: &mut Regs) {
        let old = self.byte_var(number);
        let new = (old & regs.byte(2)) ^ regs.byte(1);
        self.set_byte_var(number, new);
        regs.r[1] = u32::from(old);
        regs.r[2] = if number == 0xFF {
            u32::from(self.peek_byte(Self::byte_address(0xFF) + 1).unwrap_or(0))
        } else {
            u32::from(self.byte_var(number + 1))
        };
    }

    /// An `OS_Byte` the kernel does not know: offered as `Service_UKByte`,
    /// which a claimant answers in R2 to R4.
    fn unknown_byte(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mut service = Regs::new([
            0, UK_BYTE, regs.r[0], regs.r[1], regs.r[2], 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        self.service(&mut service)?;
        if service.r[1] == 0 {
            regs.r[0] = service.r[2];
            regs.r[1] = service.r[3];
            regs.r[2] = service.r[4];
            return Ok(());
        }
        Err(self.error(BAD_COMMAND))
    }

    /// `OS_Byte` 2: the input stream, the serial input opened or closed with
    /// it.
    fn select_input(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let old = self.byte_var(var::INPUT_STREAM);
        self.set_byte_var(var::INPUT_STREAM, regs.byte(1) & 1);
        let handle_at = Self::byte_address(0xFF) + 1;
        let handle = self.peek_byte(handle_at).unwrap_or(0);
        if regs.r[1] == 0 && handle != 0 {
            let mut close = Regs::new([0, u32::from(handle), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.perform_quietly(crate::swi::X | crate::swi::os::FIND, &mut close)?;
            let _ = self.poke_byte(handle_at, 0);
        } else if regs.r[1] != 0 && handle == 0 {
            let name = self.scratch_text(b"Devices#Buffer1:$.Serial\0")?;
            let mut open = Regs::new([0x40, name, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            if self.perform_quietly(crate::swi::X | crate::swi::os::FIND, &mut open)? {
                let _ = self.poke_byte(handle_at, open.byte(0));
            }
        }
        regs.r[1] = u32::from(old);
        Ok(())
    }

    /// Flushes `buffer`; the keyboard's and the serial input's also end any
    /// function key's expansion and the VDU queue.
    fn flush(&mut self, buffer: u32) -> Result<(), Unwind> {
        self.purge(buffer)?;
        if buffer == KEYBOARD || buffer == 1 {
            self.set_byte_var(var::SOFT_KEY_LEN, 0);
            self.set_byte_var(var::VDU_QUEUE, 0);
            self.world.input.end_soft_key();
        }
        Ok(())
    }

    /// `OS_Byte` 18: every function key's definition deleted.
    fn clear_soft_keys(&mut self) -> Result<(), Fail> {
        self.set_byte_var(var::SOFT_KEY_LEN, 0);
        self.world.input.end_soft_key();
        for key in 0..16 {
            let mut name = *b"Key$00";
            let digits = crate::convert::Decimal::of(key);
            let digits = digits.as_bytes();
            name[4..4 + digits.len()].copy_from_slice(digits);
            let at = self.scratch_text(&name[..4 + digits.len()])?;
            let mut unset = Regs::new([at, 0, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.perform_quietly(crate::swi::X | crate::swi::os::SET_VAR_VAL, &mut unset)?;
        }
        Ok(())
    }

    /// `OS_Byte` 126: an escape acknowledged — the sound stopped, the `*Exec`
    /// file closed and the buffers flushed — unless `OS_Byte` 230 asks only
    /// for it to be cleared; then it is cleared.
    fn acknowledge_escape(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let pending = self.escape_set();
        if pending && self.byte_var(var::ESCAPE_EFFECT) == 0 {
            let mut init = Regs::default();
            if self.perform(crate::swi::X | SOUND_QINIT, &mut init).is_ok() {
                for channel in (1..=8).rev() {
                    let mut stop = Regs::new([
                        0x0101_0000 | channel,
                        1 << 16,
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
                    self.perform_quietly(crate::swi::X | SOUND_CONTROL_PACKED, &mut stop)?;
                }
            }
            self.set_byte_var(var::PAGE_LINES, 0);
            let exec = self.byte_var(var::EXEC_HANDLE);
            if exec != 0 {
                self.set_byte_var(var::EXEC_HANDLE, 0);
                let mut close = Regs::new([0, u32::from(exec), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.perform(crate::swi::X | crate::swi::os::FIND, &mut close)?;
            }
            for buffer in 0..=MOUSE {
                self.flush(buffer)?;
            }
        }
        self.set_escape(false)?;
        regs.r[1] = if pending { 0xFF } else { 0 };
        Ok(())
    }

    /// `OS_Byte` 128: a buffer's count or space for R1 &80 upward, the mouse
    /// position for 7 and 8; any other channel is an ADC the emulator lacks.
    fn adval(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let channel = regs.byte(1);
        if channel >= 0x80 {
            let buffer = u32::from(channel ^ 0xFF);
            let spaces = !matches!(buffer, KEYBOARD | 1 | MOUSE);
            let count = self.count(buffer, spaces)?;
            regs.r[1] = count & 0xFF;
            regs.r[2] = (count >> 8) & 0xFF;
            return Ok(());
        }
        if channel == 7 || channel == 8 {
            let mut mouse = Regs::default();
            self.perform(crate::swi::X | crate::swi::os::MOUSE, &mut mouse)?;
            let value = if channel == 7 { mouse.r[0] } else { mouse.r[1] };
            regs.r[1] = value & 0xFF;
            regs.r[2] = (value >> 8) & 0xFF;
            return Ok(());
        }
        self.unknown_byte(regs)
    }

    /// `OS_Byte` 129: a timed read, a key's test, a scan, or the OS's
    /// identifier.
    fn inkey(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (r1, r2) = (regs.r[1] & 0xFF, regs.r[2] & 0xFF);
        if r2 & 0x80 == 0 {
            return self.timed_read(regs, r1 + (r2 << 8));
        }
        match r1 {
            0 => {
                regs.r[1] = OS_IDENTIFIER;
                regs.r[2] = 0;
            }
            0x80..=0xFF => {
                let key = r1 ^ 0xFF;
                let index = (regs.r[2] ^ 0x7F) & 0x7F;
                let down = self.internal_key_down(key, index)?;
                regs.r[1] = if down { 0xFF } else { 0 };
                regs.r[2] = regs.r[1];
                regs.set_flag(psr::C, down);
            }
            _ => {
                regs.r[1] = r1 ^ 0x7F;
                regs.r[2] ^= 0x7F;
                self.key_scan(regs)?;
            }
        }
        Ok(())
    }

    /// INKEY with a limit of `limit` centiseconds: the next character, or
    /// escape, or the limit's end. A wait keeps its deadline, so performing
    /// the `SWI` again after waiting does not start the limit afresh.
    fn timed_read(&mut self, regs: &mut Regs, limit: u32) -> Result<(), Fail> {
        let deadline = match self.world.continuation.take() {
            Some(Continuation::Inkey { deadline }) => deadline,
            _ => self.now() + u64::from(limit),
        };
        loop {
            if let Some(c) = self.next_char()? {
                regs.r[1] = u32::from(c);
                regs.r[2] = 0;
                regs.set_flag(psr::C, false);
                return Ok(());
            }
            if self.escape_set() || self.now() >= deadline {
                let value = if self.escape_set() { 0x1B } else { 0xFF };
                regs.r[1] = value;
                regs.r[2] = value;
                regs.set_flag(psr::C, true);
                return Ok(());
            }
            self.wait_for_input(Some(deadline), Some(Continuation::Inkey { deadline }))?;
        }
    }

    /// Writes `text` into the kernel's scratch buffer, for a SWI the kernel
    /// calls that takes a string's address.
    pub(crate) fn scratch_text(&mut self, text: &[u8]) -> Result<u32, Fail> {
        let scratch = self.world.scratch;
        if text.len() > crate::os::SCRATCH {
            return Err(self.buffer_overflow());
        }
        self.write_bytes(scratch, text)?;
        Ok(scratch)
    }
}

/// The alphabet and keyboard selected, and the keyboard's alphabet: RISC
/// OS starts with Latin1 and the UK's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Country {
    pub(crate) alphabet: u32,
    pub(crate) keyboard: u32,
    pub(crate) key_alphabet: u32,
}

impl Country {
    pub(crate) const fn new() -> Self {
        Self {
            alphabet: crate::international::LATIN1,
            keyboard: crate::territory::UK,
            key_alphabet: crate::international::LATIN1,
        }
    }
}

/// `Sound_QInit`.
const SOUND_QINIT: u32 = 0x4_01C0;
/// `Sound_ControlPacked`.
pub(crate) const SOUND_CONTROL_PACKED: u32 = 0x4_0186;

/// The `OS_Byte` variables' place in zero page, for `OS_ReadSysInfo` 6.
pub(crate) const VARIABLES: u32 = TABLE;

#[cfg(test)]
mod tests;

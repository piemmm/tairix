//! Character input (`plans/RISCOS-EMULATOR.md` §4.5): the kernel's own
//! buffers behind `InsV`, `RemV` and `CnpV` — the keyboard's, which the
//! session puts keys into, and the four-byte sound and speech buffers; the
//! buffer manager holds the rest — the escape condition, and `OS_ReadC`
//! through `RdchV`, which reads redirection, then the `*Exec` file, then the
//! selected buffer, with function keys expanded as `OS_Byte` sets them.
//!
//! A read that finds nothing waits: from a program's own `SWI` the context
//! sleeps until a key comes, and the `SWI` is performed again; from deeper
//! in the kernel the host is asked to wait.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;

use crate::byte::var;
use crate::events::number::{CHAR_INPUT, ESCAPE as ESCAPE_EVENT, INPUT_FULL, OUTPUT_EMPTY};
use crate::memory::layout::HIGH_ZERO_PAGE;
use crate::mouse::Packet;
use crate::os::{len32, Ending, Entry, Fail, Os, Regs, Unwind, Wait};
use crate::vectors::{self, Carry};
use crate::zeropage;

/// The keyboard buffer.
pub(crate) const KEYBOARD: u32 = 0;
/// The serial input buffer.
pub(crate) const SERIAL_IN: u32 = 1;
/// The serial output buffer.
pub(crate) const SERIAL_OUT: u32 = 2;
/// The printer buffer.
pub(crate) const PRINTER: u32 = 3;
/// The sound and speech buffers, which nothing uses.
const SOUND_AND_SPEECH: core::ops::RangeInclusive<u32> = 4..=8;
/// The mouse buffer.
pub(crate) const MOUSE: u32 = 9;
/// The sizes of the kernel's own buffers, the keyboard's and each sound and
/// speech buffer's; each holds a byte fewer.
const KEYBOARD_SIZE: usize = 0x100;
const SOUND_SIZE: usize = 4;
/// The escape character's effect: the escape condition, in `ESC_Status`'s
/// bit 6.
const ESCAPE_BIT: u8 = 1 << 6;
/// The escape character, as `OS_ReadC` returns it.
const ESCAPE: u8 = 0x1B;
/// The codes a key handler gives Copy and the cursor keys: Copy, left,
/// right, down, up.
const CURSOR_KEYS: core::ops::RangeInclusive<u8> = 0x8B..=0x8F;
/// `OS_Byte` 126: an escape acknowledged.
pub const ACKNOWLEDGE_ESCAPE: u32 = 126;
/// "Escape".
const ESCAPE_ERROR: crate::error::Error = crate::error::Error {
    number: 0x11,
    token: "Escape",
};

/// One buffer: a ring of bytes.
struct Ring {
    data: Vec<u8>,
    head: usize,
    len: usize,
}

impl Ring {
    fn capacity(&self) -> usize {
        self.data.len().saturating_sub(1)
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.len >= self.capacity() {
            return false;
        }
        let at = (self.head + self.len) % self.data.len();
        self.data[at] = byte;
        self.len += 1;
        true
    }

    /// The byte `index` places from the next to be removed.
    fn peek_at(&self, index: usize) -> Option<u8> {
        (index < self.len).then(|| self.data[(self.head + index) % self.data.len()])
    }

    fn pop(&mut self) -> Option<u8> {
        let byte = self.peek_at(0)?;
        self.head = (self.head + 1) % self.data.len();
        self.len -= 1;
        Some(byte)
    }
}

/// `InsV`'s and `RemV`'s R1 bit for a block of bytes rather than one.
pub(crate) const BLOCK: u32 = 1 << 31;

/// The kernel's own buffers and what reading them holds over between calls.
pub(crate) struct Input {
    /// The keyboard's buffer, then the sound and speech buffers.
    rings: Vec<Ring>,
    /// What is left of a function key's expansion.
    soft: VecDeque<u8>,
    /// A key code owed after the NUL that announced it.
    owed: Option<u8>,
    /// Where the mouse is and which buttons are held.
    pub(crate) mouse: crate::mouse::Mouse,
}

/// An input event the host delivers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum InputEvent {
    /// A character the keyboard gives, inserted as the keyboard driver
    /// inserts one.
    Key(u8),
    /// A key going down or up, by low-level key number, as `KeyV` takes it.
    Transition {
        /// The key.
        key: u32,
        /// Down rather than up.
        down: bool,
    },
    /// The mouse at a place on the screen, in OS units from its bottom left,
    /// with these buttons held.
    Pointer {
        /// Across.
        x: i32,
        /// Up.
        y: i32,
        /// The buttons: Adjust, Menu, Select from bit 0.
        buttons: u32,
    },
    /// The mouse moved by so much, in OS units before the multipliers
    /// `OS_Word` 21,2 sets, with these buttons held: for a host that reports
    /// movement rather than a place.
    Motion {
        /// Across.
        dx: i32,
        /// Up.
        dy: i32,
        /// The buttons: Adjust, Menu, Select from bit 0.
        buttons: u32,
    },
}

impl Input {
    pub(crate) fn new() -> Result<Self, crate::os::BootError> {
        let sizes = core::iter::once(KEYBOARD_SIZE).chain(SOUND_AND_SPEECH.map(|_| SOUND_SIZE));
        let mut rings = Vec::new();
        rings
            .try_reserve_exact(1 + SOUND_AND_SPEECH.count())
            .map_err(|_| crate::os::BootError::NoMemory)?;
        for size in sizes {
            let data = tairix_util::fallible::zeroed(size).ok_or(crate::os::BootError::NoMemory)?;
            rings.push(Ring {
                data,
                head: 0,
                len: 0,
            });
        }
        Ok(Self {
            rings,
            soft: VecDeque::new(),
            owed: None,
            mouse: crate::mouse::Mouse::new(),
        })
    }

    /// Buffer `buffer`, where it is one of the kernel's own.
    fn ring(&mut self, buffer: u32) -> Option<&mut Ring> {
        let index = if buffer == KEYBOARD {
            0
        } else if SOUND_AND_SPEECH.contains(&buffer) {
            (buffer - SOUND_AND_SPEECH.start()) as usize + 1
        } else {
            return None;
        };
        self.rings.get_mut(index)
    }

    /// Ends a function key's expansion part way.
    pub(crate) fn end_soft_key(&mut self) {
        self.soft.clear();
    }
}

/// What a SWI waiting for input has done when its wait unwinds, so that
/// performed again it carries on rather than starting over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Continuation {
    /// `OS_Byte` 129's timed read: when it times out.
    Inkey { deadline: u64 },
    /// A wait for the clock alone: when it ends.
    Until { deadline: u64 },
    /// `OS_ReadLine`: its buffer and the characters stored in it.
    Line { buffer: u32, length: u32 },
    /// `OS_Confirm`.
    Confirm(crate::text::Confirm),
}

impl Os<'_> {
    /// `InsV`'s default: R0's byte into the kernel's buffer R1, or with R1's
    /// bit 31 set, R3 bytes from R2 as far as they fit, R2 and R3 left at
    /// the rest; C set if not everything went in, as for a buffer that is
    /// not the kernel's.
    pub(crate) fn default_insert(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let buffer = regs.r[1] & !BLOCK;
        let block = regs.r[1] & BLOCK != 0;
        regs.set_flag(psr::V, false);
        if self.world.input.ring(buffer).is_none() {
            regs.set_flag(psr::C, true);
            return Ok(());
        }
        let (mut at, mut left) = if block {
            (regs.r[2], regs.r[3])
        } else {
            (0, 1)
        };
        let mut refused = None;
        while left > 0 {
            let byte = if block {
                self.read_byte(at)?
            } else {
                regs.byte(0)
            };
            let Some(ring) = self.world.input.ring(buffer) else {
                break;
            };
            if !ring.push(byte) {
                refused = Some(byte);
                break;
            }
            at = at.wrapping_add(1);
            left -= 1;
        }
        if block {
            regs.r[2] = at;
            regs.r[3] = left;
        }
        regs.set_flag(psr::C, left > 0);
        if let Some(byte) = refused.filter(|_| buffer == KEYBOARD) {
            let mut event = Regs::new([
                INPUT_FULL,
                buffer,
                u32::from(byte),
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
            self.raise_event(&mut event)?;
        }
        Ok(())
    }

    /// `RemV`'s default: a byte from the kernel's buffer R1 into R0 and R2,
    /// or with R1's bit 31 set, up to R3 bytes into the block at R2, R2 and
    /// R3 left past what came; removed unless V asks only to look. C set if
    /// it was empty. A buffer that is not the kernel's is left with C clear,
    /// as RISC OS 5 leaves it.
    pub(crate) fn default_remove(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let buffer = regs.r[1] & !BLOCK;
        let examine = regs.flag(psr::V);
        regs.set_flag(psr::V, false);
        let Some(empty) = self.world.input.ring(buffer).map(|ring| ring.len == 0) else {
            regs.set_flag(psr::C, false);
            return Ok(());
        };
        regs.set_flag(psr::C, empty);
        if regs.r[1] & BLOCK != 0 {
            self.remove_block(buffer, examine, regs)?;
        } else if let Some(ring) = self.world.input.ring(buffer) {
            let byte = if examine { ring.peek_at(0) } else { ring.pop() };
            if let Some(byte) = byte {
                regs.r[0] = u32::from(byte);
                regs.r[2] = u32::from(byte);
            }
        }
        let emptied = !examine
            && !empty
            && self
                .world
                .input
                .ring(buffer)
                .is_some_and(|ring| ring.len == 0);
        if emptied && SOUND_AND_SPEECH.contains(&buffer) {
            let mut event = Regs::new([OUTPUT_EMPTY, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.raise_event(&mut event)?;
        }
        Ok(())
    }

    /// `RemV`'s block form on `buffer`: each byte written to the block
    /// before it leaves the buffer, so a block that faults loses nothing.
    fn remove_block(&mut self, buffer: u32, examine: bool, regs: &mut Regs) -> Result<(), Fail> {
        let (mut at, mut left) = (regs.r[2], regs.r[3]);
        let mut index = 0;
        while left > 0 {
            let Some(byte) = self
                .world
                .input
                .ring(buffer)
                .and_then(|ring| ring.peek_at(index))
            else {
                break;
            };
            self.write_byte(at, byte)?;
            if examine {
                index += 1;
            } else if let Some(ring) = self.world.input.ring(buffer) {
                ring.pop();
            }
            at = at.wrapping_add(1);
            left -= 1;
        }
        regs.r[2] = at;
        regs.r[3] = left;
        Ok(())
    }

    /// `CnpV`'s default: the kernel's buffer R1 purged if V is set, else its
    /// bytes (C clear) or its free space (C set) counted into R1's low byte
    /// and R2; a buffer that is not the kernel's is left as it is.
    #[expect(clippy::unnecessary_wraps, reason = "a vector default's signature")]
    pub(crate) fn default_count_purge(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let buffer = regs.r[1] & !BLOCK;
        let purge = regs.flag(psr::V);
        let spaces = regs.flag(psr::C);
        regs.set_flag(psr::V, false);
        let Some(ring) = self.world.input.ring(buffer) else {
            return Ok(());
        };
        if purge {
            ring.len = 0;
            ring.head = 0;
            return Ok(());
        }
        let count = len32(if spaces {
            ring.capacity() - ring.len
        } else {
            ring.len
        });
        regs.r[1] = count & 0xFF;
        regs.r[2] = count >> 8;
        Ok(())
    }

    /// Inserts `byte` into buffer `buffer` through `InsV`; whether it fitted.
    pub(crate) fn insert(&mut self, buffer: u32, byte: u8) -> Result<bool, Unwind> {
        let mut regs = Regs::new([u32::from(byte), buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.call_vector(vectors::INS, &mut regs, Entry::SupervisorNoIrq)?;
        Ok(!regs.flag(psr::C))
    }

    /// Removes a byte from `buffer` through `RemV`, or looks at it.
    pub(crate) fn remove(&mut self, buffer: u32, examine: bool) -> Result<Option<u8>, Unwind> {
        let mut regs = Regs::new([0, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        regs.set_flag(psr::V, examine);
        self.call_vector(vectors::REM, &mut regs, Entry::SupervisorNoIrq)?;
        Ok((!regs.flag(psr::C)).then_some(regs.byte(0)))
    }

    /// Purges `buffer` through `CnpV`.
    pub(crate) fn purge(&mut self, buffer: u32) -> Result<(), Unwind> {
        let mut regs = Regs::new([0, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        regs.set_flag(psr::V, true);
        self.call_vector(vectors::CNP, &mut regs, Entry::SupervisorNoIrq)
    }

    /// Counts `buffer`'s bytes, or its free space, through `CnpV`.
    pub(crate) fn count(&mut self, buffer: u32, spaces: bool) -> Result<u32, Unwind> {
        let mut regs = Regs::new([0, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        regs.set_flag(psr::C, spaces);
        self.call_vector(vectors::CNP, &mut regs, Entry::SupervisorNoIrq)?;
        Ok((regs.r[1] & 0xFF) | (regs.r[2] << 8))
    }

    /// Buffers `packet` in the mouse buffer through `InsV` where the whole
    /// of it fits, as the kernel inserts its nine bytes together.
    pub(crate) fn buffer_mouse_packet(&mut self, packet: [u8; Packet::BYTES]) -> Result<(), Fail> {
        let bytes = len32(Packet::BYTES);
        if self.count(MOUSE, true)? < bytes {
            return Ok(());
        }
        self.below_supervisor_stack(bytes, |os, at| {
            os.write_bytes(at, &packet)?;
            let mut regs = Regs::new([0, MOUSE | BLOCK, at, bytes, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            os.call_vector(vectors::INS, &mut regs, Entry::SupervisorNoIrq)?;
            Ok(())
        })
    }

    /// The next packet the mouse buffer holds, taken through `RemV`.
    pub(crate) fn take_mouse_packet(&mut self) -> Result<Option<Packet>, Fail> {
        let bytes = len32(Packet::BYTES);
        if self.count(MOUSE, false)? < bytes {
            return Ok(None);
        }
        self.below_supervisor_stack(bytes, |os, at| {
            let mut regs = Regs::new([0, MOUSE | BLOCK, at, bytes, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            os.call_vector(vectors::REM, &mut regs, Entry::SupervisorNoIrq)?;
            if regs.r[3] != 0 {
                return Ok(None);
            }
            let mut packet = [0u8; Packet::BYTES];
            os.read_bytes(at, &mut packet)?;
            Ok(Some(Packet::from_bytes(packet)))
        })
    }

    /// A character for buffer `buffer` as the keyboard driver and `OS_Byte`
    /// 153 insert one: the escape character sets the escape condition (or
    /// raises its event) instead, unless `OS_Byte` 229 made it ordinary;
    /// anything else raises the character event and goes in.
    pub(crate) fn insert_checked(&mut self, buffer: u32, byte: u8) -> Result<bool, Unwind> {
        let raw = buffer == SERIAL_IN && self.byte_var(var::RS423_MODE) & 1 != 0;
        let checked = buffer == KEYBOARD || (buffer == SERIAL_IN && !raw);
        let escape_char = self.byte_var(var::ESCAPE_CHAR);
        if checked && byte == escape_char && self.byte_var(var::ESCAPE_ACTION) == 0 {
            if self.byte_var(var::ESCAPE_BREAK) & 1 != 0 {
                return Ok(true);
            }
            let mut event = Regs::new([
                ESCAPE_EVENT,
                buffer,
                u32::from(byte),
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
            if !self.raise_event(&mut event)? {
                self.set_escape(true)?;
            }
            return Ok(true);
        }
        if checked {
            let mut event = Regs::new([
                CHAR_INPUT,
                buffer,
                u32::from(byte),
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
            self.raise_event(&mut event)?;
        }
        self.insert(buffer, byte)
    }

    /// Delivers what the host has for the guest: each key into the keyboard
    /// buffer unless the keyboard is disabled.
    pub(crate) fn take_input(&mut self) -> Result<(), Unwind> {
        while let Some(event) = self.world.host.input() {
            match event {
                InputEvent::Key(byte) => self.keyboard_insert(byte)?,
                InputEvent::Transition { key, down } => {
                    let mut regs = Regs::new([
                        if down { 2 } else { 1 },
                        key,
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
                    self.call_vector(vectors::KEY, &mut regs, Entry::SupervisorNoIrq)?;
                }
                InputEvent::Pointer { x, y, buttons } => self.pointer_moved(x, y, buttons)?,
                InputEvent::Motion { dx, dy, buttons } => self.mouse_moved_by(dx, dy, buttons)?,
            }
        }
        Ok(())
    }

    /// Whether the escape condition is set.
    pub fn escape_set(&mut self) -> bool {
        self.peek_byte(HIGH_ZERO_PAGE + zeropage::ESC_STATUS)
            .is_some_and(|status| status & ESCAPE_BIT != 0)
    }

    /// A listing's answer to an escape: acknowledged with `OS_Byte` 126,
    /// then "Escape", or the error the acknowledgement gave.
    pub fn escape_acknowledged(&mut self) -> Fail {
        let mut regs = Regs::default();
        regs.r[0] = ACKNOWLEDGE_ESCAPE;
        match self.perform(crate::swi::os::BYTE, &mut regs) {
            Ok(()) => self.error(ESCAPE_ERROR),
            Err(fail) => fail,
        }
    }

    /// Sets or clears the escape condition, telling the escape handler.
    pub(crate) fn set_escape(&mut self, set: bool) -> Result<(), Unwind> {
        let value = if set { 0xFF } else { 0 };
        let _ = self.poke_byte(HIGH_ZERO_PAGE + zeropage::ESC_STATUS, value);
        self.tell_escape_handler(set)
    }

    /// `OS_ReadEscapeState`: C set while the escape condition is.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn read_escape_state(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let set = self.escape_set();
        regs.set_flag(psr::C, set);
        Ok(())
    }

    /// `OS_ReadC`, through `RdchV` with interrupts enabled.
    pub(crate) fn read_c(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::RDCH, regs, Entry::SupervisorIrq, Carry::Vector)
    }

    /// `RdchV`'s default: the next character, or 27 with C set on escape.
    pub(crate) fn default_rdch(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        loop {
            if let Some(c) = self.next_char()? {
                regs.r[0] = u32::from(c);
                regs.set_flag(psr::C, false);
                return Ok(());
            }
            if self.escape_set() {
                regs.r[0] = u32::from(ESCAPE);
                regs.set_flag(psr::C, true);
                return Ok(());
            }
            self.wait_for_input(None, None)?;
        }
    }

    /// The next character input has, without waiting: escape stops the read
    /// before any.
    pub(crate) fn next_char(&mut self) -> Result<Option<u8>, Fail> {
        loop {
            if self.escape_set() {
                return Ok(None);
            }
            if let Some(c) = self.world.input.owed.take() {
                return Ok(Some(c));
            }
            if let Some(c) = self.world.input.soft.pop_front() {
                let left = u8::try_from(self.world.input.soft.len()).unwrap_or(u8::MAX);
                self.set_byte_var(var::SOFT_KEY_LEN, left);
                return Ok(Some(c));
            }
            if let Some(c) = self.redirected_char()? {
                return Ok(Some(c));
            }
            if let Some(c) = self.exec_char()? {
                return Ok(Some(c));
            }
            self.take_input()?;
            let buffer = u32::from(self.byte_var(var::INPUT_STREAM) & 1);
            let Some(code) = self.remove(buffer, false)? else {
                return Ok(None);
            };
            if buffer == SERIAL_IN && self.byte_var(var::RS423_MODE) & 1 != 0 {
                return Ok(Some(code));
            }
            // A NUL keeps the byte after it as itself: a character past &7F,
            // not a key's code, or a second NUL, Ctrl-@, which reads as two
            // where a block of codes reads as NUL and code.
            if code == 0 {
                let literal = self.remove(buffer, false)?.unwrap_or(0);
                if literal == 0 && self.codes_read_as_nul_and_code() {
                    self.world.input.owed = Some(0);
                }
                return Ok(Some(literal));
            }
            // A code that reads as nothing — taken by the copy cursor, or a
            // function key's text begun — leaves the next to be read now.
            if let Some(c) = self.translate_code(code)? {
                return Ok(Some(c));
            }
        }
    }

    /// A character from the input redirection, if it is on: off again at its
    /// end.
    fn redirected_char(&mut self) -> Result<Option<u8>, Fail> {
        let handle = self
            .peek_byte(HIGH_ZERO_PAGE + zeropage::REDIRECT_IN)
            .unwrap_or(0);
        if handle == 0 {
            return Ok(None);
        }
        let mut get = Regs::new([0, u32::from(handle), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        match self.perform(crate::swi::X | crate::swi::os::BGET, &mut get) {
            Ok(()) if !get.flag(psr::C) => Ok(Some(get.byte(0))),
            Err(Fail::Unwind(unwind)) => Err(Fail::Unwind(unwind)),
            Ok(()) | Err(Fail::Error(_)) => {
                let _ = self.poke_byte(HIGH_ZERO_PAGE + zeropage::REDIRECT_IN, 0);
                Ok(None)
            }
        }
    }

    /// A character from the `*Exec` file, if one is open: closed at its end
    /// or on an error.
    fn exec_char(&mut self) -> Result<Option<u8>, Fail> {
        let handle = self.byte_var(var::EXEC_HANDLE);
        if handle == 0 {
            return Ok(None);
        }
        let mut get = Regs::new([0, u32::from(handle), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let got = self.perform(crate::swi::X | crate::swi::os::BGET, &mut get);
        match got {
            Ok(()) if !get.flag(psr::C) => return Ok(Some(get.byte(0))),
            Err(Fail::Unwind(unwind)) => return Err(Fail::Unwind(unwind)),
            Ok(()) | Err(Fail::Error(_)) => {}
        }
        self.set_byte_var(var::EXEC_HANDLE, 0);
        let mut close = Regs::new([0, u32::from(handle), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform_quietly(crate::swi::X | crate::swi::os::FIND, &mut close)?;
        got.map(|()| None)
    }

    /// Whether any of `OS_Byte` 221 to 228's blocks of codes reads as a NUL
    /// and the code.
    fn codes_read_as_nul_and_code(&mut self) -> bool {
        (var::BUFFER_CODE_BASE..var::BUFFER_CODE_BASE + 4)
            .chain(var::RED_KEY_BASE..var::RED_KEY_BASE + 4)
            .any(|block| self.byte_var(block) == 2)
    }

    /// What a code from the buffer reads as: below &80 itself; a function
    /// key's or special key's code as `OS_Byte` 221–228 and 4 set it.
    fn translate_code(&mut self, code: u8) -> Result<Option<u8>, Fail> {
        if code < 0x80 {
            return Ok(Some(code));
        }
        let nibble = code & 0xF;
        // Copy and the cursor keys are the copy cursor's while `OS_Byte` 4
        // is 0: the VDU drivers move it, and only Copy reads, the character
        // under it. At 1 they read 135 to 139, and at 2 as function keys 11
        // to 15.
        if CURSOR_KEYS.contains(&code) {
            match self.byte_var(var::CURSOR_EDIT) {
                0 => {
                    return match self.world.vdu {
                        Some(vdu) => (vdu.edit)(self, code - CURSOR_KEYS.start()),
                        None => Ok(None),
                    }
                }
                1 => return Ok(Some(code - CURSOR_KEYS.start() + 135)),
                _ => {}
            }
        }
        let action = match code {
            0x80..=0xBF => self.byte_var(var::RED_KEY_BASE + ((code - 0x80) >> 4)),
            _ => self.byte_var(var::BUFFER_CODE_BASE + ((code - 0xC0) >> 4)),
        };
        match action {
            0 => Ok(None),
            1 => {
                self.expand_soft_key(nibble)?;
                Ok(None)
            }
            2 => {
                self.world.input.owed = Some(code);
                Ok(Some(0))
            }
            base => Ok(Some(base.wrapping_add(nibble))),
        }
    }

    /// Starts a function key's expansion: `Key$n`'s value, delivered a
    /// character at a time.
    fn expand_soft_key(&mut self, key: u8) -> Result<(), Fail> {
        let mut name = *b"Key$00";
        let digits = crate::convert::Decimal::of(i32::from(key));
        let digits = digits.as_bytes();
        name[4..4 + digits.len()].copy_from_slice(digits);
        let value = self
            .variable_text(&name[..4 + digits.len()])?
            .unwrap_or_default();
        let left = u8::try_from(value.len()).unwrap_or(u8::MAX);
        self.world.input.soft = VecDeque::from(value);
        self.set_byte_var(var::SOFT_KEY_LEN, left);
        Ok(())
    }

    /// Nothing to read before `deadline`. Transient callbacks waiting run
    /// first, and the caller looks again. From a program's own `SWI` that
    /// has run no guest code and is not one the kernel performs for another,
    /// the context waits and the `SWI` is performed again when input comes,
    /// carrying on from `continuation`; otherwise the host waits here, the
    /// clock's interrupts taken as it ticks.
    pub(crate) fn wait_for_input(
        &mut self,
        deadline: Option<u64>,
        continuation: Option<Continuation>,
    ) -> Result<(), Fail> {
        if self.callbacks_while_idle()? {
            return Ok(());
        }
        if let Some(wait) = self.suspended(Wait::Input { until: deadline }, continuation) {
            return Err(wait);
        }
        self.host_wait(deadline)
    }

    /// The SWI being performed left to be performed again when `wait` is
    /// over, carrying on from `continuation`, where it is a program's own
    /// `SWI` that has run no guest code and is not one the kernel performs
    /// for another: the unwind that leaves it.
    pub(crate) fn suspended(
        &mut self,
        wait: Wait,
        continuation: Option<Continuation>,
    ) -> Option<Fail> {
        let resumable =
            self.world.depth == 0 && !self.world.guest_called && self.world.kernel_calls == 0;
        let number = self
            .world
            .frame
            .and_then(|frame| frame.swi)
            .filter(|_| resumable)?;
        let swi = self.world.swi_at;
        self.world.processor.cpu().set_reg(Reg::PC, swi);
        self.world.continuation = continuation;
        self.world.resume = Some((swi, number));
        Some(Fail::Unwind(Unwind::Wait(wait)))
    }

    /// Waits here for the host's next input or the next timed work, the
    /// interrupt the clock asserted meanwhile taken: for a SWI that must not
    /// be performed again, waiting on what the keys do.
    ///
    /// # Errors
    ///
    /// The unwind the host's stop or an interrupt's work starts.
    pub fn await_input(&mut self) -> Result<(), Fail> {
        self.host_wait(None)
    }

    /// As [`Self::await_input`], waking at `deadline` if nothing comes
    /// first: for a wait whose caller must look again by then.
    ///
    /// # Errors
    ///
    /// The unwind the host's stop or an interrupt's work starts.
    pub fn await_input_until(&mut self, deadline: u64) -> Result<(), Fail> {
        self.host_wait(Some(deadline))
    }

    /// The host waits for input, the next timed work or `deadline`, and the
    /// interrupt its waking is — the clock's or the keyboard's — is taken, so
    /// input that comes before the clock moves is delivered rather than
    /// waited on again.
    pub(crate) fn host_wait(&mut self, deadline: Option<u64>) -> Result<(), Fail> {
        let until = match (deadline, self.next_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let ending = if deadline.is_some() {
            Ending::Clock
        } else {
            Ending::Input
        };
        if self.world.host.wait(until, ending) == crate::os::Pulse::Stop {
            return Err(Fail::Unwind(Unwind::Stopped));
        }
        self.world.processor.set_irq(true);
        self.take_interrupt()?;
        Ok(())
    }
}

//! The keyboard (`plans/RISCOS-EMULATOR.md` §4.6): `KeyV`'s default, which
//! turns each low-level key going down or up into codes through the key
//! handler `OS_InstallKeyHandler` installs — two keys rolled over, the first
//! code at once, then repeats at `OS_Byte` 11 and 12's delay and rate — the
//! keyboard status and its LEDs, and the handler's tables through which
//! `INKEY` and the key scans read keys by internal key number.
//!
//! No key is debounced: the keyboard and mouse are the host's, whose
//! changes do not bounce, so every key, a mouse button among them, is taken
//! as it comes. Keys are ignored until `KeyV` 4 has enabled the drivers.
//!
//! A handler is guest memory, its tables read bounded at each use; a guest
//! handler's code runs on the processor, a built-in one's Rust directly.

use tairix_riscos_arm::psr;

use crate::byte::var;
use crate::memory::Bits;
use crate::os::{Entry, Fail, Os, Regs, Unwind};
use crate::vectors;

/// `KeyV`'s reasons.
const PRESENT: u32 = 0;
const UP: u32 = 1;
const DOWN: u32 = 2;
const LEDS: u32 = 3;
const ENABLE: u32 = 4;
/// The keyboard's identity before one has said: none.
pub(crate) const NO_KEYBOARD: u8 = 0xFF;
/// The keyboard every session has, a PC's, and the identity it says.
pub(crate) const PC: u32 = 2;
/// The keys a handler's map holds, and those it can scan.
const KEYS: usize = 0x300;
/// The keyboard event, and `Service_KeyHandler`.
const KEYBOARD_EVENT: u32 = 11;
pub(crate) const KEY_HANDLER: u32 = 0x44;
/// The status bits: the handler's own bit, a pending Alt; Scroll Lock on,
/// Num Lock off, Shift held, Caps Lock off, bit 5 normally set, Ctrl held,
/// Shift Enable.
pub(crate) mod status {
    pub(crate) const PENDING_ALT: u8 = 1;
    pub(crate) const SCROLL_LOCK: u8 = 1 << 1;
    pub(crate) const NO_NUM_LOCK: u8 = 1 << 2;
    pub(crate) const SHIFT: u8 = 1 << 3;
    pub(crate) const NO_CAPS_LOCK: u8 = 1 << 4;
    pub(crate) const NO_KANA_LOCK: u8 = 1 << 5;
    pub(crate) const CTRL: u8 = 1 << 6;
    pub(crate) const SHIFT_CAPS: u8 = 1 << 7;
}
/// The LEDs `KeyV` 3 lights.
const SCROLL_LED: u32 = 1;
const NUM_LED: u32 = 2;
const CAPS_LED: u32 = 4;
/// A special entry in a key's translation.
const NARROW_SPECIAL: u32 = 0xFF;
/// What a special key's code is asked: a shifting key up or down, a key's
/// first code, a repeat.
const SHIFT_UP: u32 = 0;
const SHIFT_DOWN: u32 = 1;
const FIRST: u32 = 2;
const REPEAT: u32 = 3;
/// The workspace a handler's code is given.
pub(crate) const HANDLER_WORKSPACE: u32 = 96;
/// The internal keys whose number reaches the extension ranges.
const EXTENSION_1: u32 = 13;
const EXTENSION_2: u32 = 15;
/// `OS_Byte` 247's field Break's modifiers choose, and its actions.
const BREAK_ESCAPES: u8 = 1;
const BREAK_RESETS: u8 = 0;
/// What a scan that finds no key leaves in R2, as RISC OS 5's does.
const SCAN_MISSED: u32 = 0x2000_0000;

/// A key handler's header, read from where it was installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Handler {
    base: u32,
    translation: u32,
    keys: u32,
    inkey: u32,
    shifting: u32,
    special: u32,
    codes: u32,
    init: u32,
    pending: u32,
    wide: bool,
}

/// The keyboard's state.
pub(crate) struct Keyboard {
    handler: Option<(u32, Bits)>,
    /// The identity the keyboard said.
    pub(crate) id: Option<u8>,
    /// Whether `KeyV` 4 has enabled the drivers since they last were reset.
    enabled: bool,
    /// Which low-level keys are down.
    down: [u64; KEYS / 64],
    current: Option<u32>,
    old: Option<u32>,
    /// When the current key's next code falls due.
    due: Option<u64>,
    /// The Shift and Ctrl bits of the status as the handler last left them
    /// for one of their keys.
    shifts: u8,
    leds: Option<u32>,
    /// What the handler keeps of a pending Alt.
    pending: u32,
    /// The handler's workspace, an empty list and the exits, in guest memory.
    pub(crate) workspace: u32,
    pub(crate) empty: u32,
    pub(crate) exits: u32,
}

impl Keyboard {
    pub(crate) const fn new() -> Self {
        Self {
            handler: None,
            id: None,
            enabled: false,
            down: [0; KEYS / 64],
            current: None,
            old: None,
            due: None,
            shifts: 0,
            leds: None,
            pending: 0,
            workspace: 0,
            empty: 0,
            exits: 0,
        }
    }

    fn is_down(&self, key: u32) -> bool {
        self.down
            .get(key as usize / 64)
            .is_some_and(|word| word >> (key % 64) & 1 != 0)
    }

    /// Notes `key` down or up: whether it changed.
    fn note(&mut self, key: u32, down: bool) -> bool {
        let Some(word) = self.down.get_mut(key as usize / 64) else {
            return true;
        };
        let bit = 1 << (key % 64);
        let was = *word & bit != 0;
        if down {
            *word |= bit;
        } else {
            *word &= !bit;
        }
        was != down
    }

    /// The next code's deadline, for the clock.
    pub(crate) const fn due(&self) -> Option<u64> {
        self.due
    }

    /// The highest key down but `ignored`, the shifting keys passed over by
    /// `shifting`; only the low and the extension ranges, where keys are,
    /// are scanned.
    fn rescan(&self, ignored: Option<u32>, shifting: impl Fn(u32) -> bool) -> Option<u32> {
        (0x220..0x300)
            .rev()
            .chain((0..0xA0).rev())
            .find(|&key| Some(key) != ignored && self.is_down(key) && !shifting(key))
    }
}

impl Os<'_> {
    /// The installed handler's header.
    fn handler(&mut self) -> Result<Option<Handler>, Fail> {
        let Some((base, _)) = self.world.keyboard.handler else {
            return Ok(None);
        };
        let mut words = [0; 9];
        for (i, word) in (0..).zip(words.iter_mut().take(8)) {
            *word = self.read_word(base.wrapping_add(4 * i))?;
        }
        let flagged = words[1] & (1 << 31) != 0;
        let wide = flagged && self.read_word(base.wrapping_add(32))? & 1 != 0;
        let at = |offset: u32| base.wrapping_add(offset);
        Ok(Some(Handler {
            base,
            translation: at(words[0]),
            keys: words[1] & !(1 << 31),
            inkey: at(words[2]),
            shifting: at(words[3]),
            special: at(words[4]),
            codes: at(words[5]),
            init: at(words[6]),
            pending: at(words[7]),
            wide,
        }))
    }

    /// An entry of a handler's table of bytes or halfwords.
    fn handler_entry(&mut self, handler: &Handler, at: u32, index: u32) -> Result<u32, Fail> {
        if handler.wide {
            let lo = self.read_byte(at.wrapping_add(2 * index))?;
            let hi = self.read_byte(at.wrapping_add(2 * index + 1))?;
            Ok(u32::from(u16::from_le_bytes([lo, hi])))
        } else {
            Ok(u32::from(self.read_byte(at.wrapping_add(index))?))
        }
    }

    /// Where `key` is in the list at `list`, searched from its end: its place
    /// from one.
    fn listed(&mut self, handler: &Handler, list: u32, key: u32) -> Result<Option<u32>, Fail> {
        let count = self.handler_entry(handler, list, 0)?;
        for place in (1..=count).rev() {
            if self.handler_entry(handler, list, place)? == key {
                return Ok(Some(place));
            }
            self.yield_point()?;
        }
        Ok(None)
    }

    /// `key`'s translation in `column`: plain, Shift, Ctrl, Ctrl and Shift.
    fn translation(
        &mut self,
        handler: &Handler,
        key: u32,
        column: u32,
    ) -> Result<Option<u32>, Fail> {
        if key >= handler.keys {
            return Ok(None);
        }
        let code = self.handler_entry(handler, handler.translation, 4 * key + column)?;
        Ok((code != NARROW_SPECIAL).then_some(code))
    }

    /// Whether `key` is a shifting key: special in every column, and listed
    /// as one.
    fn shifting(&mut self, handler: &Handler, key: u32) -> Result<bool, Fail> {
        for column in 0..4 {
            if self.translation(handler, key, column)?.is_some() {
                return Ok(false);
            }
        }
        Ok(self.listed(handler, handler.shifting, key)?.is_some())
    }

    /// The keyboard status, `OS_Byte` 202's variable.
    fn keyboard_status(&mut self) -> u8 {
        self.byte_var(var::KEYBOARD_STATUS)
    }

    /// The LEDs the status lights, sent to the keyboard if they changed.
    pub(crate) fn refresh_leds(&mut self, always: bool) -> Result<(), Unwind> {
        if self.world.keyboard.id.is_none() {
            return Ok(());
        }
        let state = self.keyboard_status();
        let mut leds = 0;
        if state & status::NO_CAPS_LOCK == 0 {
            leds |= CAPS_LED;
        }
        if state & status::NO_NUM_LOCK == 0 {
            leds |= NUM_LED;
        }
        if state & status::SCROLL_LOCK != 0 {
            leds |= SCROLL_LED;
        }
        if !always && self.world.keyboard.leds == Some(leds) {
            return Ok(());
        }
        self.world.keyboard.leds = Some(leds);
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[LEDS, leds]);
        self.call_vector(vectors::KEY, &mut regs, Entry::SupervisorNoIrq)
    }

    /// Calls the handler's code at `address` with `regs` — R0 the handler,
    /// R5 the status, R7 what it keeps, R12 its workspace — then takes its
    /// status and what it keeps, and inserts the list R6 points at.
    fn call_handler(
        &mut self,
        handler: &Handler,
        address: u32,
        regs: &mut Regs,
    ) -> Result<(), Fail> {
        regs.r[0] = handler.base;
        regs.r[5] = u32::from(self.keyboard_status());
        regs.r[7] = self.world.keyboard.pending;
        regs.r[12] = self.world.keyboard.workspace;
        let bits = self
            .world
            .keyboard
            .handler
            .map_or(Bits::Bit32, |(_, bits)| bits);
        let result = self.call_routine(address, bits, Entry::SupervisorNoIrq, regs);
        self.world.keyboard.pending = regs.r[7];
        self.set_byte_var(var::KEYBOARD_STATUS, regs.byte(5));
        self.refresh_leds(false)?;
        result?;
        self.insert_list(regs.r[6])
    }

    /// The handler's list at `list` inserted: a length, then the bytes, all
    /// dropped where more than one would not fit.
    fn insert_list(&mut self, list: u32) -> Result<(), Fail> {
        if list == 0 {
            return Ok(());
        }
        let length = self.read_byte(list)?;
        if length >= 2 {
            let free = self.count(crate::input::KEYBOARD, true)?;
            if free < u32::from(length) {
                return Ok(());
            }
        }
        for i in 1..=u32::from(length) {
            let byte = self.read_byte(list.wrapping_add(i))?;
            self.keyboard_insert(byte)?;
        }
        Ok(())
    }

    /// A byte the keyboard gives, inserted: none while `OS_Byte` 201
    /// disables it; the escape character also stopping the repeat.
    pub(crate) fn keyboard_insert(&mut self, byte: u8) -> Result<(), Unwind> {
        if self.byte_var(var::KEYBOARD_DISABLE) != 0 {
            return Ok(());
        }
        if byte == self.byte_var(var::ESCAPE_CHAR) && self.byte_var(var::ESCAPE_ACTION) == 0 {
            self.world.keyboard.due = None;
        }
        self.insert_checked(crate::input::KEYBOARD, byte)?;
        Ok(())
    }

    /// Key `key`'s code made, `action` first or repeat: its translation, or
    /// its special code.
    fn generate(&mut self, key: u32, action: u32) -> Result<(), Fail> {
        let Some(handler) = self.handler()? else {
            return Ok(());
        };
        let state = self.keyboard_status();
        let column =
            u32::from(state & status::SHIFT != 0) + 2 * u32::from(state & status::CTRL != 0);
        let Some(mut code) = self.translation(&handler, key, column)? else {
            return self.special(&handler, key, action);
        };
        let caps_lock = state & status::NO_CAPS_LOCK == 0;
        if caps_lock && u8::try_from(code).is_ok_and(|byte| (byte & 0xDF).is_ascii_uppercase()) {
            // Shift Enable makes Shift give lower case under Caps Lock.
            if state & status::SHIFT_CAPS != 0 {
                code ^= 0x20;
            } else {
                code &= 0xDF;
            }
        }
        let escape = u32::from(self.byte_var(var::ESCAPE_CHAR));
        let pending = state & status::PENDING_ALT != 0;
        if code == escape
            && self.byte_var(var::ESCAPE_ACTION) == 0
            && self.byte_var(var::ESCAPE_BREAK) & 1 == 0
            && pending
        {
            self.set_byte_var(var::KEYBOARD_STATUS, state & !status::PENDING_ALT);
        }
        if self.keyboard_status() & status::PENDING_ALT != 0 {
            let mut regs = Regs::default();
            regs.r[..4].copy_from_slice(&[0, action, key, code]);
            regs.r[6] = self.world.keyboard.empty;
            return self.call_handler(&handler, handler.pending, &mut regs);
        }
        let code = u8::try_from(code).unwrap_or(0);
        if code == 0 {
            self.keyboard_insert(0)?;
        }
        self.keyboard_insert(code)?;
        Ok(())
    }

    /// A special key's code called, if the special list has it.
    fn special(&mut self, handler: &Handler, key: u32, action: u32) -> Result<(), Fail> {
        let Some(number) = self.listed(handler, handler.special, key)? else {
            return Ok(());
        };
        let offset = self.read_word(handler.codes.wrapping_add(4 * (number - 1)))?;
        let code = handler.codes.wrapping_add(offset);
        let mut regs = Regs::default();
        regs.r[1..5].copy_from_slice(&[action, key, self.world.keyboard.exits, number]);
        regs.r[6] = self.world.keyboard.empty;
        self.call_handler(handler, code, &mut regs)
    }

    /// The handler started afresh: every key up, its initialisation called,
    /// then the keyboard's drivers asked to go on.
    fn enable_keyboard(&mut self) -> Result<(), Fail> {
        let keyboard = &mut self.world.keyboard;
        keyboard.current = None;
        keyboard.old = None;
        keyboard.due = None;
        keyboard.leds = None;
        keyboard.down = [0; KEYS / 64];
        keyboard.enabled = false;
        let Some(id) = keyboard.id else {
            return Ok(());
        };
        let Some(handler) = self.handler()? else {
            return Ok(());
        };
        let mut regs = Regs::default();
        regs.r[1] = u32::from(id);
        regs.r[6] = self.world.keyboard.empty;
        self.call_handler(&handler, handler.init, &mut regs)?;
        self.world.keyboard.shifts = self.keyboard_status() & (status::SHIFT | status::CTRL);
        let mut enable = Regs::default();
        enable.r[0] = ENABLE;
        self.call_vector(vectors::KEY, &mut enable, Entry::SupervisorNoIrq)?;
        self.world.keyboard.enabled = true;
        Ok(())
    }

    /// `OS_InstallKeyHandler`: R0 nought reads the handler, one the
    /// keyboard's identity; another installs it, -1 removing it, the old one
    /// returned, and `Service_KeyHandler` follows.
    pub(crate) fn install_key_handler(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let installed = self
            .world
            .keyboard
            .handler
            .map_or(u32::MAX, |(base, _)| base);
        match regs.r[0] {
            0 => regs.r[0] = installed,
            1 => regs.r[0] = u32::from(self.world.keyboard.id.unwrap_or(NO_KEYBOARD)),
            handler => {
                let bits = self.caller_bits();
                self.world.keyboard.handler = (handler != u32::MAX).then_some((handler, bits));
                regs.r[0] = installed;
                self.enable_keyboard()?;
                self.announce_key_handler()?;
            }
        }
        Ok(())
    }

    /// `KeyV`'s default: a keyboard's arrival, a key going down or up.
    pub(crate) fn default_key(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            PRESENT => {
                self.world.keyboard.id = Some(u8::try_from(regs.r[1]).unwrap_or(NO_KEYBOARD));
                let handler = self.world.keyboard.handler;
                self.announce_key_handler()?;
                if self.world.keyboard.handler == handler {
                    self.enable_keyboard()?;
                }
                Ok(())
            }
            UP | DOWN if self.world.keyboard.enabled => {
                self.key_transition(regs.r[1], regs.r[0] == DOWN)
            }
            _ => Ok(()),
        }
    }

    /// Key `key` down or up.
    fn key_transition(&mut self, key: u32, down: bool) -> Result<(), Fail> {
        if (key as usize) < KEYS && !self.world.keyboard.note(key, down) {
            return Ok(());
        }
        let id = u32::from(self.world.keyboard.id.unwrap_or(NO_KEYBOARD));
        let mut event = Regs::default();
        event.r[..4].copy_from_slice(&[KEYBOARD_EVENT, u32::from(down), key, id]);
        self.raise_event(&mut event)?;
        let Some(handler) = self.handler()? else {
            return Ok(());
        };
        // Shift and Ctrl as their keys last left them, whatever OS_Byte 202
        // wrote since.
        let held = status::SHIFT | status::CTRL;
        let state = self.keyboard_status();
        self.set_byte_var(
            var::KEYBOARD_STATUS,
            (state & !held) | self.world.keyboard.shifts,
        );
        // Every key event shows the locks as they stand, OS_Byte 202's writes
        // included.
        self.refresh_leds(false)?;
        if self.shifting(&handler, key)? {
            let action = if down { SHIFT_DOWN } else { SHIFT_UP };
            self.special(&handler, key, action)?;
            self.world.keyboard.shifts = self.keyboard_status() & held;
            return Ok(());
        }
        if down {
            let keyboard = &mut self.world.keyboard;
            if keyboard.current.is_some() && keyboard.old.is_some() {
                return Ok(());
            }
            keyboard.old = keyboard.current;
            return self.start_key(key);
        }
        if self.world.keyboard.old == Some(key) {
            let current = self.world.keyboard.current;
            match self.rescan_with(&handler, current)? {
                Some(next) => {
                    self.world.keyboard.old = current;
                    self.start_key(next)?;
                }
                None => self.world.keyboard.old = None,
            }
        } else if self.world.keyboard.current == Some(key) {
            let old = self.world.keyboard.old;
            if let Some(next) = self.rescan_with(&handler, old)? {
                self.start_key(next)?;
            } else {
                self.world.keyboard.current = None;
                self.world.keyboard.due = None;
            }
        }
        Ok(())
    }

    /// The highest key down but `ignored` that is not a shifting key.
    fn rescan_with(
        &mut self,
        handler: &Handler,
        ignored: Option<u32>,
    ) -> Result<Option<u32>, Fail> {
        let mut shifting = [0u64; KEYS / 64];
        for key in (0..0xA0).chain(0x220..0x300) {
            if self.world.keyboard.is_down(key) && self.shifting(handler, key)? {
                shifting[key as usize / 64] |= 1 << (key % 64);
            }
        }
        Ok(self.world.keyboard.rescan(ignored, |key| {
            shifting[key as usize / 64] >> (key % 64) & 1 != 0
        }))
    }

    /// `key` the current key: its first code at once, its repeats after
    /// the delay.
    fn start_key(&mut self, key: u32) -> Result<(), Fail> {
        let now = self.now();
        self.world.keyboard.current = Some(key);
        self.world.keyboard.due = self.repeat_after(now, var::REPEAT_DELAY);
        self.generate(key, FIRST)
    }

    /// `Service_KeyHandler`, for the handlers that keep tables by the
    /// keyboard's identity.
    fn announce_key_handler(&mut self) -> Result<(), Fail> {
        let id = self.world.keyboard.id.unwrap_or(NO_KEYBOARD);
        let mut service = Regs::default();
        service.r[1..3].copy_from_slice(&[KEY_HANDLER, u32::from(id)]);
        self.service(&mut service)?;
        Ok(())
    }

    /// When a code after one now falls due, by `OS_Byte` variable `which`:
    /// never for nought.
    fn repeat_after(&mut self, now: u64, which: u8) -> Option<u64> {
        let after = u64::from(self.byte_var(which));
        (after != 0).then_some(now + after)
    }

    /// The clock's work for the keyboard: the current key's code, if due.
    pub(crate) fn keyboard_tick(&mut self, now: u64) -> Result<(), Unwind> {
        let Some(key) = self.world.keyboard.current else {
            return Ok(());
        };
        self.refresh_leds(false)?;
        if self.world.keyboard.due.is_none_or(|due| now < due) {
            return Ok(());
        }
        self.world.keyboard.due = self.repeat_after(now, var::REPEAT_RATE);
        match self.generate(key, REPEAT) {
            Err(Fail::Unwind(unwind)) => Err(unwind),
            _ => Ok(()),
        }
    }

    /// Whether internal key `key` is down: by the handler's table, the
    /// extension keys by `index` in their ranges. No handler, no key down.
    pub(crate) fn internal_key_down(&mut self, key: u32, index: u32) -> Result<bool, Fail> {
        match key {
            EXTENSION_1 => return Ok(self.world.keyboard.is_down(0x100 + (index & 0x7F))),
            EXTENSION_2 => return Ok(self.world.keyboard.is_down(0x200 + (index & 0x7F))),
            _ => {}
        }
        let Some(handler) = self.handler()? else {
            return Ok(false);
        };
        let word = self.read_word(handler.inkey.wrapping_add(4 * (127 - (key & 0x7F))))?;
        let keys: [u32; 4] = if handler.wide {
            [word & 0xFFFF, word >> 16, 0xFF, 0xFF]
        } else {
            word.to_le_bytes().map(u32::from)
        };
        Ok(keys
            .iter()
            .any(|&low| low != 0xFF && self.world.keyboard.is_down(low)))
    }

    /// `OS_Byte` 121 and the scans: R1 at or above &80 tests that key, below
    /// it scans up from it; R1 the key found, or &FF.
    pub(crate) fn key_scan(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let r1 = regs.r[1] & 0xFF;
        let index = regs.r[2] & 0x7F;
        if r1 >= 0x80 {
            let down = self.internal_key_down(r1 ^ 0x80, index)?;
            regs.r[1] = if down { 0xFF } else { 0 };
            regs.r[2] = regs.r[1];
            regs.set_flag(psr::C, down);
            return Ok(());
        }
        for key in r1..=0x7F {
            let range = match key {
                EXTENSION_1 => Some((0x100, EXTENSION_1)),
                EXTENSION_2 => Some((0x200, EXTENSION_2)),
                _ => None,
            };
            if let Some((start, pseudo)) = range {
                for low in start + index..0x300 {
                    if self.world.keyboard.is_down(low) {
                        let pseudo = if low < 0x200 { pseudo } else { EXTENSION_2 };
                        regs.r[1] = pseudo | ((low & 0xFF) << 8);
                        regs.r[2] = 0;
                        regs.set_flag(psr::C, false);
                        return Ok(());
                    }
                }
            } else if self.internal_key_down(key, 0)? {
                regs.r[1] = key;
                regs.r[2] = 0;
                regs.set_flag(psr::C, false);
                return Ok(());
            }
        }
        regs.r[1] = 0xFF;
        regs.r[2] = SCAN_MISSED;
        regs.set_flag(psr::C, true);
        Ok(())
    }

    /// `OS_Byte` 120: R1 and R2 the internal keys, with their top bits, to
    /// make the current and old keys — each the first low-level key the
    /// handler's table gives it.
    pub(crate) fn write_keys_down(&mut self, regs: &Regs) -> Result<(), Fail> {
        let Some(handler) = self.handler()? else {
            return Ok(());
        };
        let mut keys = [None; 2];
        for (slot, &internal) in keys.iter_mut().zip(&regs.r[1..3]) {
            if internal & 0x80 == 0 {
                continue;
            }
            let word = self.read_word(handler.inkey.wrapping_add(4 * (127 - (internal & 0x7F))))?;
            let first = if handler.wide {
                word & 0xFFFF
            } else {
                word & 0xFF
            };
            *slot = (first != 0xFF).then_some(first);
        }
        self.world.keyboard.current = keys[0];
        self.world.keyboard.old = keys[1];
        Ok(())
    }

    /// The exit a handler's code takes for Break: R1 one for down, nought for
    /// up; what `OS_Byte` 247 says for the modifiers held — the escape key,
    /// or a reset, hard with Ctrl, the boot option reversed with Shift.
    pub(crate) fn break_exit(&mut self) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let down = cpu.reg(tairix_riscos_arm::reg::Reg::field(1)) != 0;
        let state = self.keyboard_status();
        let shift = state & status::SHIFT != 0;
        let ctrl = state & status::CTRL != 0;
        let field = (self.byte_var(var::BREAK_ACTION)
            >> (2 * (u32::from(shift) + 2 * u32::from(ctrl))))
            & 3;
        match (field, down) {
            (BREAK_ESCAPES, true) => {
                let escape = self.byte_var(var::ESCAPE_CHAR);
                self.keyboard_insert(escape)?;
            }
            (BREAK_RESETS, false) => {
                let mut regs = Regs::default();
                regs.r[1] = crate::service::PRE_RESET;
                self.service(&mut regs)?;
                return Err(Unwind::Ended(crate::os::Ended::Reset {
                    hard: ctrl,
                    reverse_boot: shift,
                }));
            }
            _ => {}
        }
        self.return_to_lr();
        Ok(())
    }

    /// The exit a handler's code takes for the mouse's buttons: R0 them.
    pub(crate) fn mouse_exit(&mut self) -> Result<(), Unwind> {
        let buttons = self
            .world
            .processor
            .cpu()
            .reg(tairix_riscos_arm::reg::Reg::field(0))
            & 7;
        let mouse = &self.world.input.mouse;
        let (x, y, held) = (mouse.x, mouse.y, mouse.buttons);
        self.pointer_moved(x, y, (held & !7) | buttons)?;
        self.return_to_lr();
        Ok(())
    }

    /// The keyboard every session has said to be there: a PC's.
    pub(crate) fn keyboard_arrives(&mut self) -> Result<(), Unwind> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[PRESENT, PC]);
        self.call_vector(vectors::KEY, &mut regs, Entry::SupervisorNoIrq)
    }
}

#[cfg(test)]
mod tests;

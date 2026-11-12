//! INT 09h and INT 16h for the 83-key keyboard: scan codes become the key
//! codes software reads, through the shift states, Caps and Num Lock, the
//! keypad's Alt codes, Ctrl+Break, Pause, print screen and Ctrl+Alt+Del.

use super::bda;
use super::guest::Guest;
use super::layout::{entry, SEGMENT};
use crate::asm::{Reg16, Reg8};
use crate::chips::keyboard::{us_key, Scan, SHIFT};
use crate::chips::ppi::port_b;
use crate::cpu::{flag, Bus as _};
use crate::machine::Machine;

/// Where the CPU's flags lie above the stack pointer in INT 16h's stub.
const FLAGS: u16 = 4;

/// The shift flags at `40:17`.
#[allow(missing_docs, reason = "each is the key of its name, or its lock")]
pub mod shift {
    pub const RIGHT_SHIFT: u8 = 0x01;
    pub const LEFT_SHIFT: u8 = 0x02;
    pub const CTRL: u8 = 0x04;
    pub const ALT: u8 = 0x08;
    pub const SCROLL: u8 = 0x10;
    pub const NUM: u8 = 0x20;
    pub const CAPS: u8 = 0x40;
    pub const INSERT: u8 = 0x80;
}

/// The keys held at `40:18`, and the pause.
mod held {
    pub const PAUSE: u8 = 0x08;
}

/// What the stub does after INT 09h's service.
mod then {
    pub const NOTHING: u8 = 0;
    pub const BREAK: u8 = 1;
    pub const PRINT_SCREEN: u8 = 2;
    pub const WAIT: u8 = 3;
}

/// The stub's state: whether the buffer-full beep is sounding, whether it
/// masked the keyboard's interrupt, and the tick it stops at.
pub const STATE: u8 = 4;
const BEEPING: u16 = 0;
const MASKED: u16 = 1;
const BEEP_END: u16 = 2;
/// The interrupt controller's mask register, and the keyboard's line in it.
const MASK_PORT: u16 = 0x21;
const KEYBOARD_LINE: u8 = 0x02;
/// The beep a full buffer makes, two ticks.
const BEEP_TICKS: u16 = 2;

const NONE: u16 = 0xFFFF;
/// What Ctrl+C puts in the buffer: the C key's scan code and the character.
pub const CTRL_C_KEY: u16 = 0x2E03;

/// Each key's code plain, shifted, with Ctrl and with Alt, by scan code:
/// the scan code in the high byte and the character in the low, or nothing.
#[rustfmt::skip]
const KEYS: [[u16; 4]; 0x54] = [
    [NONE, NONE, NONE, NONE],
    [0x011B, 0x011B, 0x011B, NONE],
    [0x0231, 0x0221, NONE, 0x7800],
    [0x0332, 0x0340, 0x0300, 0x7900],
    [0x0433, 0x0423, NONE, 0x7A00],
    [0x0534, 0x0524, NONE, 0x7B00],
    [0x0635, 0x0625, NONE, 0x7C00],
    [0x0736, 0x075E, 0x071E, 0x7D00],
    [0x0837, 0x0826, NONE, 0x7E00],
    [0x0938, 0x092A, NONE, 0x7F00],
    [0x0A39, 0x0A28, NONE, 0x8000],
    [0x0B30, 0x0B29, NONE, 0x8100],
    [0x0C2D, 0x0C5F, 0x0C1F, 0x8200],
    [0x0D3D, 0x0D2B, NONE, 0x8300],
    [0x0E08, 0x0E08, 0x0E7F, NONE],
    [0x0F09, 0x0F00, NONE, NONE],
    [0x1071, 0x1051, 0x1011, 0x1000],
    [0x1177, 0x1157, 0x1117, 0x1100],
    [0x1265, 0x1245, 0x1205, 0x1200],
    [0x1372, 0x1352, 0x1312, 0x1300],
    [0x1474, 0x1454, 0x1414, 0x1400],
    [0x1579, 0x1559, 0x1519, 0x1500],
    [0x1675, 0x1655, 0x1615, 0x1600],
    [0x1769, 0x1749, 0x1709, 0x1700],
    [0x186F, 0x184F, 0x180F, 0x1800],
    [0x1970, 0x1950, 0x1910, 0x1900],
    [0x1A5B, 0x1A7B, 0x1A1B, NONE],
    [0x1B5D, 0x1B7D, 0x1B1D, NONE],
    [0x1C0D, 0x1C0D, 0x1C0A, NONE],
    [NONE, NONE, NONE, NONE],
    [0x1E61, 0x1E41, 0x1E01, 0x1E00],
    [0x1F73, 0x1F53, 0x1F13, 0x1F00],
    [0x2064, 0x2044, 0x2004, 0x2000],
    [0x2166, 0x2146, 0x2106, 0x2100],
    [0x2267, 0x2247, 0x2207, 0x2200],
    [0x2368, 0x2348, 0x2308, 0x2300],
    [0x246A, 0x244A, 0x240A, 0x2400],
    [0x256B, 0x254B, 0x250B, 0x2500],
    [0x266C, 0x264C, 0x260C, 0x2600],
    [0x273B, 0x273A, NONE, NONE],
    [0x2827, 0x2822, NONE, NONE],
    [0x2960, 0x297E, NONE, NONE],
    [NONE, NONE, NONE, NONE],
    [0x2B5C, 0x2B7C, 0x2B1C, NONE],
    [0x2C7A, 0x2C5A, 0x2C1A, 0x2C00],
    [0x2D78, 0x2D58, 0x2D18, 0x2D00],
    [0x2E63, 0x2E43, CTRL_C_KEY, 0x2E00],
    [0x2F76, 0x2F56, 0x2F16, 0x2F00],
    [0x3062, 0x3042, 0x3002, 0x3000],
    [0x316E, 0x314E, 0x310E, 0x3100],
    [0x326D, 0x324D, 0x320D, 0x3200],
    [0x332C, 0x333C, NONE, NONE],
    [0x342E, 0x343E, NONE, NONE],
    [0x352F, 0x353F, NONE, NONE],
    [NONE, NONE, NONE, NONE],
    [0x372A, NONE, 0x7200, NONE],
    [NONE, NONE, NONE, NONE],
    [0x3920, 0x3920, 0x3920, 0x3920],
    [NONE, NONE, NONE, NONE],
    [0x3B00, 0x5400, 0x5E00, 0x6800],
    [0x3C00, 0x5500, 0x5F00, 0x6900],
    [0x3D00, 0x5600, 0x6000, 0x6A00],
    [0x3E00, 0x5700, 0x6100, 0x6B00],
    [0x3F00, 0x5800, 0x6200, 0x6C00],
    [0x4000, 0x5900, 0x6300, 0x6D00],
    [0x4100, 0x5A00, 0x6400, 0x6E00],
    [0x4200, 0x5B00, 0x6500, 0x6F00],
    [0x4300, 0x5C00, 0x6600, 0x7000],
    [0x4400, 0x5D00, 0x6700, 0x7100],
    [NONE, NONE, NONE, NONE],
    [NONE, NONE, NONE, NONE],
    [0x4700, 0x4737, 0x7700, NONE],
    [0x4800, 0x4838, NONE, NONE],
    [0x4900, 0x4939, 0x8400, NONE],
    [0x4A2D, 0x4A2D, NONE, NONE],
    [0x4B00, 0x4B34, 0x7300, NONE],
    [NONE, 0x4C35, NONE, NONE],
    [0x4D00, 0x4D36, 0x7400, NONE],
    [0x4E2B, 0x4E2B, NONE, NONE],
    [0x4F00, 0x4F31, 0x7500, NONE],
    [0x5000, 0x5032, NONE, NONE],
    [0x5100, 0x5133, 0x7600, NONE],
    [0x5200, 0x5230, NONE, NONE],
    [0x5300, 0x532E, NONE, NONE],
];

/// The keypad's digits for Alt codes, by scan code from 47h.
const KEYPAD_DIGITS: [Option<u8>; 13] = [
    Some(7),
    Some(8),
    Some(9),
    None,
    Some(4),
    Some(5),
    Some(6),
    None,
    Some(1),
    Some(2),
    Some(3),
    Some(0),
    None,
];

const fn is_letter(scan: u8) -> bool {
    matches!(scan, 0x10..=0x19 | 0x1E..=0x26 | 0x2C..=0x32)
}

/// Whether `scan` is a keypad key Num Lock makes a digit: its grey minus
/// and plus are not.
#[must_use]
pub const fn on_keypad(scan: u8) -> bool {
    matches!(scan, 0x47..=0x53) && scan != 0x4A && scan != 0x4E
}

/// The keyboard's byte has been read: the clear line pulses, so the
/// keyboard can send its next.
pub fn acknowledge(g: &mut Guest<'_>) {
    let b = g.input(0x61);
    g.output(0x61, b | port_b::KEYBOARD_CLEAR);
    g.output(0x61, b & !port_b::KEYBOARD_CLEAR);
}

/// The keyboard's buffer as the BDA's pointers describe it: its bounds,
/// and the slots its keys are read from and written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    /// The first slot.
    pub start: u16,
    /// The end past the last slot.
    pub end: u16,
    /// The slot the next key is read from.
    pub head: u16,
    /// The slot the next key is written to.
    pub tail: u16,
}

impl Ring {
    /// The buffer as the BDA has it now.
    pub fn read(g: &mut Guest<'_>) -> Self {
        Self::of(|offset| g.bda16(offset))
    }

    /// The buffer as `word` reads the BDA's words.
    fn of(mut word: impl FnMut(u16) -> u16) -> Self {
        Self {
            start: word(bda::KEY_START),
            end: word(bda::KEY_END),
            head: word(bda::KEY_HEAD),
            tail: word(bda::KEY_TAIL),
        }
    }

    /// The keys from the head to the tail of a ring that
    /// [`is_sound`](Self::is_sound).
    fn len(self) -> usize {
        let span = usize::from(self.end - self.start);
        (usize::from(self.tail) + span - usize::from(self.head)) % span / 2
    }

    /// Whether a program may walk it: its bounds a ring of whole slots,
    /// its head and tail slots of it.
    #[must_use]
    pub fn is_sound(self) -> bool {
        let slot =
            |at: u16| (self.start..self.end).contains(&at) && (at - self.start).is_multiple_of(2);
        self.start < self.end
            && (self.end - self.start).is_multiple_of(2)
            && slot(self.head)
            && slot(self.tail)
    }

    /// The slot after `at`, round to the first after the last.
    #[must_use]
    pub fn after(self, at: u16) -> u16 {
        let next = at.wrapping_add(2);
        if next >= self.end {
            self.start
        } else {
            next
        }
    }

    /// The slot before `at`, round to the last before the first.
    #[must_use]
    pub fn before(self, at: u16) -> u16 {
        if at <= self.start {
            self.end - 2
        } else {
            at - 2
        }
    }
}

/// A byte of the BIOS data area, as `machine`'s memory holds it.
fn bda_byte(machine: &Machine, offset: u16) -> u8 {
    machine
        .board()
        .peek(crate::cpu::linear(bda::SEGMENT, offset))
}

/// The keys waiting in the BIOS's buffer; none when its pointers name no
/// ring a program could walk, as when a program keeps the keyboard's codes
/// itself.
#[must_use]
pub fn keys_waiting(machine: &Machine) -> Option<usize> {
    let ring = Ring::of(|offset| {
        u16::from_le_bytes([
            bda_byte(machine, offset),
            bda_byte(machine, offset.wrapping_add(1)),
        ])
    });
    ring.is_sound().then(|| ring.len())
}

/// Whether Caps Lock is on, as the BIOS's shift flags hold it.
#[must_use]
pub fn caps_lock(machine: &Machine) -> bool {
    bda_byte(machine, bda::SHIFT_FLAGS) & shift::CAPS != 0
}

/// The left Alt key, held while a code is typed on the keypad.
const ALT: Scan = Scan::plain(0x38);

/// Each digit's keypad key, as [`KEYPAD_DIGITS`] lays them out.
const DIGIT_KEYS: [Scan; 10] = digit_keys();

const fn digit_keys() -> [Scan; 10] {
    let mut keys = [Scan::plain(0); 10];
    let mut at = 0u8;
    while (at as usize) < KEYPAD_DIGITS.len() {
        if let Some(digit) = KEYPAD_DIGITS[at as usize] {
            keys[digit as usize] = Scan::plain(0x47 + at);
        }
        at += 1;
    }
    keys
}

/// The most key events one character takes: Alt, a code's three digits
/// each pressed and let go, and Alt again.
const MOST_STROKES: usize = 8;

/// The key events that type one character, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keystrokes {
    events: [(Scan, bool); MOST_STROKES],
    len: usize,
}

impl Keystrokes {
    /// The events, in order: each key and whether it goes down.
    #[must_use]
    pub fn as_slice(&self) -> &[(Scan, bool)] {
        &self.events[..self.len]
    }

    fn push(&mut self, key: Scan, down: bool) {
        if let Some(slot) = self.events.get_mut(self.len) {
            *slot = (key, down);
            self.len += 1;
        }
    }

    fn tap(&mut self, key: Scan) {
        self.push(key, true);
        self.push(key, false);
    }
}

/// The keys that type code page 437 byte `code` through this BIOS while Caps
/// Lock is `caps_lock`: its key on the US layout, with Shift where the
/// layout and the lock call for it, else Alt held over its decimal value on
/// the keypad. No key types `00h`.
#[must_use]
pub fn keystrokes(code: u8, caps_lock: bool) -> Keystrokes {
    let mut strokes = Keystrokes::default();
    if let Some((key, shifted)) = us_key(code) {
        let shift = shifted != (caps_lock && is_letter(key.code));
        if shift {
            strokes.push(SHIFT, true);
        }
        strokes.tap(key);
        if shift {
            strokes.push(SHIFT, false);
        }
    } else if code != 0 {
        strokes.push(ALT, true);
        let digits = [code / 100, code / 10 % 10, code % 10];
        let first = digits.iter().position(|&digit| digit != 0).unwrap_or(2);
        for &digit in &digits[first..] {
            strokes.tap(DIGIT_KEYS[usize::from(digit)]);
        }
        strokes.push(ALT, false);
    }
    strokes
}

/// Puts `key` at the buffer's tail, or beeps when it is full.
fn enqueue(g: &mut Guest<'_>, key: u16) -> bool {
    let ring = Ring::read(g);
    let next = ring.after(ring.tail);
    if next == ring.head {
        return false;
    }
    g.set_bda16(ring.tail, key);
    g.set_bda16(bda::KEY_TAIL, next);
    true
}

fn next_key(g: &mut Guest<'_>, remove: bool) -> Option<u16> {
    let ring = Ring::read(g);
    if ring.head == ring.tail {
        return None;
    }
    let key = g.bda16(ring.head);
    if remove {
        g.set_bda16(bda::KEY_HEAD, ring.after(ring.head));
    }
    Some(key)
}

/// INT 09h's service, the scan code in AL: ends the interrupt and leaves in
/// AH what the stub does next.
pub fn interrupt(g: &mut Guest<'_>) {
    g.set_local(BEEPING, 0);
    g.set_local(MASKED, 0);
    let code = g.r8(Reg8::Al);
    let next = process(g, code);
    g.output(0x20, 0x20);
    g.set_r8(Reg8::Ah, next);
}

fn process(g: &mut Guest<'_>, code: u8) -> u8 {
    let scan = code & 0x7F;
    let release = code & 0x80 != 0;
    let flags = g.bda8(bda::SHIFT_FLAGS);
    if let Some(next) = shift_key(g, scan, release, flags) {
        return next;
    }
    if lock_key(g, scan, release, flags) || release {
        return then::NOTHING;
    }
    let held = g.bda8(bda::SHIFT_FLAGS_2);
    if held & held::PAUSE != 0 {
        if scan != 0x45 {
            g.set_bda8(bda::SHIFT_FLAGS_2, held & !held::PAUSE);
        }
        return then::NOTHING;
    }
    key(g, scan, flags, held)
}

/// The Shift, Ctrl and Alt keys, which only change the shift flags; Alt's
/// release delivers a code typed on the keypad.
fn shift_key(g: &mut Guest<'_>, scan: u8, release: bool, flags: u8) -> Option<u8> {
    let bit = match scan {
        0x36 => shift::RIGHT_SHIFT,
        0x2A => shift::LEFT_SHIFT,
        0x1D => shift::CTRL,
        0x38 => shift::ALT,
        _ => return None,
    };
    g.set_bda8(
        bda::SHIFT_FLAGS,
        if release { flags & !bit } else { flags | bit },
    );
    if release && bit == shift::ALT {
        let code = g.bda8(bda::ALT_KEYPAD);
        g.set_bda8(bda::ALT_KEYPAD, 0);
        if code != 0 && !enqueue(g, u16::from(code)) {
            return Some(beep(g));
        }
    }
    Some(then::NOTHING)
}

/// The lock keys, which toggle once a press however long it is held;
/// whether the key is wholly handled. Insert is also a key, so it goes on
/// to the buffer.
fn lock_key(g: &mut Guest<'_>, scan: u8, release: bool, flags: u8) -> bool {
    let shifted = flags & (shift::LEFT_SHIFT | shift::RIGHT_SHIFT) != 0;
    let as_insert = flags & shift::ALT == 0 && (flags & shift::NUM != 0) == shifted;
    let bit = match scan {
        0x3A => shift::CAPS,
        0x45 if flags & shift::CTRL == 0 => shift::NUM,
        0x46 if flags & shift::CTRL == 0 => shift::SCROLL,
        0x52 if as_insert => shift::INSERT,
        _ => return false,
    };
    let held = g.bda8(bda::SHIFT_FLAGS_2);
    if release {
        g.set_bda8(bda::SHIFT_FLAGS_2, held & !bit);
        return true;
    }
    if held & bit != 0 {
        return true;
    }
    g.set_bda8(bda::SHIFT_FLAGS_2, held | bit);
    g.set_bda8(bda::SHIFT_FLAGS, flags ^ bit);
    bit != shift::INSERT
}

/// A key's press: the system keys, Alt's keypad digits, and otherwise its
/// code to the buffer.
fn key(g: &mut Guest<'_>, scan: u8, flags: u8, held: u8) -> u8 {
    let ctrl = flags & shift::CTRL != 0;
    let alt = flags & shift::ALT != 0;
    let shifted = flags & (shift::LEFT_SHIFT | shift::RIGHT_SHIFT) != 0;
    match scan {
        0x53 if ctrl && alt => {
            g.set_bda16(bda::RESET_FLAG, bda::WARM);
            g.jump(SEGMENT, entry::POST);
            return then::NOTHING;
        }
        0x45 if ctrl => {
            g.set_bda8(bda::SHIFT_FLAGS_2, held | held::PAUSE);
            return then::WAIT;
        }
        0x46 if ctrl => {
            let start = g.bda16(bda::KEY_START);
            g.set_bda16(bda::KEY_HEAD, start);
            g.set_bda16(bda::KEY_TAIL, start);
            g.set_bda8(bda::BREAK, 0x80);
            enqueue(g, 0);
            return then::BREAK;
        }
        0x37 if shifted => return then::PRINT_SCREEN,
        _ => {}
    }
    if alt && on_keypad(scan) {
        if let Some(digit) = KEYPAD_DIGITS
            .get(usize::from(scan - 0x47))
            .copied()
            .flatten()
        {
            let code = g.bda8(bda::ALT_KEYPAD);
            g.set_bda8(bda::ALT_KEYPAD, code.wrapping_mul(10).wrapping_add(digit));
        }
        return then::NOTHING;
    }
    let Some(row) = KEYS.get(usize::from(scan)) else {
        return then::NOTHING;
    };
    let column = if alt {
        3
    } else if ctrl {
        2
    } else if on_keypad(scan) {
        usize::from((flags & shift::NUM != 0) != shifted)
    } else if is_letter(scan) {
        usize::from((flags & shift::CAPS != 0) != shifted)
    } else {
        usize::from(shifted)
    };
    match row[column] {
        NONE => then::NOTHING,
        key if enqueue(g, key) => then::NOTHING,
        _ => beep(g),
    }
}

/// Starts the buffer-full beep; the stub waits it out with the timer
/// running. A key pressed meanwhile would nest the handler, each nesting
/// beeping again on the same stack, so the keyboard keeps its codes until
/// the beep is over, as the XT's beep with interrupts off made it.
fn beep(g: &mut Guest<'_>) -> u8 {
    let on = g.input(0x61) | port_b::TIMER2_GATE | port_b::SPEAKER_DATA;
    g.output(0x61, on);
    let end = g.bda16(bda::TICKS).wrapping_add(BEEP_TICKS);
    g.set_local(BEEPING, 1);
    let [low, high] = end.to_le_bytes();
    g.set_local(BEEP_END, low);
    g.set_local(BEEP_END + 1, high);
    let mask = g.input(MASK_PORT);
    if mask & KEYBOARD_LINE == 0 {
        g.output(MASK_PORT, mask | KEYBOARD_LINE);
        g.set_local(MASKED, 1);
    }
    then::WAIT
}

/// The stub's wait, for a pause to end or a beep to stop.
pub fn paused(g: &mut Guest<'_>) {
    if g.bda8(bda::SHIFT_FLAGS_2) & held::PAUSE != 0 {
        g.done(false);
        return;
    }
    if g.local(BEEPING) != 0 {
        let end = u16::from_le_bytes([g.local(BEEP_END), g.local(BEEP_END + 1)]);
        if g.bda16(bda::TICKS).wrapping_sub(end) >= 0x8000 {
            g.done(false);
            return;
        }
        let off = g.input(0x61) & !(port_b::TIMER2_GATE | port_b::SPEAKER_DATA);
        g.output(0x61, off);
        g.set_local(BEEPING, 0);
        if g.local(MASKED) != 0 {
            let mask = g.input(MASK_PORT);
            g.output(MASK_PORT, mask & !KEYBOARD_LINE);
            g.set_local(MASKED, 0);
        }
    }
    g.done(true);
}

/// INT 16h. Reading a key waits for one; the enhanced functions read the
/// same keys, as an 83-key keyboard has no others.
pub fn service(g: &mut Guest<'_>) {
    match g.r8(Reg8::Ah) {
        0x00 | 0x10 => match next_key(g, true) {
            Some(key) => g.set_r16(Reg16::Ax, key),
            None => return g.done(false),
        },
        0x01 | 0x11 => {
            let key = next_key(g, false);
            if let Some(key) = key {
                g.set_r16(Reg16::Ax, key);
            }
            g.set_returned_flag(FLAGS, flag::ZF, key.is_none());
        }
        0x02 => {
            let flags = g.bda8(bda::SHIFT_FLAGS);
            g.set_r8(Reg8::Al, flags);
        }
        0x05 => {
            let key = g.r16(Reg16::Cx);
            let stored = enqueue(g, key);
            g.set_r8(Reg8::Al, u8::from(!stored));
        }
        0x12 => {
            let flags = g.bda8(bda::SHIFT_FLAGS);
            let held = g.bda8(bda::SHIFT_FLAGS_2);
            let sides = ((flags & shift::CTRL) >> 2) | ((flags & shift::ALT) >> 2);
            g.set_r16(
                Reg16::Ax,
                u16::from_le_bytes([flags, (held & 0x70) | sides]),
            );
        }
        _ => {}
    }
    g.done(true);
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;

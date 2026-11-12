//! The keyboard: the enhanced 101-key keyboard in XT mode, sending scan code
//! set 1 over its serial link to the 8255, with its power-on self-test, its
//! buffer, and its own typematic repeat.
//!
//! The host reports keys going down and up; repeats are the keyboard's, at
//! the XT keyboard's rate, never the host's. An XT BIOS reads an `E0` prefix
//! as the release of a key it does not have, so the grey keys reach it as the
//! keypad's, which the keyboard keeps true to their labels with the fake
//! shifts IBM's enhanced keyboard sends: it counts Num Lock's presses itself,
//! as an XT-mode keyboard must, having no way to be told the lock's state.

use crate::time::Nanos;
use tairix_abi::input::PhysicalKey;

/// A key, as scan code set 1 names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Scan {
    /// The make code.
    pub code: u8,
    /// Whether the key is one of the enhanced keyboard's `E0`-prefixed keys.
    pub extended: bool,
}

impl Scan {
    /// A key of the original 83-key layout.
    #[must_use]
    pub const fn plain(code: u8) -> Self {
        Self {
            code: code & 0x7F,
            extended: false,
        }
    }

    /// An enhanced keyboard's `E0`-prefixed key.
    #[must_use]
    pub const fn extended(code: u8) -> Self {
        Self {
            code: code & 0x7F,
            extended: true,
        }
    }
}

/// The make codes of the letters `A` to `Z`.
const LETTERS: &[u8; 26] = &[
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
];

/// The key that types ASCII `byte` on the US layout, and whether Shift is
/// held for it: the printable characters, Enter, Tab, Backspace and
/// Escape; `None` for a byte no key types.
#[must_use]
pub const fn us_key(byte: u8) -> Option<(Scan, bool)> {
    let (code, shifted) = match byte {
        b'a'..=b'z' => (LETTERS[(byte - b'a') as usize], false),
        b'A'..=b'Z' => (LETTERS[(byte - b'A') as usize], true),
        b'1'..=b'9' => (byte - b'1' + 0x02, false),
        b'0' => (0x0B, false),
        b'!' => (0x02, true),
        b'@' => (0x03, true),
        b'#' => (0x04, true),
        b'$' => (0x05, true),
        b'%' => (0x06, true),
        b'^' => (0x07, true),
        b'&' => (0x08, true),
        b'*' => (0x09, true),
        b'(' => (0x0A, true),
        b')' => (0x0B, true),
        b'-' => (0x0C, false),
        b'_' => (0x0C, true),
        b'=' => (0x0D, false),
        b'+' => (0x0D, true),
        b'[' => (0x1A, false),
        b'{' => (0x1A, true),
        b']' => (0x1B, false),
        b'}' => (0x1B, true),
        b';' => (0x27, false),
        b':' => (0x27, true),
        b'\'' => (0x28, false),
        b'"' => (0x28, true),
        b'`' => (0x29, false),
        b'~' => (0x29, true),
        b'\\' => (0x2B, false),
        b'|' => (0x2B, true),
        b',' => (0x33, false),
        b'<' => (0x33, true),
        b'.' => (0x34, false),
        b'>' => (0x34, true),
        b'/' => (0x35, false),
        b'?' => (0x35, true),
        b' ' => (0x39, false),
        b'\r' => (0x1C, false),
        b'\t' => (0x0F, false),
        0x08 => (0x0E, false),
        0x1B => (0x01, false),
        _ => return None,
    };
    Some((Scan::plain(code), shifted))
}

/// The key at host position `key` on the enhanced keyboard, `None` for one
/// it does not have: the GUI and application keys, which the desktop keeps,
/// and every key past the 102 of the international layout.
#[must_use]
pub fn scan_of(key: PhysicalKey) -> Option<Scan> {
    let usage = key.usage();
    let code: u16 = match usage {
        0x04..=0x1D => u16::from(LETTERS[usize::from(usage - 0x04)]),
        0x1E..=0x26 => usage - 0x1E + 0x02,
        0x27 => 0x0B,
        0x28 => 0x1C,
        0x29 => 0x01,
        0x2A => 0x0E,
        0x2B => 0x0F,
        0x2C => 0x39,
        0x2D => 0x0C,
        0x2E => 0x0D,
        0x2F => 0x1A,
        0x30 => 0x1B,
        // The international layout's key beside Enter sends what the US
        // layout's backslash does.
        0x31 | 0x32 => 0x2B,
        0x33 => 0x27,
        0x34 => 0x28,
        0x35 => 0x29,
        0x36 => 0x33,
        0x37 => 0x34,
        0x38 => 0x35,
        0x39 => 0x3A,
        0x3A..=0x43 => usage - 0x3A + 0x3B,
        0x44 => 0x57,
        0x45 => 0x58,
        0x46 => return Some(PRINT_SCREEN),
        0x47 => 0x46,
        0x48 => return Some(PAUSE),
        0x49 => return Some(Scan::extended(0x52)),
        0x4A => return Some(Scan::extended(0x47)),
        0x4B => return Some(Scan::extended(0x49)),
        0x4C => return Some(Scan::extended(0x53)),
        0x4D => return Some(Scan::extended(0x4F)),
        0x4E => return Some(Scan::extended(0x51)),
        0x4F => return Some(Scan::extended(0x4D)),
        0x50 => return Some(Scan::extended(0x4B)),
        0x51 => return Some(Scan::extended(0x50)),
        0x52 => return Some(Scan::extended(0x48)),
        0x53 => 0x45,
        0x54 => return Some(Scan::extended(0x35)),
        0x55 => 0x37,
        0x56 => 0x4A,
        0x57 => 0x4E,
        0x58 => return Some(Scan::extended(0x1C)),
        0x59 => 0x4F,
        0x5A => 0x50,
        0x5B => 0x51,
        0x5C => 0x4B,
        0x5D => 0x4C,
        0x5E => 0x4D,
        0x5F => 0x47,
        0x60 => 0x48,
        0x61 => 0x49,
        0x62 => 0x52,
        0x63 => 0x53,
        0x64 => 0x56,
        0xE0 => 0x1D,
        0xE1 => 0x2A,
        0xE2 => 0x38,
        0xE4 => return Some(Scan::extended(0x1D)),
        0xE5 => 0x36,
        0xE6 => return Some(Scan::extended(0x38)),
        _ => return None,
    };
    u8::try_from(code).ok().map(Scan::plain)
}

/// The left Shift key.
pub const SHIFT: Scan = Scan::plain(0x2A);
/// The right Shift key.
const RIGHT_SHIFT: Scan = Scan::plain(0x36);
/// The Num Lock key.
const NUM_LOCK: Scan = Scan::plain(0x45);
/// Print Screen, whose make carries a fake Shift for a BIOS that prints on
/// Shift with the keypad's `*`.
pub const PRINT_SCREEN: Scan = Scan::extended(0x37);
/// Pause, which has no code of its own: the keyboard sends Ctrl and Num
/// Lock under an `E1` prefix as it goes down, and nothing as it comes up.
pub const PAUSE: Scan = Scan::extended(0x45);

/// The make code of the Shift key an `E0`-prefixed fake shift names.
const FAKE_LEFT: u8 = 0x2A;
const FAKE_RIGHT: u8 = 0x36;
/// What Alt with Print Screen sends: the System Request key.
const SYS_REQ: u8 = 0x54;
/// What Ctrl with Pause sends: the 83-key keyboard's Break.
const BREAK: u8 = 0x46;

/// The bytes one key edge sends; six at most, Pause's.
struct Sequence {
    bytes: [u8; 6],
    len: usize,
}

impl Sequence {
    const fn new() -> Self {
        Self {
            bytes: [0; 6],
            len: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.bytes.get_mut(self.len) {
            *slot = byte;
            self.len += 1;
        }
    }

    fn code(&mut self, key: Scan, break_code: bool) {
        if key.extended {
            self.push(0xE0);
        }
        self.push(key.code | if break_code { 0x80 } else { 0 });
    }

    fn fake(&mut self, shift: u8, break_code: bool) {
        self.code(
            Scan {
                code: shift,
                extended: true,
            },
            break_code,
        );
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The grey keys a fake shift keeps apart from the keypad's: Insert,
/// Delete, Home, End, the page keys and the arrows.
const fn is_grey_cursor(key: Scan) -> bool {
    key.extended && matches!(key.code, 0x47..=0x53) && !matches!(key.code, 0x4A | 0x4C | 0x4E)
}

/// Bytes the keyboard's controller buffers before it reports an overrun.
pub const BUFFER: usize = 16;
/// The byte sent in place of what an overrun lost.
const OVERRUN: u8 = 0xFF;
/// The self-test's result.
const SELF_TEST_PASSED: u8 = 0xAA;
/// One byte's transmission over the keyboard's serial link.
const BYTE_TIME: Nanos = 1_000_000;
/// How long the clock line must be held low to reset the keyboard.
const RESET_HOLD: Nanos = 20_000_000;
/// The self-test's duration after the clock line is released.
const SELF_TEST_TIME: Nanos = 10_000_000;
/// The delay before a held key repeats, and the interval between repeats:
/// the XT keyboard's fixed half second and 10.9 characters a second.
const TYPEMATIC_DELAY: Nanos = 500_000_000;
const TYPEMATIC_INTERVAL: Nanos = 91_743_119;

/// The keyboard.
#[derive(Clone, Debug)]
pub struct Keyboard {
    buffer: [u8; BUFFER],
    head: usize,
    len: usize,
    /// The byte in the 8255's shift register, raising IRQ 1.
    shift: Option<u8>,
    /// When the next byte may leave the keyboard.
    next_send: Nanos,
    clear_held: bool,
    clock_low_since: Option<Nanos>,
    held: [u64; 4],
    repeating: Option<(Scan, Nanos)>,
    /// Num Lock as the keyboard counts it.
    num_lock: bool,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    /// A keyboard with nothing pressed and nothing to send.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [0; BUFFER],
            head: 0,
            len: 0,
            shift: None,
            next_send: 0,
            clear_held: false,
            clock_low_since: None,
            held: [0; 4],
            repeating: None,
            num_lock: false,
        }
    }

    fn enqueue(&mut self, byte: u8) {
        if self.len == BUFFER {
            let last = (self.head + BUFFER - 1) % BUFFER;
            self.buffer[last] = OVERRUN;
            return;
        }
        self.buffer[(self.head + self.len) % BUFFER] = byte;
        self.len += 1;
    }

    fn send(&mut self, sequence: &Sequence) {
        for &byte in sequence.as_slice() {
            self.enqueue(byte);
        }
    }

    fn slot(key: Scan) -> (usize, u64) {
        let index = usize::from(key.code) | if key.extended { 0x80 } else { 0 };
        (index / 64, 1u64 << (index % 64))
    }

    fn is_held(&self, key: Scan) -> bool {
        let (word, bit) = Self::slot(key);
        self.held[word] & bit != 0
    }

    /// Whether either Control or either Alt key is held.
    fn holds(&self, code: u8) -> bool {
        self.is_held(Scan::plain(code)) || self.is_held(Scan::extended(code))
    }

    /// What key `key` going down (`!break_code`) or up sends now.
    fn sequence(&self, key: Scan, break_code: bool) -> Sequence {
        let mut out = Sequence::new();
        let left = self.is_held(SHIFT);
        let right = self.is_held(RIGHT_SHIFT);
        let shifted = left || right;
        if key == PAUSE {
            if !break_code {
                if self.holds(0x1D) {
                    out.code(Scan::extended(BREAK), false);
                    out.code(Scan::extended(BREAK), true);
                } else {
                    for byte in [0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5] {
                        out.push(byte);
                    }
                }
            }
        } else if key == PRINT_SCREEN && self.holds(0x38) {
            out.code(Scan::plain(SYS_REQ), break_code);
        } else if key == PRINT_SCREEN && !shifted && !self.holds(0x1D) {
            if !break_code {
                out.fake(FAKE_LEFT, false);
            }
            out.code(key, break_code);
            if break_code {
                out.fake(FAKE_LEFT, true);
            }
        } else if is_grey_cursor(key) && self.num_lock != shifted {
            // Num Lock alone shifts the key back out of the keypad, and a
            // Shift without it has to be let go of first.
            let undo = !self.num_lock;
            let fakes = [(left || self.num_lock, FAKE_LEFT), (right, FAKE_RIGHT)];
            if !break_code {
                for (held, shift) in fakes {
                    if held {
                        out.fake(shift, undo);
                    }
                }
            }
            out.code(key, break_code);
            if break_code {
                for (held, shift) in fakes.into_iter().rev() {
                    if held {
                        out.fake(shift, !undo);
                    }
                }
            }
        } else if key == Scan::extended(0x35) && shifted {
            let fakes = [(left, FAKE_LEFT), (right, FAKE_RIGHT)];
            if !break_code {
                for (held, shift) in fakes {
                    if held {
                        out.fake(shift, true);
                    }
                }
            }
            out.code(key, break_code);
            if break_code {
                for (held, shift) in fakes.into_iter().rev() {
                    if held {
                        out.fake(shift, false);
                    }
                }
            }
        } else {
            out.code(key, break_code);
        }
        out
    }

    /// Whether key `key` going down (`down`) or up now would fit in the
    /// buffer rather than overrun it.
    #[must_use]
    pub fn fits(&self, key: Scan, down: bool) -> bool {
        if !down && !self.is_held(key) {
            return true;
        }
        self.sequence(key, !down).len <= self.room()
    }

    /// A key goes down at `now`.
    pub fn press(&mut self, key: Scan, now: Nanos) {
        // The BIOS reads Num Lock with Ctrl as Pause, and leaves the lock.
        if key == NUM_LOCK && !self.is_held(key) && !self.holds(0x1D) {
            self.num_lock = !self.num_lock;
        }
        let (word, bit) = Self::slot(key);
        self.held[word] |= bit;
        let sequence = self.sequence(key, false);
        self.send(&sequence);
        self.repeating = (key != PAUSE).then_some((key, now + TYPEMATIC_DELAY));
    }

    /// A key comes up.
    pub fn release(&mut self, key: Scan) {
        let (word, bit) = Self::slot(key);
        if self.held[word] & bit == 0 {
            return;
        }
        self.held[word] &= !bit;
        let sequence = self.sequence(key, true);
        self.send(&sequence);
        if self.repeating.is_some_and(|(k, _)| k == key) {
            self.repeating = None;
        }
    }

    /// Whether every byte the keyboard had to send has been read: none is
    /// buffered, and the 8255 holds none.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.len == 0 && self.shift.is_none()
    }

    /// Bytes the buffer can take before a key overruns it.
    #[must_use]
    pub const fn room(&self) -> usize {
        BUFFER - self.len
    }

    /// Whether any key is held down.
    #[must_use]
    pub fn any_held(&self) -> bool {
        self.held.iter().any(|&word| word != 0)
    }

    /// The keys held down, the lowest code first.
    pub fn held_keys(&self) -> impl Iterator<Item = Scan> + '_ {
        (0..=0xFFu8)
            .map(|index| {
                if index & 0x80 != 0 {
                    Scan::extended(index)
                } else {
                    Scan::plain(index)
                }
            })
            .filter(|&key| self.is_held(key))
    }

    /// The byte the 8255's port A shows.
    #[must_use]
    pub fn data(&self) -> u8 {
        self.shift.unwrap_or(0)
    }

    /// Whether the keyboard is asking for IRQ 1.
    #[must_use]
    pub const fn irq(&self) -> bool {
        self.shift.is_some()
    }

    /// Port B's keyboard lines changed at `now`.
    pub fn set_lines(&mut self, clock_running: bool, clear: bool, now: Nanos) {
        if clear && !self.clear_held {
            self.shift = None;
            self.next_send = self.next_send.max(now + BYTE_TIME);
        }
        self.clear_held = clear;
        match (clock_running, self.clock_low_since) {
            (false, None) => self.clock_low_since = Some(now),
            (true, Some(since)) => {
                self.clock_low_since = None;
                if now.saturating_sub(since) >= RESET_HOLD {
                    self.len = 0;
                    self.shift = None;
                    self.repeating = None;
                    self.held = [0; 4];
                    self.num_lock = false;
                    self.enqueue(SELF_TEST_PASSED);
                    self.next_send = now + SELF_TEST_TIME;
                }
            }
            _ => {}
        }
    }

    /// Delivers everything due by `now`, in order; returns whether a byte
    /// reached the shift register, raising IRQ 1.
    pub fn advance(&mut self, now: Nanos) -> bool {
        let mut arrived = false;
        while let Some(at) = self.next_event().filter(|&at| at <= now) {
            if let Some((key, due)) = self.repeating.filter(|&(_, due)| due == at) {
                self.repeat(key, due, now);
            } else {
                self.shift = Some(self.buffer[self.head]);
                self.head = (self.head + 1) % BUFFER;
                self.len -= 1;
                self.next_send = at + BYTE_TIME;
                arrived = true;
            }
        }
        arrived
    }

    /// The typematic repeat due at `due`. Once the buffer is full every
    /// further repeat up to `now` only overruns, so they are taken at once.
    fn repeat(&mut self, key: Scan, due: Nanos, now: Nanos) {
        let mut next = due + TYPEMATIC_INTERVAL;
        if self.len == BUFFER {
            next += (now - due) / TYPEMATIC_INTERVAL * TYPEMATIC_INTERVAL;
        }
        let mut sequence = Sequence::new();
        sequence.code(key, false);
        self.send(&sequence);
        self.repeating = Some((key, next));
    }

    /// When the keyboard next needs to act, if ever.
    #[must_use]
    pub fn next_event(&self) -> Option<Nanos> {
        let send = (self.len > 0
            && self.shift.is_none()
            && !self.clear_held
            && self.clock_low_since.is_none())
        .then_some(self.next_send);
        let repeat = self.repeating.map(|(_, at)| at);
        match (send, repeat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Takes every byte the keyboard sends within 400 ms of `now`.
    fn drain(keyboard: &mut Keyboard, mut now: Nanos) -> alloc::vec::Vec<u8> {
        let horizon = now + 400_000_000;
        let mut out = alloc::vec::Vec::new();
        while let Some(at) = keyboard.next_event() {
            if at > horizon {
                break;
            }
            now = now.max(at);
            if keyboard.advance(now) {
                out.push(keyboard.data());
                keyboard.set_lines(true, true, now);
                keyboard.set_lines(true, false, now);
            }
        }
        out
    }

    #[test]
    fn a_keystroke_sends_make_then_break() {
        let mut kb = Keyboard::new();
        kb.press(Scan::plain(0x1E), 0);
        kb.release(Scan::plain(0x1E));
        assert_eq!(drain(&mut kb, 0), [0x1E, 0x9E]);
    }

    #[test]
    fn enhanced_keys_carry_their_prefix() {
        let mut kb = Keyboard::new();
        kb.press(Scan::extended(0x48), 0);
        kb.release(Scan::extended(0x48));
        assert_eq!(drain(&mut kb, 0), [0xE0, 0x48, 0xE0, 0xC8]);
    }

    #[test]
    fn a_held_key_repeats_at_the_keyboards_rate() {
        let mut kb = Keyboard::new();
        kb.press(Scan::plain(0x10), 0);
        let mut bytes = alloc::vec::Vec::new();
        let mut now = 0;
        while now < 1_000_000_000 {
            if kb.advance(now) {
                bytes.push((now, kb.data()));
                kb.set_lines(true, true, now);
                kb.set_lines(true, false, now);
            }
            now += 1_000_000;
        }
        let repeats: alloc::vec::Vec<_> = bytes.iter().filter(|(_, b)| *b == 0x10).collect();
        assert_eq!(
            repeats.len(),
            7,
            "one make, then repeats from 500 ms at 10.9 a second"
        );
        assert!(repeats[1].0 >= TYPEMATIC_DELAY);
    }

    #[test]
    fn holding_the_clock_low_runs_the_self_test() {
        let mut kb = Keyboard::new();
        kb.press(Scan::plain(0x02), 0);
        kb.set_lines(false, false, 0);
        kb.set_lines(true, false, RESET_HOLD);
        assert_eq!(drain(&mut kb, RESET_HOLD), [SELF_TEST_PASSED]);
    }

    /// What tapping `key` sends with `held` down round it, the held keys'
    /// own codes left out.
    fn tapped(kb: &mut Keyboard, held: &[Scan], key: Scan) -> alloc::vec::Vec<u8> {
        for &h in held {
            kb.press(h, 0);
        }
        let before = drain(kb, 0).len();
        kb.press(key, 0);
        kb.release(key);
        let sent = drain(kb, 0);
        for &h in held {
            kb.release(h);
        }
        drain(kb, 0);
        let makes: usize = held.iter().map(|h| 1 + usize::from(h.extended)).sum();
        assert_eq!(before, makes, "the held keys' own makes");
        sent
    }

    const UP: Scan = Scan::extended(0x48);

    #[test]
    fn a_grey_key_is_shifted_out_of_the_keypad_under_num_lock() {
        let mut kb = Keyboard::new();
        assert_eq!(tapped(&mut kb, &[], UP), [0xE0, 0x48, 0xE0, 0xC8]);
        kb.press(NUM_LOCK, 0);
        kb.release(NUM_LOCK);
        drain(&mut kb, 0);
        assert_eq!(
            tapped(&mut kb, &[], UP),
            [0xE0, 0x2A, 0xE0, 0x48, 0xE0, 0xC8, 0xE0, 0xAA]
        );
        assert_eq!(
            tapped(&mut kb, &[SHIFT], UP),
            [0xE0, 0x48, 0xE0, 0xC8],
            "Shift already takes it out"
        );
    }

    #[test]
    fn a_held_shift_is_let_go_of_round_a_grey_key() {
        let mut kb = Keyboard::new();
        assert_eq!(
            tapped(&mut kb, &[SHIFT], UP),
            [0xE0, 0xAA, 0xE0, 0x48, 0xE0, 0xC8, 0xE0, 0x2A]
        );
        assert_eq!(
            tapped(&mut kb, &[SHIFT, RIGHT_SHIFT], Scan::extended(0x53)),
            [0xE0, 0xAA, 0xE0, 0xB6, 0xE0, 0x53, 0xE0, 0xD3, 0xE0, 0x36, 0xE0, 0x2A]
        );
        assert_eq!(
            tapped(&mut kb, &[RIGHT_SHIFT], Scan::extended(0x35)),
            [0xE0, 0xB6, 0xE0, 0x35, 0xE0, 0xB5, 0xE0, 0x36],
            "the grey slash"
        );
        assert_eq!(
            tapped(&mut kb, &[SHIFT], Scan::extended(0x1C)),
            [0xE0, 0x1C, 0xE0, 0x9C],
            "the keypad's Enter has no keypad twin"
        );
    }

    #[test]
    fn print_screen_and_pause_send_what_an_xt_bios_reads() {
        let mut kb = Keyboard::new();
        assert_eq!(
            tapped(&mut kb, &[], PRINT_SCREEN),
            [0xE0, 0x2A, 0xE0, 0x37, 0xE0, 0xB7, 0xE0, 0xAA]
        );
        assert_eq!(
            tapped(&mut kb, &[Scan::extended(0x1D)], PRINT_SCREEN),
            [0xE0, 0x37, 0xE0, 0xB7]
        );
        assert_eq!(
            tapped(&mut kb, &[Scan::plain(0x38)], PRINT_SCREEN),
            [0x54, 0xD4],
            "System Request"
        );
        assert_eq!(
            tapped(&mut kb, &[], PAUSE),
            [0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5],
            "and nothing as it comes up"
        );
        assert_eq!(
            tapped(&mut kb, &[Scan::plain(0x1D)], PAUSE),
            [0xE0, 0x46, 0xE0, 0xC6],
            "Break"
        );
        kb.press(PAUSE, 0);
        assert_eq!(drain(&mut kb, 0).len(), 6);
        assert_eq!(kb.next_event(), None, "Pause never repeats");
    }

    #[test]
    fn num_lock_under_ctrl_leaves_the_keyboards_count() {
        let mut kb = Keyboard::new();
        assert_eq!(
            tapped(&mut kb, &[Scan::plain(0x1D)], NUM_LOCK),
            [0x45, 0xC5]
        );
        assert_eq!(tapped(&mut kb, &[], UP), [0xE0, 0x48, 0xE0, 0xC8]);
        kb.press(NUM_LOCK, 0);
        kb.press(NUM_LOCK, 0);
        kb.release(NUM_LOCK);
        drain(&mut kb, 0);
        assert_eq!(
            tapped(&mut kb, &[], UP).len(),
            8,
            "a press held down counts once"
        );
        kb.set_lines(false, false, 0);
        kb.set_lines(true, false, RESET_HOLD);
        drain(&mut kb, RESET_HOLD);
        assert_eq!(
            tapped(&mut kb, &[], UP),
            [0xE0, 0x48, 0xE0, 0xC8],
            "a reset starts it off"
        );
    }

    #[test]
    fn a_key_fits_only_where_its_whole_sequence_does() {
        let mut kb = Keyboard::new();
        kb.press(NUM_LOCK, 0);
        kb.release(NUM_LOCK);
        drain(&mut kb, 0);
        for code in 0x10..0x10 + u8::try_from(BUFFER - 3).unwrap() {
            kb.press(Scan::plain(code), 0);
        }
        assert_eq!(kb.room(), 3);
        assert!(!kb.fits(UP, true), "four bytes with its fake shift");
        assert!(kb.fits(Scan::plain(0x30), true));
        assert!(kb.fits(UP, false), "a key not down sends nothing");
        assert!(!kb.fits(PAUSE, true));
    }

    #[test]
    fn the_held_keys_are_listed_lowest_first() {
        let mut kb = Keyboard::new();
        for key in [UP, Scan::plain(0x1E), SHIFT] {
            kb.press(key, 0);
        }
        assert_eq!(
            kb.held_keys().collect::<alloc::vec::Vec<_>>(),
            [Scan::plain(0x1E), SHIFT, UP]
        );
    }

    #[test]
    fn every_host_key_is_its_own_key_but_the_ones_beside_enter() {
        let mut seen = alloc::collections::BTreeMap::new();
        for usage in 0..=0xFFFF {
            let Some(key) = PhysicalKey::from_usage(usage) else {
                continue;
            };
            if let Some(scan) = scan_of(key) {
                assert!(scan.code != 0 && scan.code < 0x80);
                if let Some(other) = seen.insert((scan.code, scan.extended), usage) {
                    assert_eq!((other, usage), (0x31, 0x32), "{scan:?}");
                }
            }
        }
        assert_eq!(seen.len(), 102, "the international keyboard's keys");
        for usage in [0x65, 0xE3, 0xE7, 0x67, 0x87] {
            let key = PhysicalKey::from_usage(usage).unwrap();
            assert_eq!(scan_of(key), None, "usage {usage:#x}");
        }
        assert_eq!(scan_of(PhysicalKey::A), Some(Scan::plain(0x1E)));
        assert_eq!(scan_of(PhysicalKey::NUM_LOCK), Some(NUM_LOCK));
        assert_eq!(scan_of(PhysicalKey::RIGHT_CTRL), Some(Scan::extended(0x1D)));
        for byte in b"azAZ09 ".iter().copied() {
            let (from_ascii, _) = us_key(byte).unwrap();
            assert!(seen.contains_key(&(from_ascii.code, false)), "{byte}");
        }
    }

    #[test]
    fn an_overflowing_buffer_reports_overrun() {
        let mut kb = Keyboard::new();
        for code in 1..=20 {
            kb.press(Scan::plain(code), 0);
        }
        let bytes = drain(&mut kb, 0);
        assert_eq!(bytes.len(), BUFFER);
        assert_eq!(bytes.last(), Some(&OVERRUN));
    }
}

crate::snapshot::state!(Scan { code, extended });

crate::snapshot::state!(
    Keyboard {
        buffer,
        head,
        len,
        shift,
        next_send,
        clear_held,
        clock_low_since,
        held,
        repeating,
        num_lock,
    }
    check |keyboard: &Keyboard| keyboard.head < BUFFER && keyboard.len <= BUFFER
);

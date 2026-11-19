//! The host's keyboard as a machine's: each host key placed on the machine's
//! key in its position, or typing the character it produces, for each
//! model's keyboard; and the numeric keypad as a C64's joystick.

use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode, PhysicalKey};

use crate::machine::c64::joystick;
use crate::machine::c64::keyboard::{Key as C64Key, Keycap};
use crate::machine::{bbc, electron, LetterCase};

/// How host keys reach a machine's keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mapping {
    /// A symbol presses the machine's key and SHIFT that type it; a letter
    /// presses its own key, its case what the machine's CAPS LOCK and the
    /// host's Shift make of it.
    #[default]
    Logical,
    /// Each host key is the machine's key in its place, for games.
    Physical,
}

/// A change a machine's keyboard is to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press<K> {
    /// A key goes down or up.
    Key(K, bool),
    /// BREAK goes down or up.
    Break(bool),
}

/// What a host key's press does, undone by its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held<K> {
    /// A machine key, with another held beneath it.
    Key(K, Option<K>),
    /// BREAK.
    Break,
    /// A host Shift, which the machine's SHIFT follows unless a held symbol
    /// needs it otherwise.
    Shift,
}

/// A machine's keyboard, as the host's keys reach it.
pub trait MachineKey: Copy + Eq + core::fmt::Debug {
    /// Its SHIFT.
    const SHIFT: Self;
    /// What the host's Ctrl presses.
    const CTRL: Self;
    /// What the host's Caps Lock presses by character, and the SHIFT that
    /// takes.
    const CAPS: (Self, Option<bool>);
    /// What the host's Alt presses, either way.
    const ALT: Option<Self>;

    /// Its number, below [`KEY_NUMBERS`].
    fn number(self) -> u8;

    /// The key in `physical`'s place, a host Shift, Ctrl or Alt aside.
    fn placed(physical: PhysicalKey) -> Option<Held<Self>>;

    /// What a named key types, and the SHIFT that takes.
    fn named(named: NamedKeyCode) -> Option<(Held<Self>, Option<bool>)>;

    /// The key that types `ch` and whether SHIFT is held with it, CAPS LOCK
    /// on.
    fn typing(ch: char) -> Option<(Self, bool)>;
}

/// One past the highest key usage.
const USAGES: usize = 0xE8;
/// One past the highest key number either model has.
pub const KEY_NUMBERS: usize = 0x80;

/// The host's keys as a machine's, by one [`Mapping`].
#[derive(Clone, Debug)]
pub struct Keyboard<K> {
    mapping: Mapping,
    /// What each host key down did, by its usage.
    held: [Option<Held<K>>; USAGES],
    /// How many host keys hold each machine key down, so two host keys on one
    /// machine key let it go only when both are up.
    holding: [u8; KEY_NUMBERS],
    /// Host Shift keys down.
    shifts: u8,
    /// The SHIFT the symbol last pressed needs, and its host key's usage.
    forced: Option<(u16, bool)>,
    /// Whether the machine's SHIFT is down.
    shift: bool,
}

impl<K: MachineKey> Keyboard<K> {
    /// A keyboard with no key down, mapped by `mapping`.
    #[must_use]
    pub const fn new(mapping: Mapping) -> Self {
        Self {
            mapping,
            held: [None; USAGES],
            holding: [0; KEY_NUMBERS],
            shifts: 0,
            forced: None,
            shift: false,
        }
    }

    /// How host keys are mapped.
    #[must_use]
    pub const fn mapping(&self) -> Mapping {
        self.mapping
    }

    /// Map by `mapping` from now on, every key let go first.
    pub fn set_mapping(&mut self, mapping: Mapping, out: &mut Vec<Press<K>>) {
        if mapping != self.mapping {
            self.release_all(out);
            self.mapping = mapping;
        }
    }

    /// The machine's changes for one host key edge, pushed onto `out`:
    /// nothing for a key that maps to none, or an edge that is no change.
    pub fn edge(&mut self, input: &KeyInput, out: &mut Vec<Press<K>>) {
        let (physical, pressed) = match *input {
            KeyInput::Pressed { physical, .. } => (physical, true),
            KeyInput::Released { physical, .. } => (physical, false),
        };
        let usage = physical.usage();
        let Some(slot) = self.held.get(usize::from(usage)).copied() else {
            return;
        };
        if !pressed {
            if let Some(held) = slot {
                self.let_go(usage, held, out);
                self.settle_shift(out);
            }
            return;
        }
        if slot.is_some() {
            return;
        }
        let target = match self.mapping {
            Mapping::Physical => common(physical)
                .or_else(|| K::placed(physical))
                .map(|held| (held, None)),
            Mapping::Logical => typed(physical, input.key()),
        };
        let Some((held, needs)) = target else {
            return;
        };
        if let Some(entry) = self.held.get_mut(usize::from(usage)) {
            *entry = Some(held);
        }
        match held {
            Held::Shift => {
                self.shifts += 1;
                self.settle_shift(out);
            }
            Held::Break => out.push(Press::Break(true)),
            Held::Key(key, with) => {
                self.forced = needs.map(|shift| (usage, shift));
                self.settle_shift(out);
                if let Some(with) = with {
                    self.hold(with, out);
                }
                self.hold(key, out);
            }
        }
    }

    /// Every key let go: what the keyboard is left with when the window
    /// loses the focus.
    pub fn release_all(&mut self, out: &mut Vec<Press<K>>) {
        self.forced = None;
        for (usage, held) in (0u16..).zip(self.held) {
            if let Some(held) = held {
                self.let_go(usage, held, out);
            }
        }
        self.settle_shift(out);
    }

    /// `held`, by the host key `usage`, undone, the machine's SHIFT left for
    /// the caller to settle.
    fn let_go(&mut self, usage: u16, held: Held<K>, out: &mut Vec<Press<K>>) {
        if let Some(entry) = self.held.get_mut(usize::from(usage)) {
            *entry = None;
        }
        match held {
            Held::Shift => self.shifts = self.shifts.saturating_sub(1),
            Held::Break => out.push(Press::Break(false)),
            Held::Key(key, with) => {
                self.release(key, out);
                if let Some(with) = with {
                    self.release(with, out);
                }
                if self.forced.is_some_and(|(by, _)| by == usage) {
                    self.forced = None;
                }
            }
        }
    }

    fn hold(&mut self, key: K, out: &mut Vec<Press<K>>) {
        let count = &mut self.holding[usize::from(key.number()) % KEY_NUMBERS];
        *count = count.saturating_add(1);
        if *count == 1 {
            out.push(Press::Key(key, true));
        }
    }

    fn release(&mut self, key: K, out: &mut Vec<Press<K>>) {
        let count = &mut self.holding[usize::from(key.number()) % KEY_NUMBERS];
        *count = count.saturating_sub(1);
        if *count == 0 {
            out.push(Press::Key(key, false));
        }
    }

    /// The machine's SHIFT brought to what the held keys need of it.
    fn settle_shift(&mut self, out: &mut Vec<Press<K>>) {
        let wanted = self.forced.map_or(self.shifts > 0, |(_, shift)| shift);
        if wanted != self.shift {
            self.shift = wanted;
            out.push(Press::Key(K::SHIFT, wanted));
        }
    }
}

/// The key that types `ch` on a machine whose letter keys type as `case`
/// says, and whether SHIFT is held with it: what pasted text is typed with, a
/// line ending as RETURN and `£` as its code.
#[must_use]
pub fn typing<K: MachineKey>(ch: char, case: LetterCase) -> Option<(K, bool)> {
    match ch {
        '\n' => K::typing('\r'),
        '£' => K::typing('`'),
        ch if ch.is_ascii_alphabetic() => {
            let (key, _) = K::typing(ch.to_ascii_uppercase())?;
            let shifted = match case {
                LetterCase::Small => ch.is_ascii_uppercase(),
                LetterCase::Capitals => ch.is_ascii_lowercase(),
                LetterCase::CapitalsOnly => false,
            };
            Some((key, shifted))
        }
        ch => K::typing(ch),
    }
}

const fn is_shift(physical: PhysicalKey) -> bool {
    physical.usage() == PhysicalKey::LEFT_SHIFT.usage()
        || physical.usage() == PhysicalKey::RIGHT_SHIFT.usage()
}

const fn is_ctrl(physical: PhysicalKey) -> bool {
    physical.usage() == PhysicalKey::LEFT_CTRL.usage()
        || physical.usage() == PhysicalKey::RIGHT_CTRL.usage()
}

const fn is_alt(physical: PhysicalKey) -> bool {
    physical.usage() == PhysicalKey::LEFT_ALT.usage()
        || physical.usage() == PhysicalKey::RIGHT_ALT.usage()
}

/// What the host's Shift, Ctrl and Alt are to either mapping.
fn common<K: MachineKey>(physical: PhysicalKey) -> Option<Held<K>> {
    if is_shift(physical) {
        Some(Held::Shift)
    } else if is_ctrl(physical) {
        Some(Held::Key(K::CTRL, None))
    } else if is_alt(physical) {
        K::ALT.map(|alt| Held::Key(alt, None))
    } else {
        None
    }
}

/// What `physical` pressed, typing `key`, does by the character it typed,
/// and the SHIFT that needs: none for a letter or a named key.
fn typed<K: MachineKey>(
    physical: PhysicalKey,
    key: Option<KeyValue>,
) -> Option<(Held<K>, Option<bool>)> {
    if let Some(held) = common(physical) {
        return Some((held, None));
    }
    if physical.usage() == PhysicalKey::CAPS_LOCK.usage() {
        let (caps, needs) = K::CAPS;
        return Some((Held::Key(caps, None), needs));
    }
    match key? {
        KeyValue::Named(named) => K::named(named),
        KeyValue::Char(ch) if ch.is_ascii_alphabetic() => {
            let (letter, _) = K::typing(ch.to_ascii_uppercase())?;
            Some((Held::Key(letter, None), None))
        }
        KeyValue::Char(ch) => {
            let (key, shifted) = K::typing(if ch == '£' { '`' } else { ch })?;
            Some((Held::Key(key, None), Some(shifted)))
        }
    }
}

/// The host's letters and digits in place: what the key at `usage` types on
/// a UK keyboard.
fn letter_or_digit(usage: u16) -> Option<char> {
    let from = |first: u16, base: u8| {
        u8::try_from(usage.checked_sub(first)?)
            .ok()
            .map(|at| char::from(base + at))
    };
    match usage {
        0x04..=0x1D => from(0x04, b'A'),
        0x1E..=0x26 => from(0x1E, b'1'),
        0x27 => Some('0'),
        _ => None,
    }
}

/// Host function key `n`, F1 being 1, as a machine's tenth, F10 being 0.
fn function_number(named: NamedKeyCode) -> Option<usize> {
    let n = usize::from(named.code().checked_sub(NamedKeyCode::F1.code())?) + 1;
    match n {
        1..=9 => Some(n),
        10 => Some(0),
        _ => None,
    }
}

impl MachineKey for bbc::keyboard::Key {
    const SHIFT: Self = Self::SHIFT;
    const CTRL: Self = Self::CTRL;
    const CAPS: (Self, Option<bool>) = (Self::CAPS_LOCK, None);
    const ALT: Option<Self> = None;

    fn number(self) -> u8 {
        Self::number(self) & 0x7F
    }

    /// The key in `physical`'s place on a UK keyboard; keys the BBC has and
    /// the PC's main block does not sit on its editing keys.
    fn placed(physical: PhysicalKey) -> Option<Held<Self>> {
        let usage = physical.usage();
        if let Some(ch) = letter_or_digit(usage) {
            return Some(Held::Key(Self::typing(ch)?.0, None));
        }
        let key = match usage {
            0x28 => Self::RETURN,
            0x29 => Self::ESCAPE,
            0x2A | 0x4C => Self::DELETE,
            0x2B => Self::TAB,
            0x2C => Self::SPACE,
            0x2D => Self::MINUS,
            0x2E => Self::CARET,
            0x2F => Self::AT,
            0x30 => Self::LEFT_BRACKET,
            0x31 | 0x32 => Self::RIGHT_BRACKET,
            0x33 => Self::SEMICOLON,
            0x34 => Self::COLON,
            0x35 => Self::UNDERSCORE,
            0x36 => Self::COMMA,
            0x37 => Self::FULL_STOP,
            0x38 => Self::SLASH,
            0x39 => Self::CAPS_LOCK,
            0x3A..=0x42 => bbc_function(usize::from(usage - 0x3A) + 1)?,
            0x43 => Self::F0,
            0x45 | 0x48 => return Some(Held::Break),
            0x49 | 0x64 => Self::BACKSLASH,
            0x4A => Self::SHIFT_LOCK,
            0x4D => Self::COPY,
            0x4F => Self::RIGHT,
            0x50 => Self::LEFT,
            0x51 => Self::DOWN,
            0x52 => Self::UP,
            _ => return None,
        };
        Some(Held::Key(key, None))
    }

    fn named(named: NamedKeyCode) -> Option<(Held<Self>, Option<bool>)> {
        let key = match named {
            NamedKeyCode::Enter => Self::RETURN,
            NamedKeyCode::Escape => Self::ESCAPE,
            NamedKeyCode::Backspace | NamedKeyCode::Delete => Self::DELETE,
            NamedKeyCode::Tab => Self::TAB,
            NamedKeyCode::End => Self::COPY,
            NamedKeyCode::Left => Self::LEFT,
            NamedKeyCode::Right => Self::RIGHT,
            NamedKeyCode::Up => Self::UP,
            NamedKeyCode::Down => Self::DOWN,
            NamedKeyCode::F12 => return Some((Held::Break, None)),
            named => bbc_function(function_number(named)?)?,
        };
        Some((Held::Key(key, None), None))
    }

    fn typing(ch: char) -> Option<(Self, bool)> {
        Self::typing(ch)
    }
}

/// The Model B's function key `number`, f0 to f9.
fn bbc_function(number: usize) -> Option<bbc::keyboard::Key> {
    use bbc::keyboard::Key;
    const FUNCTIONS: [Key; 10] = [
        Key::F0,
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
    ];
    FUNCTIONS.get(number).copied()
}

/// The Electron's digit `number`, which with FUNC is soft key `number`.
fn electron_digit(number: usize) -> Option<electron::keyboard::Key> {
    let digit = u8::try_from(number).ok().filter(|&n| n <= 9)?;
    Some(electron::keyboard::Key::typing(char::from(b'0' + digit))?.0)
}

impl MachineKey for electron::keyboard::Key {
    const SHIFT: Self = Self::SHIFT;
    const CTRL: Self = Self::CTRL;
    // CAPS LK/FUNC toggles CAPS LOCK with SHIFT.
    const CAPS: (Self, Option<bool>) = (Self::FUNC, Some(true));
    const ALT: Option<Self> = Some(Self::FUNC);

    fn number(self) -> u8 {
        Self::number(self)
    }

    /// The key in `physical`'s place; the cursor keys and COPY, which the
    /// PC's main block lacks, on its arrows and End, and CAPS LK/FUNC on Caps
    /// Lock.
    fn placed(physical: PhysicalKey) -> Option<Held<Self>> {
        let usage = physical.usage();
        if let Some(ch) = letter_or_digit(usage) {
            return Some(Held::Key(Self::typing(ch)?.0, None));
        }
        let key = match usage {
            0x28 => Self::RETURN,
            0x29 => Self::ESCAPE,
            0x2A | 0x4C => Self::DELETE,
            0x2C => Self::SPACE,
            0x2D => Self::MINUS,
            0x33 => Self::SEMICOLON,
            0x34 => Self::COLON,
            0x36 => Self::COMMA,
            0x37 => Self::FULL_STOP,
            0x38 => Self::SLASH,
            0x39 => Self::FUNC,
            0x45 | 0x48 => return Some(Held::Break),
            0x4D => Self::COPY,
            0x4F => Self::RIGHT,
            0x50 => Self::LEFT,
            0x51 => Self::DOWN,
            0x52 => Self::UP,
            _ => return None,
        };
        Some(Held::Key(key, None))
    }

    /// A function key is FUNC with its digit, F10 with 0.
    fn named(named: NamedKeyCode) -> Option<(Held<Self>, Option<bool>)> {
        let key = match named {
            NamedKeyCode::Enter => Self::RETURN,
            NamedKeyCode::Escape => Self::ESCAPE,
            NamedKeyCode::Backspace | NamedKeyCode::Delete => Self::DELETE,
            NamedKeyCode::End => Self::COPY,
            NamedKeyCode::Left => Self::LEFT,
            NamedKeyCode::Right => Self::RIGHT,
            NamedKeyCode::Up => Self::UP,
            NamedKeyCode::Down => Self::DOWN,
            NamedKeyCode::F12 => return Some((Held::Break, None)),
            named => {
                let digit = electron_digit(function_number(named)?)?;
                return Some((Held::Key(digit, Some(Self::FUNC)), None));
            }
        };
        Some((Held::Key(key, None), None))
    }

    fn typing(ch: char) -> Option<(Self, bool)> {
        match Self::typing(ch)? {
            (key, electron::keyboard::Modifier::None) => Some((key, false)),
            (key, electron::keyboard::Modifier::Shift) => Some((key, true)),
            (_, electron::keyboard::Modifier::Ctrl) => None,
        }
    }
}

impl MachineKey for Keycap {
    const SHIFT: Self = Self::Matrix(C64Key::LEFT_SHIFT);
    const CTRL: Self = Self::Matrix(C64Key::CONTROL);
    // The C64 has no CAPS LOCK; the host's, like its Alt, is the Commodore
    // key.
    const CAPS: (Self, Option<bool>) = (Self::Matrix(C64Key::COMMODORE), None);
    const ALT: Option<Self> = Some(Self::Matrix(C64Key::COMMODORE));

    fn number(self) -> u8 {
        Self::number(self)
    }

    /// The key in `physical`'s place on a UK keyboard: RUN/STOP on Caps Lock
    /// and Escape, the left arrow left of 1, CLR/HOME and the pound sign on
    /// Home and Insert, RESTORE on F12, Pause and Page Up, and the cursor
    /// keys and the second function of each function key as SHIFT gives
    /// them.
    fn placed(physical: PhysicalKey) -> Option<Held<Self>> {
        let usage = physical.usage();
        let shifted = |key| Some(Held::Key(Self::Matrix(key), Some(C64_SHIFTED)));
        if let Some(ch) = letter_or_digit(usage) {
            return Some(Held::Key(Self::Matrix(C64Key::typing(ch)?.0), None));
        }
        let key = match usage {
            0x28 => C64Key::RETURN,
            0x29 | 0x39 => C64Key::RUN_STOP,
            0x2A | 0x4C => C64Key::DELETE,
            0x2B => C64Key::CONTROL,
            0x2C => C64Key::SPACE,
            0x2D => C64Key::PLUS,
            0x2E => C64Key::MINUS,
            0x2F => C64Key::AT,
            0x30 => C64Key::ASTERISK,
            0x31 => C64Key::UP_ARROW,
            0x32 => C64Key::EQUALS,
            0x33 => C64Key::COLON,
            0x34 => C64Key::SEMICOLON,
            0x35 => C64Key::LEFT_ARROW,
            0x36 => C64Key::COMMA,
            0x37 => C64Key::PERIOD,
            0x38 => C64Key::SLASH,
            0x3A..=0x41 => {
                let (key, shift) = c64_function(usize::from(usage - 0x3A) + 1)?;
                return if shift {
                    shifted(key)
                } else {
                    Some(Held::Key(Self::Matrix(key), None))
                };
            }
            0x45 | 0x48 | 0x4B => return Some(Held::Key(Self::Restore, None)),
            0x49 => C64Key::POUND,
            0x4A => C64Key::HOME,
            0x4F => C64Key::CURSOR_RIGHT,
            0x50 => return shifted(C64Key::CURSOR_RIGHT),
            0x51 => C64Key::CURSOR_DOWN,
            0x52 => return shifted(C64Key::CURSOR_DOWN),
            _ => return None,
        };
        Some(Held::Key(Self::Matrix(key), None))
    }

    /// The cursor keys, Insert and the even function keys as SHIFT gives
    /// them on the C64's own keys; RESTORE on F12 and Page Up.
    fn named(named: NamedKeyCode) -> Option<(Held<Self>, Option<bool>)> {
        let (key, shift) = match named {
            NamedKeyCode::Enter => (C64Key::RETURN, None),
            NamedKeyCode::Escape => (C64Key::RUN_STOP, None),
            NamedKeyCode::Backspace | NamedKeyCode::Delete => (C64Key::DELETE, Some(false)),
            NamedKeyCode::Insert => (C64Key::DELETE, Some(true)),
            NamedKeyCode::Tab => (C64Key::CONTROL, None),
            NamedKeyCode::Home => (C64Key::HOME, None),
            NamedKeyCode::Right => (C64Key::CURSOR_RIGHT, Some(false)),
            NamedKeyCode::Left => (C64Key::CURSOR_RIGHT, Some(true)),
            NamedKeyCode::Down => (C64Key::CURSOR_DOWN, Some(false)),
            NamedKeyCode::Up => (C64Key::CURSOR_DOWN, Some(true)),
            NamedKeyCode::F12 | NamedKeyCode::PageUp => {
                return Some((Held::Key(Self::Restore, None), None));
            }
            named => {
                let (key, shift) = c64_function(function_number(named)?)?;
                (key, Some(shift))
            }
        };
        Some((Held::Key(Self::Matrix(key), None), shift))
    }

    fn typing(ch: char) -> Option<(Self, bool)> {
        C64Key::typing(ch).map(|(key, shifted)| (Self::Matrix(key), shifted))
    }
}

/// The SHIFT a placed key holds beneath it for its key's second function:
/// the right one, so the host's Shift keys, on the left one, are not undone.
const C64_SHIFTED: Keycap = Keycap::Matrix(C64Key::RIGHT_SHIFT);

/// The C64's key for host function key `number`, F1 being 1, and whether
/// SHIFT goes with it: f1, f3, f5 and f7 the keys themselves, the even ones
/// SHIFT with the odd before.
fn c64_function(number: usize) -> Option<(C64Key, bool)> {
    const KEYS: [C64Key; 4] = [C64Key::F1, C64Key::F3, C64Key::F5, C64Key::F7];
    let at = number.checked_sub(1)?;
    Some((*KEYS.get(at / 2)?, at % 2 == 1))
}

/// Keypad keys, by their usages from keypad 1: each a joystick's lines.
const STICK: [(u16, u8); 10] = [
    (0x59, joystick::DOWN | joystick::LEFT),
    (0x5A, joystick::DOWN),
    (0x5B, joystick::DOWN | joystick::RIGHT),
    (0x5C, joystick::LEFT),
    (0x5D, joystick::FIRE),
    (0x5E, joystick::RIGHT),
    (0x5F, joystick::UP | joystick::LEFT),
    (0x60, joystick::UP),
    (0x61, joystick::UP | joystick::RIGHT),
    (0x62, joystick::FIRE),
];

/// The host's numeric keypad as a joystick: its arrows and their diagonals
/// the stick, 5 and 0 the fire button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stick {
    /// The keypad keys held, a bit each in [`STICK`]'s order.
    held: u16,
}

impl Stick {
    /// A stick with nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self { held: 0 }
    }

    /// The lines held after `input`, where it is a keypad key's edge;
    /// `None` for another key, which the keyboard takes.
    pub fn edge(&mut self, input: &KeyInput) -> Option<u8> {
        let (physical, pressed) = match *input {
            KeyInput::Pressed { physical, .. } => (physical, true),
            KeyInput::Released { physical, .. } => (physical, false),
        };
        let at = STICK
            .iter()
            .position(|&(usage, _)| usage == physical.usage())?;
        if pressed {
            self.held |= 1 << at;
        } else {
            self.held &= !(1 << at);
        }
        Some(self.lines())
    }

    /// Every keypad key let go: the lines it held, now none, if any were.
    pub fn release_all(&mut self) -> Option<u8> {
        let held = self.lines() != 0;
        self.held = 0;
        held.then_some(0)
    }

    /// The lines the keys held hold.
    #[must_use]
    pub fn lines(self) -> u8 {
        STICK
            .iter()
            .enumerate()
            .filter(|&(at, _)| self.held & 1 << at != 0)
            .fold(0, |lines, (_, &(_, held))| lines | held)
    }
}

/// A control port's joystick lines.
pub const STICK_LINES: usize = joystick::ALL.count_ones() as usize;

/// When each key and each joystick line last went down and came up, in the
/// machine's cycles, so an edge is put off until the press before it has
/// been held, or the release before it kept, for the least hold: a scan of
/// the keyboard then sees every press and every release, however quickly
/// the host's keys moved.
#[derive(Clone, Debug)]
pub struct Edges {
    keys: [Times; KEY_NUMBERS],
    stick: [u8; 2],
    stick_times: [[Times; STICK_LINES]; 2],
}

/// When a key or a line last went down and last came up.
#[derive(Clone, Copy, Debug, Default)]
struct Times {
    down: Option<u64>,
    up: Option<u64>,
}

impl Times {
    /// The cycle an edge going `down` or up at `at` takes effect at, the
    /// edge before it given `least` cycles; recorded.
    fn edge(&mut self, down: bool, at: u64, least: u64) -> u64 {
        let (before, now) = if down {
            (self.up, &mut self.down)
        } else {
            (self.down, &mut self.up)
        };
        let at = before.map_or(at, |before| at.max(before.saturating_add(least)));
        *now = Some(at);
        at
    }
}

impl Default for Edges {
    fn default() -> Self {
        Self::new()
    }
}

impl Edges {
    /// No edge yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            keys: [Times::default(); KEY_NUMBERS],
            stick: [0; 2],
            stick_times: [[Times::default(); STICK_LINES]; 2],
        }
    }

    /// The cycle key `number` going `down` or up takes effect at: `at` or
    /// later, once the edge before it has lasted `least` cycles.
    pub fn key(&mut self, number: usize, down: bool, at: u64, least: u64) -> u64 {
        self.keys[number % KEY_NUMBERS].edge(down, at, least)
    }

    /// The cycle `port`'s lines become `lines` at: `at` or later, once each
    /// line that changes has kept its state `least` cycles. A port past the
    /// second changes nothing.
    pub fn stick(&mut self, port: usize, lines: u8, at: u64, least: u64) -> u64 {
        let (Some(held), Some(times)) = (self.stick.get_mut(port), self.stick_times.get_mut(port))
        else {
            return at;
        };
        let changed = *held ^ lines;
        let lasted = (0..STICK_LINES)
            .filter(|&line| changed & 1 << line != 0)
            .filter_map(|line| {
                let times = times[line];
                if lines & 1 << line != 0 {
                    times.up
                } else {
                    times.down
                }
            })
            .map(|before| before.saturating_add(least))
            .fold(at, u64::max);
        for (line, times) in times.iter_mut().enumerate() {
            if changed & 1 << line != 0 {
                times.edge(lines & 1 << line != 0, lasted, 0);
            }
        }
        *held = lines;
        lasted
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;

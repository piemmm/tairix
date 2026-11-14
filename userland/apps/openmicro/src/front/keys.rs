//! The host's keyboard as a machine's: each host key placed on the machine's
//! key in its position, or typing the character it produces, for either
//! model's keyboard.

use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode, PhysicalKey};

use crate::machine::{bbc, electron};

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

/// The key that types `ch` on a machine whose CAPS LOCK is as `caps_lock`
/// says, and whether SHIFT is held with it: what pasted text is typed with, a
/// line ending as RETURN and `£` as its code.
#[must_use]
pub fn typing<K: MachineKey>(ch: char, caps_lock: bool) -> Option<(K, bool)> {
    match ch {
        '\n' => K::typing('\r'),
        '£' => K::typing('`'),
        ch if ch.is_ascii_alphabetic() => {
            let (key, _) = K::typing(ch.to_ascii_uppercase())?;
            Some((key, ch.is_ascii_lowercase() == caps_lock))
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

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;

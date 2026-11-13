//! The host's keyboard as the BBC's: each host key placed on the BBC key in
//! its position, or typing the character it produces.

use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode, PhysicalKey};

use crate::machine::keyboard::Key;

/// How host keys reach the BBC's keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mapping {
    /// A symbol presses the BBC key and SHIFT that type it; a letter presses
    /// its own key, its case what the machine's CAPS LOCK and the host's
    /// Shift make of it, as on a BBC.
    #[default]
    Logical,
    /// Each host key is the BBC key in its place, for games.
    Physical,
}

/// A change the BBC's keyboard is to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// A key goes down or up.
    Key(Key, bool),
    /// BREAK goes down or up.
    Break(bool),
}

/// What a host key's press did, undone by its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Key(Key),
    Break,
    /// A host Shift, which the BBC's SHIFT follows unless a held symbol
    /// needs it otherwise.
    Shift,
}

/// One past the highest key usage.
const USAGES: usize = 0xE8;
/// One past the highest internal key number.
const KEY_NUMBERS: usize = 0x80;

/// The host's keys as the BBC's, by one [`Mapping`].
#[derive(Clone, Debug)]
pub struct Keyboard {
    mapping: Mapping,
    /// What each host key down did, by its usage.
    held: [Option<Held>; USAGES],
    /// How many host keys hold each BBC key down, so two host keys on one
    /// BBC key let it go only when both are up.
    holding: [u8; KEY_NUMBERS],
    /// Host Shift keys down.
    shifts: u8,
    /// The SHIFT the symbol last pressed needs, and its host key's usage.
    forced: Option<(u16, bool)>,
    /// Whether the BBC's SHIFT is down.
    shift: bool,
}

impl Keyboard {
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
    pub fn set_mapping(&mut self, mapping: Mapping, out: &mut Vec<Press>) {
        if mapping != self.mapping {
            self.release_all(out);
            self.mapping = mapping;
        }
    }

    /// The BBC's changes for one host key edge, pushed onto `out`: nothing
    /// for a key that maps to none, or an edge that is no change.
    pub fn edge(&mut self, input: &KeyInput, out: &mut Vec<Press>) {
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
            Mapping::Physical => placed(physical).map(|held| (held, None)),
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
            Held::Key(key) => {
                self.forced = needs.map(|shift| (usage, shift));
                self.settle_shift(out);
                self.hold(key, out);
            }
        }
    }

    /// Every key let go: what the keyboard is left with when the window
    /// loses the focus.
    pub fn release_all(&mut self, out: &mut Vec<Press>) {
        self.forced = None;
        for (usage, held) in (0u16..).zip(self.held) {
            if let Some(held) = held {
                self.let_go(usage, held, out);
            }
        }
        self.settle_shift(out);
    }

    /// `held`, by the host key `usage`, undone, the BBC's SHIFT left for the
    /// caller to settle.
    fn let_go(&mut self, usage: u16, held: Held, out: &mut Vec<Press>) {
        if let Some(entry) = self.held.get_mut(usize::from(usage)) {
            *entry = None;
        }
        match held {
            Held::Shift => self.shifts = self.shifts.saturating_sub(1),
            Held::Break => out.push(Press::Break(false)),
            Held::Key(key) => {
                let count = &mut self.holding[usize::from(key.number())];
                *count = count.saturating_sub(1);
                if *count == 0 {
                    out.push(Press::Key(key, false));
                }
                if self.forced.is_some_and(|(by, _)| by == usage) {
                    self.forced = None;
                }
            }
        }
    }

    fn hold(&mut self, key: Key, out: &mut Vec<Press>) {
        let count = &mut self.holding[usize::from(key.number())];
        *count = count.saturating_add(1);
        if *count == 1 {
            out.push(Press::Key(key, true));
        }
    }

    /// The BBC's SHIFT brought to what the held keys need of it.
    fn settle_shift(&mut self, out: &mut Vec<Press>) {
        let wanted = self.forced.map_or(self.shifts > 0, |(_, shift)| shift);
        if wanted != self.shift {
            self.shift = wanted;
            out.push(Press::Key(Key::SHIFT, wanted));
        }
    }
}

/// The BBC key that types `ch`, and whether SHIFT is held with it, on a
/// machine whose CAPS LOCK is as `caps_lock` says: what pasted text is typed
/// with, a line ending as RETURN.
#[must_use]
pub fn typing(ch: char, caps_lock: bool) -> Option<(Key, bool)> {
    match ch {
        '\n' => Key::typing('\r'),
        '£' => Key::typing('`'),
        ch if ch.is_ascii_alphabetic() => {
            let (key, _) = Key::typing(ch.to_ascii_uppercase())?;
            Some((key, ch.is_ascii_lowercase() == caps_lock))
        }
        ch => Key::typing(ch),
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

/// The BBC key in `physical`'s place on a UK keyboard; keys the BBC has
/// and the PC's main block does not sit on its editing keys.
fn placed(physical: PhysicalKey) -> Option<Held> {
    if is_shift(physical) {
        return Some(Held::Shift);
    }
    if is_ctrl(physical) {
        return Some(Held::Key(Key::CTRL));
    }
    let usage = physical.usage();
    let from = |first: u16, base: u8| {
        u8::try_from(usage - first)
            .ok()
            .map(|at| char::from(base + at))
    };
    let key = match usage {
        0x04..=0x1D => Key::typing(from(0x04, b'A')?)?.0,
        0x1E..=0x26 => Key::typing(from(0x1E, b'1')?)?.0,
        0x27 => Key::DIGIT_0,
        0x28 => Key::RETURN,
        0x29 => Key::ESCAPE,
        0x2A | 0x4C => Key::DELETE,
        0x2B => Key::TAB,
        0x2C => Key::SPACE,
        0x2D => Key::MINUS,
        0x2E => Key::CARET,
        0x2F => Key::AT,
        0x30 => Key::LEFT_BRACKET,
        0x31 | 0x32 => Key::RIGHT_BRACKET,
        0x33 => Key::SEMICOLON,
        0x34 => Key::COLON,
        0x35 => Key::UNDERSCORE,
        0x36 => Key::COMMA,
        0x37 => Key::FULL_STOP,
        0x38 => Key::SLASH,
        0x39 => Key::CAPS_LOCK,
        0x3A..=0x42 => function(usize::from(usage - 0x3A) + 1)?,
        0x43 => Key::F0,
        0x45 | 0x48 => return Some(Held::Break),
        0x49 | 0x64 => Key::BACKSLASH,
        0x4A => Key::SHIFT_LOCK,
        0x4D => Key::COPY,
        0x4F => Key::RIGHT,
        0x50 => Key::LEFT,
        0x51 => Key::DOWN,
        0x52 => Key::UP,
        _ => return None,
    };
    Some(Held::Key(key))
}

/// What `physical` pressed, typing `key`, does by the character it typed, and
/// the SHIFT that needs: none for a letter or a named key.
fn typed(physical: PhysicalKey, key: Option<KeyValue>) -> Option<(Held, Option<bool>)> {
    if is_shift(physical) {
        return Some((Held::Shift, None));
    }
    if is_ctrl(physical) {
        return Some((Held::Key(Key::CTRL), None));
    }
    if physical.usage() == PhysicalKey::CAPS_LOCK.usage() {
        return Some((Held::Key(Key::CAPS_LOCK), None));
    }
    match key? {
        KeyValue::Named(named) => named_key(named).map(|held| (held, None)),
        KeyValue::Char(ch) if ch.is_ascii_alphabetic() => {
            let (letter, _) = Key::typing(ch.to_ascii_uppercase())?;
            Some((Held::Key(letter), None))
        }
        KeyValue::Char(ch) => {
            let (key, shifted) = Key::typing(if ch == '£' { '`' } else { ch })?;
            Some((Held::Key(key), Some(shifted)))
        }
    }
}

fn named_key(named: NamedKeyCode) -> Option<Held> {
    let key = match named {
        NamedKeyCode::Enter => Key::RETURN,
        NamedKeyCode::Escape => Key::ESCAPE,
        NamedKeyCode::Backspace | NamedKeyCode::Delete => Key::DELETE,
        NamedKeyCode::Tab => Key::TAB,
        NamedKeyCode::End => Key::COPY,
        NamedKeyCode::Left => Key::LEFT,
        NamedKeyCode::Right => Key::RIGHT,
        NamedKeyCode::Up => Key::UP,
        NamedKeyCode::Down => Key::DOWN,
        NamedKeyCode::F1
        | NamedKeyCode::F2
        | NamedKeyCode::F3
        | NamedKeyCode::F4
        | NamedKeyCode::F5
        | NamedKeyCode::F6
        | NamedKeyCode::F7
        | NamedKeyCode::F8
        | NamedKeyCode::F9 => function(usize::from(named.code() - NamedKeyCode::F1.code()) + 1)?,
        NamedKeyCode::F10 => Key::F0,
        NamedKeyCode::F12 => return Some(Held::Break),
        NamedKeyCode::Insert
        | NamedKeyCode::Home
        | NamedKeyCode::PageUp
        | NamedKeyCode::PageDown
        | NamedKeyCode::F11 => return None,
    };
    Some(Held::Key(key))
}

/// Function key `number`, f0 to f9.
fn function(number: usize) -> Option<Key> {
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

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;

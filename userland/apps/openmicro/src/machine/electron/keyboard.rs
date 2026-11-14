//! The Electron's keyboard: fourteen lines of four keys, read through the
//! paged window while ROM 8 or 9 is paged (Electron Advanced User Guide
//! §14.2). BREAK is not in the matrix; it drives the reset line.

/// A key, by its place in the matrix: line × 4 + bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(u8);

macro_rules! keys {
    ($($name:ident = $number:literal, $label:literal;)*) => {
        impl Key {
            $(
                #[doc = concat!("The ", $label, " key.")]
                pub const $name: Self = Self($number);
            )*

            /// Every key on the keyboard.
            pub const ALL: &'static [Self] = &[$(Self::$name),*];

            /// What the key is engraved with.
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self.0 {
                    $($number => $label,)*
                    _ => "",
                }
            }
        }
    };
}

keys! {
    RIGHT = 0, "cursor right";
    COPY = 1, "COPY";
    SPACE = 3, "SPACE";
    LEFT = 4, "cursor left";
    DOWN = 5, "cursor down";
    RETURN = 6, "RETURN";
    DELETE = 7, "DELETE";
    MINUS = 8, "-";
    UP = 9, "cursor up";
    COLON = 10, ":";
    DIGIT_0 = 12, "0";
    P = 13, "P";
    SEMICOLON = 14, ";";
    SLASH = 15, "/";
    DIGIT_9 = 16, "9";
    O = 17, "O";
    L = 18, "L";
    FULL_STOP = 19, ".";
    DIGIT_8 = 20, "8";
    I = 21, "I";
    K = 22, "K";
    COMMA = 23, ",";
    DIGIT_7 = 24, "7";
    U = 25, "U";
    J = 26, "J";
    M = 27, "M";
    DIGIT_6 = 28, "6";
    Y = 29, "Y";
    H = 30, "H";
    N = 31, "N";
    DIGIT_5 = 32, "5";
    T = 33, "T";
    G = 34, "G";
    B = 35, "B";
    DIGIT_4 = 36, "4";
    R = 37, "R";
    F = 38, "F";
    V = 39, "V";
    DIGIT_3 = 40, "3";
    E = 41, "E";
    D = 42, "D";
    C = 43, "C";
    DIGIT_2 = 44, "2";
    W = 45, "W";
    S = 46, "S";
    X = 47, "X";
    DIGIT_1 = 48, "1";
    Q = 49, "Q";
    A = 50, "A";
    Z = 51, "Z";
    ESCAPE = 52, "ESCAPE";
    FUNC = 53, "CAPS LK/FUNC";
    CTRL = 54, "CTRL";
    SHIFT = 55, "SHIFT";
}

/// The lines the matrix has.
pub const LINES: usize = 14;
/// The bits a line's keys read in, down as ones.
pub const LINE_KEYS: u8 = 0x0F;

/// What a key is typed with besides itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    /// Nothing.
    None,
    /// SHIFT.
    Shift,
    /// CTRL.
    Ctrl,
}

impl Key {
    /// The key at matrix place `number`, if the keyboard has one.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Self::ALL.iter().copied().find(|key| key.0 == number)
    }

    /// Its place in the matrix.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Its line, 0–13.
    #[must_use]
    pub const fn line(self) -> u8 {
        self.0 >> 2
    }

    /// Its bit in its line, 0–3.
    #[must_use]
    pub const fn bit(self) -> u8 {
        self.0 & 3
    }

    /// The key that types `ch`, and what it is typed with, with CAPS LOCK on
    /// as the OS starts: printable ASCII and carriage return, `£` as its code
    /// `` ` ``. The cursor keys and COPY type their characters with SHIFT and
    /// CTRL, as the OS translates them.
    #[must_use]
    pub const fn typing(ch: char) -> Option<(Self, Modifier)> {
        use Modifier::{Ctrl, None as Plain, Shift};
        Some(match ch {
            'A'..='Z' => (Self::letter(ch), Plain),
            '0' => (Self::DIGIT_0, Plain),
            '@' => (Self::DIGIT_0, Shift),
            '1' => (Self::DIGIT_1, Plain),
            '!' => (Self::DIGIT_1, Shift),
            '2' => (Self::DIGIT_2, Plain),
            '"' => (Self::DIGIT_2, Shift),
            '3' => (Self::DIGIT_3, Plain),
            '#' => (Self::DIGIT_3, Shift),
            '4' => (Self::DIGIT_4, Plain),
            '$' => (Self::DIGIT_4, Shift),
            '5' => (Self::DIGIT_5, Plain),
            '%' => (Self::DIGIT_5, Shift),
            '6' => (Self::DIGIT_6, Plain),
            '&' => (Self::DIGIT_6, Shift),
            '7' => (Self::DIGIT_7, Plain),
            '\'' => (Self::DIGIT_7, Shift),
            '8' => (Self::DIGIT_8, Plain),
            '(' => (Self::DIGIT_8, Shift),
            '9' => (Self::DIGIT_9, Plain),
            ')' => (Self::DIGIT_9, Shift),
            '-' => (Self::MINUS, Plain),
            '=' => (Self::MINUS, Shift),
            ':' => (Self::COLON, Plain),
            '*' => (Self::COLON, Shift),
            ';' => (Self::SEMICOLON, Plain),
            '+' => (Self::SEMICOLON, Shift),
            ',' => (Self::COMMA, Plain),
            '<' => (Self::COMMA, Shift),
            '.' => (Self::FULL_STOP, Plain),
            '>' => (Self::FULL_STOP, Shift),
            '/' => (Self::SLASH, Plain),
            '?' => (Self::SLASH, Shift),
            '^' => (Self::LEFT, Shift),
            '~' => (Self::LEFT, Ctrl),
            '|' => (Self::RIGHT, Shift),
            '\\' => (Self::RIGHT, Ctrl),
            '`' => (Self::UP, Shift),
            '{' => (Self::UP, Ctrl),
            '_' => (Self::DOWN, Shift),
            '}' => (Self::DOWN, Ctrl),
            '[' => (Self::COPY, Shift),
            ']' => (Self::COPY, Ctrl),
            ' ' => (Self::SPACE, Plain),
            '\r' => (Self::RETURN, Plain),
            _ => return None,
        })
    }

    /// Capital `upper`'s key.
    const fn letter(upper: char) -> Self {
        const LETTERS: [Key; 26] = [
            Key::A,
            Key::B,
            Key::C,
            Key::D,
            Key::E,
            Key::F,
            Key::G,
            Key::H,
            Key::I,
            Key::J,
            Key::K,
            Key::L,
            Key::M,
            Key::N,
            Key::O,
            Key::P,
            Key::Q,
            Key::R,
            Key::S,
            Key::T,
            Key::U,
            Key::V,
            Key::W,
            Key::X,
            Key::Y,
            Key::Z,
        ];
        LETTERS[(upper as u8 - b'A') as usize]
    }
}

/// The switches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyboard {
    /// Bit `bit` of `lines[line]` is set while that key is down.
    lines: [u8; LINES],
}

impl Keyboard {
    /// A keyboard with no key down.
    #[must_use]
    pub const fn new() -> Self {
        Self { lines: [0; LINES] }
    }

    /// Press or release `key`.
    pub fn set(&mut self, key: Key, down: bool) {
        let line = &mut self.lines[usize::from(key.line())];
        let bit = 1 << key.bit();
        if down {
            *line |= bit;
        } else {
            *line &= !bit;
        }
    }

    /// Release every key.
    pub fn release_all(&mut self) {
        self.lines = [0; LINES];
    }

    /// What a read of the paged window at `addr` sees: each line whose
    /// address bit is low, its keys down as ones in bits 0–3, the lines
    /// together.
    #[must_use]
    pub fn read(&self, addr: u16) -> u8 {
        self.lines
            .iter()
            .enumerate()
            .filter(|&(line, _)| addr & (1 << line) == 0)
            .fold(0, |byte, (_, &keys)| byte | keys)
    }
}

crate::snapshot::fields!(
    Keyboard { lines },
    check | keyboard | keyboard.lines.iter().all(|&keys| keys & !LINE_KEYS == 0)
        && keyboard.lines[0] & 0x04 == 0
        && keyboard.lines[2] & 0x08 == 0
);

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;

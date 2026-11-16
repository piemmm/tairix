//! The C64's keyboard: sixty-four keys on an eight-by-eight matrix CIA1
//! scans, its columns on port A and its rows on port B, the joysticks
//! grounding the same lines. RESTORE is wired apart, to NMI.

use crate::snapshot;

/// A key, by where it sits on the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    /// Its column: the port A line that selects it.
    column: u8,
    /// Its row: the port B line it pulls down.
    row: u8,
}

macro_rules! keys {
    ($($name:ident = ($column:literal, $row:literal), $doc:literal;)*) => {
        impl Key {
            $(
                #[doc = $doc]
                pub const $name: Self = Self { column: $column, row: $row };
            )*

            /// Every key, column by column.
            pub const ALL: [Self; 64] = [$(Self::$name),*];
        }
    };
}

keys! {
    DELETE = (0, 0), "INST DEL.";
    RETURN = (0, 1), "RETURN.";
    CURSOR_RIGHT = (0, 2), "CRSR left and right.";
    F7 = (0, 3), "f7 and f8.";
    F1 = (0, 4), "f1 and f2.";
    F3 = (0, 5), "f3 and f4.";
    F5 = (0, 6), "f5 and f6.";
    CURSOR_DOWN = (0, 7), "CRSR up and down.";
    DIGIT_3 = (1, 0), "3.";
    W = (1, 1), "W.";
    A = (1, 2), "A.";
    DIGIT_4 = (1, 3), "4.";
    Z = (1, 4), "Z.";
    S = (1, 5), "S.";
    E = (1, 6), "E.";
    LEFT_SHIFT = (1, 7), "The left SHIFT, and SHIFT LOCK.";
    DIGIT_5 = (2, 0), "5.";
    R = (2, 1), "R.";
    D = (2, 2), "D.";
    DIGIT_6 = (2, 3), "6.";
    C = (2, 4), "C.";
    F = (2, 5), "F.";
    T = (2, 6), "T.";
    X = (2, 7), "X.";
    DIGIT_7 = (3, 0), "7.";
    Y = (3, 1), "Y.";
    G = (3, 2), "G.";
    DIGIT_8 = (3, 3), "8.";
    B = (3, 4), "B.";
    H = (3, 5), "H.";
    U = (3, 6), "U.";
    V = (3, 7), "V.";
    DIGIT_9 = (4, 0), "9.";
    I = (4, 1), "I.";
    J = (4, 2), "J.";
    DIGIT_0 = (4, 3), "0.";
    M = (4, 4), "M.";
    K = (4, 5), "K.";
    O = (4, 6), "O.";
    N = (4, 7), "N.";
    PLUS = (5, 0), "+.";
    P = (5, 1), "P.";
    L = (5, 2), "L.";
    MINUS = (5, 3), "-.";
    PERIOD = (5, 4), "Full stop.";
    COLON = (5, 5), "Colon.";
    AT = (5, 6), "@.";
    COMMA = (5, 7), "Comma.";
    POUND = (6, 0), "The pound sign.";
    ASTERISK = (6, 1), "*.";
    SEMICOLON = (6, 2), "Semicolon.";
    HOME = (6, 3), "CLR HOME.";
    RIGHT_SHIFT = (6, 4), "The right SHIFT.";
    EQUALS = (6, 5), "=.";
    UP_ARROW = (6, 6), "The up arrow.";
    SLASH = (6, 7), "/.";
    DIGIT_1 = (7, 0), "1.";
    LEFT_ARROW = (7, 1), "The left arrow.";
    CONTROL = (7, 2), "CTRL.";
    DIGIT_2 = (7, 3), "2.";
    SPACE = (7, 4), "The space bar.";
    COMMODORE = (7, 5), "The Commodore key.";
    Q = (7, 6), "Q.";
    RUN_STOP = (7, 7), "RUN STOP.";
}

impl Key {
    /// Its number in the KERNAL's scan: eight to a column, a column's rows
    /// in order.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.column * 8 + self.row
    }

    /// The port A line that selects it.
    #[must_use]
    pub const fn column(self) -> u8 {
        self.column
    }

    /// The port B line it pulls down.
    #[must_use]
    pub const fn row(self) -> u8 {
        self.row
    }

    /// The key whose [`code`](Self::code) is `code`, if there is one.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.get(usize::from(code)).copied()
    }

    /// The key that types `ch` as the upper-case set shows it, and whether
    /// SHIFT goes with it: a line's end is RETURN, `^` the up arrow, `_` the
    /// left arrow, and `` ` `` the pound sign.
    #[must_use]
    pub fn typing(ch: char) -> Option<(Self, bool)> {
        let plain = |key| Some((key, false));
        let shifted = |key| Some((key, true));
        match ch {
            'A'..='Z' => plain(LETTERS[usize::from(ch as u8 - b'A')]),
            '0'..='9' => plain(DIGITS[usize::from(ch as u8 - b'0')]),
            ' ' => plain(Self::SPACE),
            '\r' | '\n' => plain(Self::RETURN),
            ',' => plain(Self::COMMA),
            '.' => plain(Self::PERIOD),
            ':' => plain(Self::COLON),
            ';' => plain(Self::SEMICOLON),
            '=' => plain(Self::EQUALS),
            '+' => plain(Self::PLUS),
            '-' => plain(Self::MINUS),
            '*' => plain(Self::ASTERISK),
            '/' => plain(Self::SLASH),
            '@' => plain(Self::AT),
            '^' | '\u{2191}' => plain(Self::UP_ARROW),
            '_' | '\u{2190}' => plain(Self::LEFT_ARROW),
            '£' | '`' => plain(Self::POUND),
            '\u{3C0}' => shifted(Self::UP_ARROW),
            '!' => shifted(Self::DIGIT_1),
            '"' => shifted(Self::DIGIT_2),
            '#' => shifted(Self::DIGIT_3),
            '$' => shifted(Self::DIGIT_4),
            '%' => shifted(Self::DIGIT_5),
            '&' => shifted(Self::DIGIT_6),
            '\'' => shifted(Self::DIGIT_7),
            '(' => shifted(Self::DIGIT_8),
            ')' => shifted(Self::DIGIT_9),
            '<' => shifted(Self::COMMA),
            '>' => shifted(Self::PERIOD),
            '?' => shifted(Self::SLASH),
            '[' => shifted(Self::COLON),
            ']' => shifted(Self::SEMICOLON),
            _ => None,
        }
    }
}

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
const DIGITS: [Key; 10] = [
    Key::DIGIT_0,
    Key::DIGIT_1,
    Key::DIGIT_2,
    Key::DIGIT_3,
    Key::DIGIT_4,
    Key::DIGIT_5,
    Key::DIGIT_6,
    Key::DIGIT_7,
    Key::DIGIT_8,
    Key::DIGIT_9,
];

/// A key on the C64's keyboard: one of the matrix's, or RESTORE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keycap {
    /// A key of the matrix.
    Matrix(Key),
    /// RESTORE.
    Restore,
}

/// RESTORE's number, just past the matrix's codes.
const RESTORE_NUMBER: u8 = 64;
const _: () = assert!(Key::ALL.len() == RESTORE_NUMBER as usize);

impl Keycap {
    /// Its number: a matrix key's code, then RESTORE.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Matrix(key) => key.code(),
            Self::Restore => RESTORE_NUMBER,
        }
    }

    /// The keycap numbered `number`, if there is one.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        if number == RESTORE_NUMBER {
            Some(Self::Restore)
        } else {
            Key::from_code(number).map(Self::Matrix)
        }
    }
}

/// The keys held, as the matrix connects them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keyboard {
    /// For each column, the rows it is joined to.
    rows: [u8; 8],
    /// For each row, the columns it is joined to.
    columns: [u8; 8],
}

impl Keyboard {
    /// Nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rows: [0; 8],
            columns: [0; 8],
        }
    }

    /// Press or release `key`.
    pub fn set(&mut self, key: Key, down: bool) {
        let (column, row) = (usize::from(key.column), usize::from(key.row));
        if down {
            self.rows[column] |= 1 << row;
            self.columns[row] |= 1 << column;
        } else {
            self.rows[column] &= !(1 << row);
            self.columns[row] &= !(1 << column);
        }
    }

    /// Whether `key` is held.
    #[must_use]
    pub const fn held(&self, key: Key) -> bool {
        self.rows[key.column as usize] & 1 << key.row != 0
    }

    /// The levels on port A and port B when their lines are driven to
    /// `port_a` and `port_b`: a low line pulls down every line a held key
    /// joins it to, and through them every line those join, as the
    /// matrix's ghosting does.
    #[must_use]
    pub fn resolve(&self, port_a: u8, port_b: u8) -> (u8, u8) {
        let (mut low_a, mut low_b) = (!port_a, !port_b);
        loop {
            let reached_b = spread(low_a, self.rows) | low_b;
            let reached_a = spread(reached_b, self.columns) | low_a;
            if (reached_a, reached_b) == (low_a, low_b) {
                return (!low_a, !low_b);
            }
            (low_a, low_b) = (reached_a, reached_b);
        }
    }
}

snapshot::fields!(
    Keyboard { rows, columns },
    check
        | k
        | (0..8).all(|column| {
            (0..8).all(|row| (k.rows[column] >> row) & 1 == (k.columns[row] >> column) & 1)
        })
);

/// The lines `joined` connects to any of `low`.
fn spread(low: u8, joined: [u8; 8]) -> u8 {
    joined
        .into_iter()
        .enumerate()
        .filter(|&(line, _)| low & 1 << line != 0)
        .fold(0, |reached, (_, lines)| reached | lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_places_each_key_once() {
        let mut seen = [[false; 8]; 8];
        for key in Key::ALL {
            let place = &mut seen[usize::from(key.column)][usize::from(key.row)];
            assert!(!*place, "{key:?}");
            *place = true;
        }
    }

    #[test]
    fn a_held_key_joins_its_column_to_its_row() {
        let mut keyboard = Keyboard::new();
        keyboard.set(Key::A, true);
        assert_eq!(
            keyboard.resolve(!0x02, 0xFF),
            (!0x02, !0x04),
            "column 1 low pulls row 2"
        );
        assert_eq!(keyboard.resolve(0xFF, 0xFF), (0xFF, 0xFF));
        assert_eq!(
            keyboard.resolve(0xFF, !0x04),
            (!0x02, !0x04),
            "and the other way"
        );
        keyboard.set(Key::A, false);
        assert_eq!(keyboard.resolve(!0x02, 0xFF), (!0x02, 0xFF));
    }

    #[test]
    fn every_keycap_has_its_own_number() {
        for number in 0..=u8::MAX {
            let keycap = Keycap::from_number(number);
            assert_eq!(keycap.is_some(), number <= RESTORE_NUMBER, "{number}");
            assert!(keycap.is_none_or(|keycap| keycap.number() == number));
        }
        assert!(Key::ALL
            .iter()
            .all(|&key| Key::from_code(key.code()) == Some(key)));
    }

    #[test]
    fn three_keys_on_a_rectangle_ghost_the_fourth() {
        let mut keyboard = Keyboard::new();
        for key in [Key::W, Key::A, Key::R] {
            keyboard.set(key, true);
        }
        // W (1, 1), A (1, 2) and R (2, 1) join column 2 to row 2 as D would.
        let (_, rows) = keyboard.resolve(!(1 << 2), 0xFF);
        assert_eq!(rows & 1 << 2, 0, "D reads held");
        assert!(!keyboard.held(Key::D));
    }
}

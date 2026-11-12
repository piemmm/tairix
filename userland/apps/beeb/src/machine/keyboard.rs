//! The keyboard: ten columns by eight rows of switches, and the eight startup
//! links that share row 0 of columns 2–9.
//!
//! The MOS reads it one switch at a time through the slow data bus, or lets
//! the keyboard's own counter scan it, in which case a key down in rows 1–7 of
//! the column being scanned raises CA2 on the System VIA. BREAK is not in the
//! matrix; it drives the reset line.

/// A key, by its internal key number: row × 16 + column, as the Advanced
/// User Guide's Appendix C tabulates them.
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
    SHIFT = 0x00, "SHIFT";
    CTRL = 0x01, "CTRL";
    Q = 0x10, "Q";
    DIGIT_3 = 0x11, "3";
    DIGIT_4 = 0x12, "4";
    DIGIT_5 = 0x13, "5";
    F4 = 0x14, "f4";
    DIGIT_8 = 0x15, "8";
    F7 = 0x16, "f7";
    MINUS = 0x17, "-";
    CARET = 0x18, "^";
    LEFT = 0x19, "cursor left";
    F0 = 0x20, "f0";
    W = 0x21, "W";
    E = 0x22, "E";
    T = 0x23, "T";
    DIGIT_7 = 0x24, "7";
    I = 0x25, "I";
    DIGIT_9 = 0x26, "9";
    DIGIT_0 = 0x27, "0";
    UNDERSCORE = 0x28, "_";
    DOWN = 0x29, "cursor down";
    DIGIT_1 = 0x30, "1";
    DIGIT_2 = 0x31, "2";
    D = 0x32, "D";
    R = 0x33, "R";
    DIGIT_6 = 0x34, "6";
    U = 0x35, "U";
    O = 0x36, "O";
    P = 0x37, "P";
    LEFT_BRACKET = 0x38, "[";
    UP = 0x39, "cursor up";
    CAPS_LOCK = 0x40, "CAPS LOCK";
    A = 0x41, "A";
    X = 0x42, "X";
    F = 0x43, "F";
    Y = 0x44, "Y";
    J = 0x45, "J";
    K = 0x46, "K";
    AT = 0x47, "@";
    COLON = 0x48, ":";
    RETURN = 0x49, "RETURN";
    SHIFT_LOCK = 0x50, "SHIFT LOCK";
    S = 0x51, "S";
    C = 0x52, "C";
    G = 0x53, "G";
    H = 0x54, "H";
    N = 0x55, "N";
    L = 0x56, "L";
    SEMICOLON = 0x57, ";";
    RIGHT_BRACKET = 0x58, "]";
    DELETE = 0x59, "DELETE";
    TAB = 0x60, "TAB";
    Z = 0x61, "Z";
    SPACE = 0x62, "SPACE";
    V = 0x63, "V";
    B = 0x64, "B";
    M = 0x65, "M";
    COMMA = 0x66, ",";
    FULL_STOP = 0x67, ".";
    SLASH = 0x68, "/";
    COPY = 0x69, "COPY";
    ESCAPE = 0x70, "ESCAPE";
    F1 = 0x71, "f1";
    F2 = 0x72, "f2";
    F3 = 0x73, "f3";
    F5 = 0x74, "f5";
    F6 = 0x75, "f6";
    F8 = 0x76, "f8";
    F9 = 0x77, "f9";
    BACKSLASH = 0x78, "\\";
    RIGHT = 0x79, "cursor right";
}

impl Key {
    /// The key with internal key number `number`, if the keyboard has one.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Self::ALL.iter().copied().find(|key| key.0 == number)
    }

    /// Its internal key number.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Its column, 0–9.
    #[must_use]
    pub const fn column(self) -> u8 {
        self.0 & 0x0F
    }

    /// Its row, 0–7.
    #[must_use]
    pub const fn row(self) -> u8 {
        self.0 >> 4
    }
}

/// Columns the scan counter steps through: the counter is four bits wide and
/// only the first [`COLUMNS`] values select a column.
pub(crate) const SCAN_COLUMNS: u64 = 16;
/// Columns the matrix has.
const COLUMNS: u8 = 10;

/// The switches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyboard {
    /// Bit `row` of `columns[column]` is set while that switch is closed.
    columns: [u8; 16],
}

impl Keyboard {
    /// A keyboard with no key down and every startup link open.
    #[must_use]
    pub const fn new() -> Self {
        Self { columns: [0; 16] }
    }

    /// Press or release `key`.
    pub fn set(&mut self, key: Key, down: bool) {
        let column = &mut self.columns[usize::from(key.column())];
        let bit = 1 << key.row();
        if down {
            *column |= bit;
        } else {
            *column &= !bit;
        }
    }

    /// Release every key, leaving the links as they are.
    pub fn release_all(&mut self) {
        let options = self.startup_options();
        self.columns = [0; 16];
        self.set_startup_options(options);
    }

    /// The startup options the links set, as OSBYTE &FF reports them after a
    /// hard BREAK: bit 0 is the link in column 9, bit 7 the one in column 2,
    /// and an open link reads as a set bit, so a keyboard with no link fitted
    /// gives `&FF` — mode 7, SHIFT-BREAK booting, the slowest disc timings.
    #[must_use]
    pub fn startup_options(&self) -> u8 {
        (0..8).fold(0, |byte, bit| byte | ((!self.columns[9 - bit] & 1) << bit))
    }

    /// Fit or remove links so the startup options read `options`.
    pub fn set_startup_options(&mut self, options: u8) {
        for bit in 0..8 {
            let column = &mut self.columns[9 - bit];
            *column = (*column & !1) | ((!options >> bit) & 1);
        }
    }

    /// The switch the slow data bus selects, column in bits 0–3 and row in
    /// bits 4–6, as PA7 reads it. A column above 9 selects no switch.
    #[must_use]
    pub const fn scan(&self, selector: u8) -> bool {
        let column = selector & 0x0F;
        let row = (selector >> 4) & 0x07;
        column < COLUMNS && self.columns[column as usize] & (1 << row) != 0
    }

    /// Whether a key in rows 1–7 of `column` is down: what the autoscan
    /// raises CA2 on. Row 0 holds SHIFT, CTRL and the links, which never
    /// interrupt.
    #[must_use]
    pub const fn column_interrupts(&self, column: u8) -> bool {
        column < COLUMNS && self.columns[column as usize] & 0xFE != 0
    }

    /// The column the free-running counter selects at 1 MHz tick `tick`.
    #[must_use]
    pub const fn scanned_column(tick: u64) -> u8 {
        (tick % SCAN_COLUMNS) as u8
    }
}

crate::snapshot::fields!(
    Keyboard { columns },
    check
        | keyboard
        | keyboard.columns[usize::from(COLUMNS)..]
            .iter()
            .all(|&column| column == 0)
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_its_own_number_of_row_times_sixteen_plus_column() {
        assert_eq!(Key::ALL.len(), 72);
        let mut numbers: alloc::vec::Vec<u8> = Key::ALL.iter().map(|key| key.number()).collect();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(numbers.len(), Key::ALL.len());
        assert!(Key::ALL
            .iter()
            .all(|key| key.column() < 10 && key.row() < 8));
        assert_eq!((Key::RETURN.row(), Key::RETURN.column()), (4, 9));
        assert_eq!(Key::from_number(0x49), Some(Key::RETURN));
        assert_eq!(Key::from_number(0x0F), None);
        assert_eq!(Key::RIGHT.label(), "cursor right");
    }

    #[test]
    fn a_key_closes_its_switch_but_only_rows_1_to_7_raise_the_interrupt() {
        let mut keyboard = Keyboard::new();
        keyboard.set(Key::SHIFT, true);
        assert!(keyboard.scan(0x00));
        assert!(!keyboard.column_interrupts(0), "SHIFT never interrupts");
        keyboard.set(Key::Q, true);
        assert!(keyboard.scan(0x10));
        assert!(keyboard.column_interrupts(0));
        assert!(!keyboard.column_interrupts(1));
        keyboard.set(Key::Q, false);
        assert!(!keyboard.scan(0x10));
        assert!(!keyboard.column_interrupts(0));
    }

    #[test]
    fn a_column_above_9_selects_no_switch() {
        let mut keyboard = Keyboard::new();
        keyboard.set_startup_options(0);
        for key in Key::ALL {
            keyboard.set(*key, true);
        }
        for column in 10..16 {
            assert!((0..8).all(|row| !keyboard.scan(row << 4 | column)));
            assert!(!keyboard.column_interrupts(column));
        }
    }

    #[test]
    fn an_unfitted_link_reads_as_a_set_startup_option_bit() {
        let mut keyboard = Keyboard::new();
        assert_eq!(keyboard.startup_options(), 0xFF);
        assert!((2..10).all(|column| !keyboard.scan(column)));
        // Clearing bit 3 reverses SHIFT-BREAK: the link in column 6 is fitted.
        keyboard.set_startup_options(0xF7);
        assert!(keyboard.scan(0x06));
        assert!(!keyboard.scan(0x05) && !keyboard.scan(0x07));
        keyboard.set(Key::A, true);
        keyboard.release_all();
        assert!(!keyboard.scan(Key::A.number()));
        assert_eq!(keyboard.startup_options(), 0xF7);
    }

    #[test]
    fn the_scan_counter_steps_through_sixteen_values() {
        let columns: alloc::vec::Vec<u8> = (14..20).map(Keyboard::scanned_column).collect();
        assert_eq!(columns, [14, 15, 0, 1, 2, 3]);
    }
}

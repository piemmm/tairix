//! What DOS's programs write: the messages more than one of them gives, each
//! the one text MS-DOS's own programs share, and numbers as DOS writes them.

use alloc::vec::Vec;

pub const NEW_LINE: &[u8] = b"\r\n";

pub const TOO_MANY: &[u8] = b"Too many parameters\r\n";
pub const MISSING: &[u8] = b"Required parameter missing\r\n";
pub const BAD_SWITCH: &[u8] = b"Invalid switch\r\n";
pub const BAD_PARAMETER: &[u8] = b"Invalid parameter\r\n";
pub const BAD_FORMAT: &[u8] = b"Parameter format not correct\r\n";
pub const OUT_OF_RANGE: &[u8] = b"Parameter value not in allowed range\r\n";
pub const NOT_ALLOWED: &[u8] = b"Parameter value not allowed\r\n";

pub const BAD_DRIVE: &[u8] = b"Invalid drive specification\r\n";
pub const BAD_DATE: &[u8] = b"Invalid date\r\n";
pub const MEMORY: &[u8] = b"Insufficient memory\r\n";
pub const DISK_FULL: &[u8] = b"Insufficient disk space\r\n";
pub const FILE_NOT_FOUND: &[u8] = b"File not found\r\n";
pub const PATH_NOT_FOUND: &[u8] = b"Path not found\r\n";
pub const ACCESS_DENIED: &[u8] = b"Access denied\r\n";
pub const CREATION: &[u8] = b"File creation error\r\n";
pub const NO_DIRECTORY: &[u8] = b"Unable to create directory\r\n";
pub const ONTO_ITSELF: &[u8] = b"File cannot be copied onto itself\r\n";
pub const TRANSFERRED: &[u8] = b"System transferred\r\n";
pub const PRESS_ANY_KEY: &[u8] = b"Press any key to continue . . .";
pub const ASK_LABEL: &[u8] = b"Volume label (11 characters, ENTER for none)? ";

/// `line` with the name of what it is about, as DOS's message service
/// names it: `File not found - NAME`.
#[must_use]
pub fn named(line: &[u8], name: &[u8]) -> Vec<u8> {
    let words = line.strip_suffix(NEW_LINE).unwrap_or(line);
    [words, b" - ", name, NEW_LINE].concat()
}

/// `n` in decimal.
#[must_use]
pub fn decimal(n: u32) -> Vec<u8> {
    let mut digits = Vec::new();
    let mut rest = n;
    loop {
        digits.push(b'0' + u8::try_from(rest % 10).unwrap_or(0));
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    digits.reverse();
    digits
}

/// `n` in decimal, right-aligned in `width` columns.
#[must_use]
pub fn decimal_in(n: u32, width: usize) -> Vec<u8> {
    let digits = decimal(n);
    let mut out = alloc::vec![b' '; width.saturating_sub(digits.len())];
    out.extend_from_slice(&digits);
    out
}

/// `value`'s last `out.len()` hexadecimal digits, upper case, into `out`.
pub fn hex(value: u32, out: &mut [u8]) {
    for (place, slot) in out.iter_mut().rev().enumerate() {
        let shift = u32::try_from(place * 4).unwrap_or(u32::MAX);
        let digit = value.checked_shr(shift).unwrap_or(0) & 0xF;
        *slot = b"0123456789ABCDEF"[usize::try_from(digit).unwrap_or(0)];
    }
}

/// A volume's serial number as DOS shows it: `1A2B-3C4D`.
#[must_use]
pub fn serial_text(serial: u32) -> [u8; 9] {
    let mut out = *b"0000-0000";
    hex(serial >> 16, &mut out[..4]);
    hex(serial, &mut out[5..]);
    out
}

/// The line that gives a volume's serial number.
#[must_use]
pub fn serial_line(serial: u32) -> Vec<u8> {
    [
        &b"Volume Serial Number is "[..],
        &serial_text(serial),
        NEW_LINE,
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_writes_the_last_digits_the_slot_holds() {
        let mut out = [0u8; 4];
        hex(0x1234_ABCD, &mut out);
        assert_eq!(&out, b"ABCD");
        let mut wide = [0u8; 10];
        hex(0xFFFF_FFFF, &mut wide);
        assert_eq!(&wide, b"00FFFFFFFF");
        let mut none: [u8; 0] = [];
        hex(1, &mut none);
    }

    #[test]
    fn a_serial_number_is_two_words_of_hex() {
        assert_eq!(&serial_text(0x0123_ABCD), b"0123-ABCD");
        assert_eq!(serial_line(0), b"Volume Serial Number is 0000-0000\r\n");
    }

    #[test]
    fn a_named_message_puts_the_name_before_the_line_end() {
        assert_eq!(
            named(FILE_NOT_FOUND, b"A.TXT"),
            b"File not found - A.TXT\r\n"
        );
        assert_eq!(named(b"Not ready", b"A"), b"Not ready - A\r\n");
    }

    #[test]
    fn decimals_are_right_aligned_and_never_cut() {
        assert_eq!(decimal(0), b"0");
        assert_eq!(decimal(u32::MAX), b"4294967295");
        assert_eq!(decimal_in(42, 5), b"   42");
        assert_eq!(decimal_in(123_456, 3), b"123456");
    }
}

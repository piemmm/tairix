//! Integers of any size, held as decimal digits, so a sequence of integers
//! is exact however far past a float's integers it runs.

use alloc::vec::Vec;
use core::cmp::Ordering;

/// A signed integer.
///
/// The digits are least significant first with no high zeros, so zero has
/// none; a negative zero is kept, because `seq -0 …` prints `-0` first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Integer {
    negative: bool,
    digits: Vec<u8>,
}

impl Integer {
    /// The integer an integral decimal or hexadecimal spelling names (an
    /// operand already scanned whole), or `None` for a spelling with a
    /// fraction or one past any finite `f64`.
    #[must_use]
    pub fn from_spelling(spelling: &str) -> Option<Self> {
        let (negative, digits, inexact) = parse(spelling)?;
        (!inexact).then_some(Self { negative, digits })
    }

    /// The integer nearest the bound `spelling` names on a sequence's side
    /// of it — its floor for an ascending sequence, its ceiling for a
    /// descending one — so an integer passes the one exactly when it passes
    /// the other.
    #[must_use]
    pub fn bound(spelling: &str, descending: bool) -> Option<Self> {
        let (negative, mut digits, inexact) = parse(spelling)?;
        // A fraction rounds away from zero for a positive ceiling or a
        // negative floor, and toward it otherwise.
        if inexact && negative != descending {
            add_magnitude(&mut digits, &[1]);
        }
        Some(Self {
            negative: negative && !digits.is_empty(),
            digits,
        })
    }

    /// Whether the integer is below zero; a negative zero is not.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.negative && !self.digits.is_empty()
    }

    /// `self + other`. A zero sum is positive, as a float's is.
    pub fn add(&mut self, other: &Self) {
        if self.negative == other.negative {
            add_magnitude(&mut self.digits, &other.digits);
        } else if magnitude_order(&self.digits, &other.digits) == Ordering::Less {
            let mut larger = other.digits.clone();
            subtract_magnitude(&mut larger, &self.digits);
            self.digits = larger;
            self.negative = other.negative;
        } else {
            subtract_magnitude(&mut self.digits, &other.digits);
        }
        if self.digits.is_empty() {
            self.negative = false;
        }
    }

    /// Whether a `-` leads the integer as printed, as it does a negative
    /// zero's.
    #[must_use]
    pub const fn sign_shown(&self) -> bool {
        self.negative
    }

    /// The integer's decimal digits as printed, most significant first.
    pub fn digit_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        let zero = self.digits.is_empty().then_some(b'0');
        zero.into_iter()
            .chain(self.digits.iter().rev().map(|digit| b'0' + digit))
    }

    /// The bytes the integer prints as, its sign included.
    #[must_use]
    pub fn shown_len(&self) -> usize {
        usize::from(self.negative) + self.digits.len().max(1)
    }
}

impl Ord for Integer {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => magnitude_order(&self.digits, &other.digits),
            (true, true) => magnitude_order(&other.digits, &self.digits),
        }
    }
}

impl PartialOrd for Integer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The most digits an integer is held to: past it no finite `f64` reaches,
/// so a spelling that needs more is no operand of the exact path.
const MAX_DIGITS: usize = 400;

/// A decimal or hexadecimal integer spelling's sign, the digits of its
/// integer part, and whether a non-zero fraction was cut from it.
fn parse(spelling: &str) -> Option<(bool, Vec<u8>, bool)> {
    let spelling = spelling.trim_start_matches(tairix_util::cnum::c_isspace);
    let (negative, body) = match spelling.as_bytes().first() {
        Some(b'-') => (true, &spelling[1..]),
        Some(b'+') => (false, &spelling[1..]),
        _ => (false, spelling),
    };
    let (digits, inexact) = match body.get(..2) {
        Some(radix) if radix.eq_ignore_ascii_case("0x") => (from_hex(&body[2..])?, false),
        _ => from_decimal(body)?,
    };
    Some((negative, digits, inexact))
}

/// The integer part of a decimal spelling, and whether its fraction held a
/// non-zero digit.
fn from_decimal(body: &str) -> Option<(Vec<u8>, bool)> {
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (body, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let spelled: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes())
        .map(|digit| digit.is_ascii_digit().then(|| digit - b'0'))
        .collect::<Option<_>>()?;
    // The point sits this many digits into what was spelled, the exponent
    // having moved it.
    let point = i64::try_from(whole.len()).ok()?.checked_add(exponent)?;
    let split = usize::try_from(point.max(0)).ok()?;
    let (integral, fractional) = spelled.split_at(split.min(spelled.len()));
    let inexact = fractional.iter().any(|&digit| digit != 0);
    let significant: Vec<u8> = integral
        .iter()
        .copied()
        .skip_while(|&digit| digit == 0)
        .collect();
    if significant.is_empty() {
        return Some((Vec::new(), inexact));
    }
    let zeros = split - integral.len();
    if significant.len().saturating_add(zeros) > MAX_DIGITS {
        return None;
    }
    let digits = core::iter::repeat_n(0, zeros)
        .chain(significant.into_iter().rev())
        .collect();
    Some((digits, inexact))
}

/// The decimal digits of a hexadecimal integer's digits.
fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let mut digits = Vec::new();
    for nibble in hex.chars() {
        if digits.len() > MAX_DIGITS {
            return None;
        }
        let mut carry = nibble.to_digit(16)?;
        for digit in &mut digits {
            let value = u32::from(*digit) * 16 + carry;
            *digit = u8::try_from(value % 10).ok()?;
            carry = value / 10;
        }
        while carry > 0 {
            digits.push(u8::try_from(carry % 10).ok()?);
            carry /= 10;
        }
    }
    Some(digits)
}

/// Drop high zeros.
fn trim(digits: &mut Vec<u8>) {
    while digits.last() == Some(&0) {
        digits.pop();
    }
}

fn magnitude_order(a: &[u8], b: &[u8]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

fn add_magnitude(sum: &mut Vec<u8>, addend: &[u8]) {
    if sum.len() < addend.len() {
        sum.resize(addend.len(), 0);
    }
    let mut carry = 0;
    for (at, digit) in sum.iter_mut().enumerate() {
        let total = *digit + addend.get(at).copied().unwrap_or(0) + carry;
        (*digit, carry) = (total % 10, total / 10);
        if carry == 0 && at >= addend.len() {
            break;
        }
    }
    if carry > 0 {
        sum.push(carry);
    }
}

/// `larger − smaller`, `larger` being at least `smaller`.
fn subtract_magnitude(larger: &mut Vec<u8>, smaller: &[u8]) {
    let mut borrow = 0;
    for (at, digit) in larger.iter_mut().enumerate() {
        let taken = smaller.get(at).copied().unwrap_or(0) + borrow;
        (*digit, borrow) = if *digit >= taken {
            (*digit - taken, 0)
        } else {
            (*digit + 10 - taken, 1)
        };
        if borrow == 0 && at >= smaller.len() {
            break;
        }
    }
    trim(larger);
}

#[cfg(test)]
mod tests {
    use super::Integer;
    use alloc::string::String;
    use alloc::vec::Vec;

    fn int(text: &str) -> Integer {
        Integer::from_spelling(text).expect("an integral spelling")
    }

    fn shown(value: &Integer, width: usize) -> String {
        let mut out = Vec::new();
        if value.sign_shown() {
            out.push(b'-');
        }
        out.resize(width.saturating_sub(value.shown_len()) + out.len(), b'0');
        out.extend(value.digit_bytes());
        String::from_utf8(out).expect("digits")
    }

    #[test]
    fn integral_spellings_name_their_integers() {
        for (text, value) in [
            ("0", "0"),
            ("007", "7"),
            ("-12", "-12"),
            ("+5", "5"),
            ("1.5e1", "15"),
            ("1.e2", "100"),
            ("0.01e2", "1"),
            ("1e25", "10000000000000000000000000"),
            ("0x1F", "31"),
            ("-0x10", "-16"),
            ("0xFFFFFFFFFFFFFFFFFF", "4722366482869645213695"),
        ] {
            assert_eq!(shown(&int(text), 0), value, "{text}");
        }
        assert_eq!(Integer::from_spelling("1.5"), None);
        assert_eq!(Integer::from_spelling("15e-1"), None);
        assert_eq!(
            shown(&int("0e99999999999"), 0),
            "0",
            "no zero is held for an exponent"
        );
        assert_eq!(
            Integer::from_spelling("1e99999"),
            None,
            "past any finite double"
        );
    }

    #[test]
    fn a_bound_rounds_toward_the_sequence() {
        let bound =
            |text, descending| shown(&Integer::bound(text, descending).expect("a bound"), 0);
        assert_eq!(bound("2.5", false), "2");
        assert_eq!(bound("-2.5", false), "-3");
        assert_eq!(bound("2.5", true), "3");
        assert_eq!(bound("-2.5", true), "-2");
        assert_eq!(bound("-0.5", true), "0");
        assert_eq!(bound("25e-1", false), "2");
        assert_eq!(bound("7", true), "7");
        assert_eq!(bound("1.0", false), "1", "a zero fraction is exact");
    }

    #[test]
    fn sums_carry_and_borrow_across_any_length() {
        let mut value = int("99999999999999999999");
        value.add(&int("1"));
        assert_eq!(shown(&value, 0), "100000000000000000000");
        value.add(&int("-100000000000000000001"));
        assert_eq!(shown(&value, 0), "-1");
        value.add(&int("1"));
        assert_eq!(shown(&value, 0), "0", "a zero sum is positive");
        let mut zero = int("-0");
        assert_eq!(shown(&zero, 3), "-00", "a negative zero keeps its sign");
        zero.add(&int("-2"));
        assert_eq!(shown(&zero, 0), "-2");
    }

    #[test]
    fn order_is_numeric_and_signed() {
        assert!(int("-2") < int("-1"));
        assert!(int("-1") < int("0"));
        assert!(int("9") < int("10"));
        assert_eq!(int("-0").cmp(&int("0")), core::cmp::Ordering::Equal);
    }

    #[test]
    fn padding_follows_the_sign() {
        assert_eq!(shown(&int("5"), 3), "005");
        assert_eq!(shown(&int("-5"), 3), "-05");
        assert_eq!(shown(&int("-123"), 2), "-123");
        assert_eq!(int("-123").shown_len(), 4);
        assert_eq!(shown(&int("0"), 2), "00");
    }
}

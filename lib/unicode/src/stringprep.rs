//! Stringprep (RFC 3454) under SASLprep (RFC 4013), its profile for user
//! names and passwords: one form of a string however it was typed — its
//! invisible characters dropped, its spaces plain, its compatibility forms
//! normalized — over Unicode 3.2, which stringprep fixes so a prepared string
//! never changes.

use crate::in_ranges;
use crate::normalize::{normalize_in, Form, Repertoire};
use crate::tables::{
    LEFT_TO_RIGHT, MAPPED_TO_NOTHING, NON_ASCII_SPACES, RIGHT_TO_LEFT, SASLPREP_PROHIBITED,
    UNASSIGNED_3_2,
};

/// What preparing a string makes of a code point Unicode 3.2 does not assign
/// (RFC 3454 §7).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unassigned {
    /// A query — a password offered, a name looked up: it passes through as
    /// it is.
    Allowed,
    /// A string to be stored — a password being set: it is refused.
    Refused,
}

/// Why a string has no SASLprep form.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaslprepError {
    /// It holds a character SASLprep prohibits (RFC 4013 §2.3): a control,
    /// a private-use, surrogate or noncharacter code point, a character that
    /// is inappropriate for plain text or changes how text is displayed, or
    /// a tag.
    Prohibited,
    /// It holds a code point Unicode 3.2 does not assign, which
    /// [`Unassigned::Refused`] refuses.
    Unassigned,
    /// Its right-to-left text breaks stringprep's bidirectional rules (RFC
    /// 3454 §6): it is mixed with left-to-right text, or does not both begin
    /// and end the string.
    Bidi,
    /// Prepared, it is longer than the buffer it is written into.
    Overflow,
}

/// `input` as SASLprep prepares it, written to the start of `out`: how many
/// characters it takes there.
///
/// The non-ASCII spaces become U+0020 and the characters stringprep maps to
/// nothing are dropped — U+200B ZERO WIDTH SPACE among them, which RFC 3454
/// lists as both without ordering the two mappings, a choice implementations
/// differ on — then the string is normalized to NFKC as Unicode 3.2 defines
/// it, and checked. `out` need only be as long as the prepared string, and
/// [`MAX_DECOMPOSITION`](crate::MAX_DECOMPOSITION) times the input's length
/// always is.
///
/// # Errors
///
/// [`SaslprepError`] where the string has no prepared form, or it is longer
/// than `out`, whose contents are then unspecified.
pub fn saslprep(
    input: &str,
    unassigned: Unassigned,
    out: &mut [char],
) -> Result<usize, SaslprepError> {
    let mapped = input.chars().filter_map(|ch| {
        let code = u32::from(ch);
        if in_ranges(MAPPED_TO_NOTHING, code) {
            None
        } else if in_ranges(NON_ASCII_SPACES, code) {
            Some(' ')
        } else {
            Some(ch)
        }
    });
    let len = normalize_in(mapped, Form::Nfkc, Repertoire::Stringprep, out)
        .map_err(|_| SaslprepError::Overflow)?;
    let prepared = out.get(..len).ok_or(SaslprepError::Overflow)?;
    let (mut right_to_left, mut left_to_right) = (false, false);
    for &ch in prepared {
        let code = u32::from(ch);
        if in_ranges(SASLPREP_PROHIBITED, code) {
            return Err(SaslprepError::Prohibited);
        }
        if unassigned == Unassigned::Refused && in_ranges(UNASSIGNED_3_2, code) {
            return Err(SaslprepError::Unassigned);
        }
        right_to_left = right_to_left || in_ranges(RIGHT_TO_LEFT, code);
        left_to_right = left_to_right || in_ranges(LEFT_TO_RIGHT, code);
    }
    // RFC 3454 §6: right-to-left text holds no left-to-right character, and
    // both begins and ends with a right-to-left one.
    let right_to_left_at =
        |end: Option<&char>| end.is_some_and(|&ch| in_ranges(RIGHT_TO_LEFT, u32::from(ch)));
    if right_to_left
        && (left_to_right
            || !right_to_left_at(prepared.first())
            || !right_to_left_at(prepared.last()))
    {
        return Err(SaslprepError::Bidi);
    }
    Ok(len)
}

#[cfg(test)]
#[path = "stringprep_tests.rs"]
mod tests;

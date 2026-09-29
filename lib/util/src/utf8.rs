//! Which bytes begin a UTF-8 sequence, and how long it is (Unicode 15.0,
//! Table 3-7), for the decoders that must step over or validate text by hand.

/// How many bytes the sequence `lead` begins, or `None` for a byte no
/// well-formed sequence begins with: a continuation byte, `0xC0`/`0xC1`
/// (always overlong), or `0xF5..=0xFF` (past U+10FFFF).
#[must_use]
pub const fn sequence_len(lead: u8) -> Option<usize> {
    match lead {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::sequence_len;

    #[test]
    fn every_lead_matches_the_standard_decoder() {
        for lead in 0..=u8::MAX {
            // Only the second byte is constrained beyond being a continuation.
            let decoded = (1..=4).find(|&len| {
                (0x80..=0xbf)
                    .any(|second| core::str::from_utf8(&[lead, second, 0x80, 0x80][..len]).is_ok())
            });
            assert_eq!(sequence_len(lead), decoded, "lead {lead:#04x}");
        }
    }
}

//! ITU-T G.711 μ-law and A-law, expanded to 16-bit linear as the
//! recommendation's reference expansion does (Sun's `g711.c`).

/// μ-law's bias, added to a magnitude before its segment is taken.
const ULAW_BIAS: i32 = 0x84;

/// The 16-bit linear sample μ-law code `code` stands for, within ±32124.
const fn ulaw(code: u8) -> i16 {
    let code = !code as i32;
    let magnitude = (((code & 0x0F) << 3) + ULAW_BIAS) << ((code & 0x70) >> 4);
    let linear = if code & 0x80 != 0 {
        ULAW_BIAS - magnitude
    } else {
        magnitude - ULAW_BIAS
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "every expansion lies within ±32124"
    )]
    let sample = linear as i16;
    sample
}

/// The 16-bit linear sample A-law code `code` stands for, within ±32256.
const fn alaw(code: u8) -> i16 {
    let code = (code ^ 0x55) as i32;
    let segment = (code & 0x70) >> 4;
    let mut magnitude = (code & 0x0F) << 4;
    magnitude += match segment {
        0 => 8,
        _ => 0x108,
    };
    if segment > 1 {
        magnitude <<= segment - 1;
    }
    let linear = if code & 0x80 != 0 {
        magnitude
    } else {
        -magnitude
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "every expansion lies within ±32256"
    )]
    let sample = linear as i16;
    sample
}

/// Every code's sample under `expand`, built at compile time.
macro_rules! table {
    ($expand:ident) => {{
        let mut table = [0i16; 256];
        let mut code: u8 = 0;
        loop {
            table[code as usize] = $expand(code);
            if code == u8::MAX {
                break table;
            }
            code += 1;
        }
    }};
}

/// Every μ-law code's linear sample.
pub(crate) const ULAW: [i16; 256] = table!(ulaw);

/// Every A-law code's linear sample.
pub(crate) const ALAW: [i16; 256] = table!(alaw);

#[cfg(test)]
mod tests {
    use super::{ALAW, ULAW};

    #[test]
    fn the_expansions_are_the_recommendations() {
        // The two silence codes, the extremes, and a code in each sign.
        assert_eq!((ULAW[0xFF], ULAW[0x7F]), (0, 0));
        assert_eq!((ULAW[0x00], ULAW[0x80]), (-32124, 32124));
        assert_eq!((ULAW[0xF0], ULAW[0x70]), (120, -120));
        assert_eq!((ALAW[0xD5], ALAW[0x55]), (8, -8));
        assert_eq!((ALAW[0xAA], ALAW[0x2A]), (32256, -32256));
        assert_eq!((ALAW[0xC5], ALAW[0x45]), (264, -264));
    }

    #[test]
    fn each_law_is_odd_and_monotone_in_its_magnitude() {
        for code in 0u8..0x80 {
            assert_eq!(ULAW[usize::from(code)], -ULAW[usize::from(code | 0x80)]);
            assert_eq!(ALAW[usize::from(code)], -ALAW[usize::from(code | 0x80)]);
        }
        // μ-law's magnitude falls as its code rises; A-law's, with its even
        // bits inverted, rises.
        for code in 0x80u8..0xFF {
            assert!(ULAW[usize::from(code)] > ULAW[usize::from(code + 1)]);
            let at = |c: u8| ALAW[usize::from(c ^ 0x55)];
            assert!(at(code) < at(code + 1));
        }
    }
}

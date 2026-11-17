//! The UK territory's alphabet, Latin1, as text: the character properties,
//! the case, control, plain and value tables, and its collation — three
//! passes, by letter, then accent, then case, through one table of sort
//! weights — with the transformation that sorts as it collates.

/// A byte range in a table's definition.
type Span = (u8, u8);

/// The characters each property holds, from 0: control, upper case, lower
/// case, alphabetic, punctuation, space, digit, hexadecimal digit, accented,
/// forward-flowing and backward-flowing.
const PROPERTIES: [&[Span]; 11] = [
    &[(0x00, 0x1F), (0x7F, 0x7F)],
    &[
        (0x41, 0x5A),
        (0x81, 0x81),
        (0x85, 0x85),
        (0x9A, 0x9A),
        (0xC0, 0xD6),
        (0xD8, 0xDE),
    ],
    &[
        (0x61, 0x7A),
        (0x82, 0x82),
        (0x86, 0x86),
        (0x9B, 0x9B),
        (0x9E, 0x9F),
        (0xDF, 0xF6),
        (0xF8, 0xFF),
    ],
    &[
        (0x41, 0x5A),
        (0x61, 0x7A),
        (0x81, 0x82),
        (0x85, 0x86),
        (0x9A, 0x9B),
        (0x9E, 0x9F),
        (0xC0, 0xD6),
        (0xD8, 0xF6),
        (0xF8, 0xFF),
    ],
    &[
        (0x21, 0x2F),
        (0x3A, 0x40),
        (0x5B, 0x60),
        (0x7B, 0x7E),
        (0x80, 0x80),
        (0x8C, 0x99),
        (0x9C, 0x9D),
        (0xA1, 0xBF),
        (0xD7, 0xD7),
        (0xF7, 0xF7),
    ],
    &[(0x09, 0x0D), (0x20, 0x20), (0xA0, 0xA0)],
    &[(0x30, 0x39)],
    &[(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)],
    &[
        (0x81, 0x82),
        (0x85, 0x86),
        (0xC0, 0xC5),
        (0xC7, 0xCF),
        (0xD1, 0xD6),
        (0xD8, 0xDD),
        (0xE0, 0xE5),
        (0xE7, 0xEF),
        (0xF1, 0xF6),
        (0xF8, 0xFD),
        (0xFF, 0xFF),
    ],
    &[(0x00, 0xFF)],
    &[],
];

/// The properties there are.
pub(crate) const PROPERTY_COUNT: usize = PROPERTIES.len();

/// Each property's bit map: bit `c & 7` of byte `c >> 3` set where `c` has
/// it.
pub(crate) const PROPERTY_MAPS: [[u8; 32]; PROPERTY_COUNT] = {
    let mut maps = [[0; 32]; PROPERTY_COUNT];
    let mut property = 0;
    while property < PROPERTY_COUNT {
        let spans = PROPERTIES[property];
        let mut span = 0;
        while span < spans.len() {
            let (first, last) = spans[span];
            let mut c = first as usize;
            while c <= last as usize {
                maps[property][c >> 3] |= 1 << (c & 7);
                c += 1;
            }
            span += 1;
        }
        property += 1;
    }
    maps
};

/// A table that maps every byte to itself but those listed.
const fn identity_but(changes: &[(u8, u8)]) -> [u8; 256] {
    let mut table = [0; 256];
    let mut c = 0;
    loop {
        table[c as usize] = c;
        if c == u8::MAX {
            break;
        }
        c += 1;
    }
    let mut i = 0;
    while i < changes.len() {
        table[changes[i].0 as usize] = changes[i].1;
        i += 1;
    }
    table
}

/// `table` with each byte of `spans` moved by `delta`.
const fn shifted(mut table: [u8; 256], spans: &[Span], delta: i8) -> [u8; 256] {
    let mut span = 0;
    while span < spans.len() {
        let (first, last) = spans[span];
        let mut c = first;
        while c <= last {
            table[c as usize] = c.wrapping_add_signed(delta);
            if c == u8::MAX {
                break;
            }
            c += 1;
        }
        span += 1;
    }
    table
}

/// Upper case to lower: the letters of both halves, and RISC OS's own.
pub(crate) const LOWER: [u8; 256] = shifted(
    identity_but(&[(0x81, 0x82), (0x85, 0x86), (0x9A, 0x9B)]),
    &[(0x41, 0x5A), (0xC0, 0xD6), (0xD8, 0xDE)],
    0x20,
);

/// Lower case to upper; ÿ has no capital in Latin1, so it is Y.
pub(crate) const UPPER: [u8; 256] = shifted(
    identity_but(&[(0x82, 0x81), (0x86, 0x85), (0x9B, 0x9A), (0xFF, 0x59)]),
    &[(0x61, 0x7A), (0xE0, 0xF6), (0xF8, 0xFE)],
    -0x20,
);

/// The control code a character gives with Ctrl.
pub(crate) const CONTROL: [u8; 256] = {
    let mut table = identity_but(&[
        (0x21, 0x01),
        (0x22, 0x02),
        (0x24, 0x04),
        (0x25, 0x05),
        (0x26, 0x07),
        (0x27, 0x00),
        (0x28, 0x09),
        (0x29, 0x00),
        (0x2A, 0x08),
        (0x2D, 0x1F),
        (0x30, 0x00),
        (0x36, 0x1E),
        (0x81, 0x17),
        (0x82, 0x17),
        (0x85, 0x19),
        (0x86, 0x19),
        (0xA3, 0x03),
        (0xC7, 0x03),
        (0xE7, 0x03),
        (0xD1, 0x0E),
        (0xF1, 0x0E),
        (0xDD, 0x19),
        (0xFD, 0x19),
        (0xFF, 0x19),
    ]);
    let groups: [(u8, u8, u8); 12] = [
        (0x31, 0x35, 0),
        (0x37, 0x39, 0),
        (0x40, 0x5F, 0),
        (0x61, 0x7D, 0),
        (0xC0, 0xC5, 0x01),
        (0xE0, 0xE5, 0x01),
        (0xC8, 0xCB, 0x05),
        (0xE8, 0xEB, 0x05),
        (0xCC, 0xCF, 0x09),
        (0xEC, 0xEF, 0x09),
        (0xD2, 0xD6, 0x0F),
        (0xF2, 0xF6, 0x0F),
    ];
    let mut g = 0;
    while g < groups.len() {
        let (first, last, to) = groups[g];
        let mut c = first;
        while c <= last {
            table[c as usize] = if to != 0 {
                to
            } else if c >= 0x61 {
                c - 0x60
            } else if c >= 0x40 {
                c - 0x40
            } else {
                c - 0x30
            };
            c += 1;
        }
        g += 1;
    }
    let mut c = 0xD9;
    while c <= 0xDC {
        table[c] = 0x15;
        table[c + 0x20] = 0x15;
        c += 1;
    }
    table
};

/// Each character without its accent: the base letter, or itself.
pub(crate) const PLAIN: [u8; 256] = {
    let mut table = identity_but(&[
        (0x81, b'W'),
        (0x82, b'w'),
        (0x85, b'Y'),
        (0x86, b'y'),
        (0xC7, b'C'),
        (0xD1, b'N'),
        (0xD8, b'O'),
        (0xDD, b'Y'),
        (0xE7, b'c'),
        (0xF1, b'n'),
        (0xF8, b'o'),
        (0xFD, b'y'),
        (0xFF, b'y'),
    ]);
    let groups: [(u8, u8, u8); 10] = [
        (0xC0, 0xC5, b'A'),
        (0xC8, 0xCB, b'E'),
        (0xCC, 0xCF, b'I'),
        (0xD2, 0xD6, b'O'),
        (0xD9, 0xDC, b'U'),
        (0xE0, 0xE5, b'a'),
        (0xE8, 0xEB, b'e'),
        (0xEC, 0xEF, b'i'),
        (0xF2, 0xF6, b'o'),
        (0xF9, 0xFC, b'u'),
    ];
    let mut g = 0;
    while g < groups.len() {
        let (first, last, to) = groups[g];
        let mut c = first;
        while c <= last {
            table[c as usize] = to;
            c += 1;
        }
        g += 1;
    }
    table
};

/// Each character's value as a digit: hexadecimal for both cases, nought
/// for the rest.
pub(crate) const VALUE: [u8; 256] = {
    let mut table = [0; 256];
    let mut c = 0;
    while c < 10 {
        table[(b'0' + c) as usize] = c;
        c += 1;
    }
    let mut c = 0;
    while c < 6 {
        table[(b'A' + c) as usize] = 10 + c;
        table[(b'a' + c) as usize] = 10 + c;
        c += 1;
    }
    table
};

/// Each digit's character.
pub(crate) const REPRESENTATION: &[u8; 16] = b"0123456789ABCDEF";

/// Each character's sort weight, as the UK territory orders them.
#[rustfmt::skip]
const WEIGHTS: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
    0x20, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x30, 0x31,
    0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41,
    0x42, 0x43, 0x53, 0x55, 0x59, 0x5D, 0x67, 0x6B, 0x6D, 0x6F, 0x79, 0x7B, 0x7D, 0x7F, 0x81, 0x85,
    0x95, 0x97, 0x99, 0x9B, 0x9E, 0xA2, 0xAC, 0xAE, 0xB2, 0xB4, 0xBB, 0xBD, 0xBE, 0xBF, 0xC0, 0xC1,
    0xC2, 0x44, 0x54, 0x56, 0x5A, 0x5E, 0x68, 0x6C, 0x6E, 0x70, 0x7A, 0x7C, 0x7E, 0x80, 0x82, 0x86,
    0x96, 0x98, 0x9A, 0x9C, 0x9F, 0xA3, 0xAD, 0xAF, 0xB3, 0xB5, 0xBC, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7,
    0xC8, 0xB0, 0xB1, 0xC9, 0xCA, 0xB6, 0xB7, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF, 0xD0, 0xD1, 0xD2, 0xD3,
    0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0x91, 0x92, 0xDE, 0xDF, 0x69, 0x6A,
    0x21, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0x2F, 0xEC, 0xED,
    0xEE, 0xEF, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF0, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD,
    0x4B, 0x49, 0x45, 0x4D, 0x47, 0x4F, 0x51, 0x57, 0x65, 0x63, 0x5F, 0x61, 0x77, 0x75, 0x71, 0x73,
    0x5B, 0x83, 0x8D, 0x8B, 0x87, 0x8F, 0x89, 0xFE, 0x93, 0xAA, 0xA8, 0xA4, 0xA6, 0xB9, 0xA0, 0x9D,
    0x4C, 0x4A, 0x46, 0x4E, 0x48, 0x50, 0x52, 0x58, 0x66, 0x64, 0x60, 0x62, 0x78, 0x76, 0x72, 0x74,
    0x5C, 0x84, 0x8E, 0x8C, 0x88, 0x90, 0x8A, 0xFF, 0x94, 0xAB, 0xA9, 0xA5, 0xA7, 0xBA, 0xA1, 0xB8,
];

/// The ligatures collation takes apart: ﬁ and ﬂ.
const LIGATURES: [(u8, [u8; 2]); 2] = [(0x9E, *b"fi"), (0x9F, *b"fl")];

/// `Territory_Collate`'s flags: case and accents ignored, digit runs read as
/// numbers.
pub(crate) const IGNORE_CASE: u32 = 1;
pub(crate) const IGNORE_ACCENTS: u32 = 1 << 1;
pub(crate) const NUMERIC: u32 = 1 << 2;

/// A collation pass: how a character becomes the byte weighed. Case is
/// weighed on the unaccented letters where accents are ignored.
#[derive(Clone, Copy)]
enum Pass {
    Letters,
    Accents,
    Case,
    UnaccentedCase,
}

impl Pass {
    const fn weighed(self, c: u8) -> u8 {
        WEIGHTS[match self {
            Self::Letters => LOWER[PLAIN[c as usize] as usize],
            Self::Accents => LOWER[c as usize],
            Self::Case => c,
            Self::UnaccentedCase => PLAIN[c as usize],
        } as usize]
    }
}

/// A string as one pass reads it: each character, a ligature as its two
/// letters, then the terminator.
struct Characters<'a> {
    text: &'a [u8],
    at: usize,
    pending: Option<u8>,
    ended: bool,
}

impl<'a> Characters<'a> {
    const fn of(text: &'a [u8]) -> Self {
        Self {
            text,
            at: 0,
            pending: None,
            ended: false,
        }
    }

    /// The run of digits the digit just read begins, read to its end: its
    /// significant digits, those after any leading noughts. A digit is
    /// never a ligature's, so it is the byte just passed.
    fn number(&mut self) -> &'a [u8] {
        let start = self.at.saturating_sub(1);
        let tail = self.text.get(self.at..).unwrap_or_default();
        self.at += tail
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(tail.len());
        let run = self.text.get(start..self.at).unwrap_or_default();
        let significant = run.iter().position(|&b| b != b'0').unwrap_or(run.len());
        run.get(significant..).unwrap_or_default()
    }
}

impl Iterator for Characters<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if let Some(second) = self.pending.take() {
            return Some(second);
        }
        if self.ended {
            return None;
        }
        let Some(&c) = self.text.get(self.at) else {
            self.ended = true;
            return Some(0);
        };
        self.at += 1;
        match LIGATURES.iter().find(|&&(ligature, _)| ligature == c) {
            Some(&(_, [first, second])) => {
                self.pending = Some(second);
                Some(first)
            }
            None => Some(c),
        }
    }
}

/// The passes `flags` asks for, in order.
fn passes(flags: u32) -> impl Iterator<Item = Pass> {
    let accents = flags & IGNORE_ACCENTS == 0;
    let case = if accents {
        Pass::Case
    } else {
        Pass::UnaccentedCase
    };
    [
        Some(Pass::Letters),
        accents.then_some(Pass::Accents),
        (flags & IGNORE_CASE == 0).then_some(case),
    ]
    .into_iter()
    .flatten()
}

/// One pass over two strings, digit runs read as numbers when `numeric`:
/// the first difference.
fn compare_pass(left: &[u8], right: &[u8], pass: Pass, numeric: bool) -> i32 {
    let (mut lefts, mut rights) = (Characters::of(left), Characters::of(right));
    loop {
        let (Some(one), Some(other)) = (lefts.next(), rights.next()) else {
            return 0;
        };
        if numeric && one.is_ascii_digit() && other.is_ascii_digit() {
            let (first, second) = (lefts.number(), rights.number());
            if first.len() != second.len() {
                return i32::try_from(first.len()).unwrap_or(i32::MAX)
                    - i32::try_from(second.len()).unwrap_or(i32::MAX);
            }
            if let Some((&p, &q)) = first.iter().zip(second).find(|(p, q)| p != q) {
                return i32::from(pass.weighed(p)) - i32::from(pass.weighed(q));
            }
            continue;
        }
        let difference = i32::from(pass.weighed(one)) - i32::from(pass.weighed(other));
        if difference != 0 {
            return difference;
        }
        if one == 0 {
            return 0;
        }
    }
}

/// How `a` collates against `b`: negative before, zero equal, positive after.
pub(crate) fn collate(a: &[u8], b: &[u8], flags: u32) -> i32 {
    let numeric = flags & NUMERIC != 0;
    passes(flags)
        .map(|pass| compare_pass(a, b, pass, numeric))
        .find(|&difference| difference != 0)
        .unwrap_or(0)
}

/// `Territory_TransformString`'s form of `text`, added to `out`: each pass's
/// weights, a one or a two escaped by a two, and a one after each pass.
pub(crate) fn transform(text: &[u8], out: &mut alloc::vec::Vec<u8>) {
    for pass in [Pass::Letters, Pass::Accents, Pass::Case] {
        for c in Characters::of(text).take_while(|&c| c != 0) {
            let weight = pass.weighed(c);
            if matches!(weight, 1 | 2) {
                out.push(2);
            }
            out.push(weight);
        }
        out.push(1);
    }
}

#[cfg(test)]
mod tests;

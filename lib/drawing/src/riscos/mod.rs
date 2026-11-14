//! RISC OS Draw files (file type `&AFF`), read into a drawing and written
//! from one.
//!
//! A Draw file measures in draw units, 640 to the point, upward from its
//! paper's bottom-left corner. Its objects are paths, text and transformed
//! text, sprites, groups, tagged objects and text areas, an options object
//! holds its paper, and a font table names its fonts. RISC OS's own font
//! families map to the drawing's by their kind: Trinity is serif, Homerton
//! sans-serif and Corpus monospace. The format is the one the RISC OS
//! Programmer's Reference Manual sets out (volume 4, appendix E), its paths
//! read as the Draw module strokes and fills them (volume 3, chapter 63).

mod read;
mod write;

use alloc::string::String;

use tairix_collections::SharedStr;
use tairix_colour::{Rgb, Rgba};

use crate::drawing::{Paper, PAPER};
use crate::ReadError;

pub use read::read;
pub use write::{write, Dropped, Written};

/// Draw units to the drawing's pixel: 640 to the point, 72 points and 96
/// pixels to the inch.
const UNITS_PER_PIXEL: f64 = 480.0;

/// What every Draw file begins with.
const MAGIC: &[u8; 4] = b"Draw";

/// The format's major version, the one RISC OS writes and reads.
const MAJOR_VERSION: u32 = 201;

/// The header's length, in bytes: the magic, the two versions, the program's
/// name and the drawing's bounds.
const HEADER: usize = 40;

/// An object's header, in bytes: its type, its size and its bounds.
const OBJECT_HEADER: usize = 24;

/// A transformed object's matrix, in bytes: four entries of sixteen bits of
/// fraction, then a translation in draw units.
const MATRIX: usize = 24;

/// The colour word that stands for no colour at all.
const NO_COLOUR: u32 = 0xFFFF_FFFF;

/// Object types.
const FONT_TABLE: u32 = 0;
const TEXT: u32 = 1;
const PATH: u32 = 2;
const SPRITE: u32 = 5;
const GROUP: u32 = 6;
const TAGGED: u32 = 7;
const TEXT_AREA: u32 = 9;
const TEXT_COLUMN: u32 = 10;
const OPTIONS: u32 = 11;
const TRANSFORMED_TEXT: u32 = 12;
const TRANSFORMED_SPRITE: u32 = 13;
const JPEG: u32 = 16;

/// Path components, by the tag in a component word's low byte.
const END: u32 = 0;
const MOVE: u32 = 2;
/// The sub-path closed, its closing edge left undrawn.
const CLOSE_WITH_GAP: u32 = 4;
const CLOSE: u32 = 5;
const CURVE: u32 = 6;
/// A segment edging a fill that a stroke leaves undrawn.
const GAP: u32 = 7;
const LINE: u32 = 8;

/// The bits of a component word its tag is in.
const TAG: u32 = 0xFF;

/// The two bits a path style holds a join or a cap in.
const CODE: u32 = 0b11;

/// Joins, as a path style's low two bits hold them.
const JOIN_MITRE: u32 = 0;
const JOIN_ROUND: u32 = 1;
const JOIN_BEVEL: u32 = 2;

/// Caps, as a path style holds each end's.
const CAP_BUTT: u32 = 0;
const CAP_ROUND: u32 = 1;
const CAP_SQUARE: u32 = 2;
const CAP_TRIANGLE: u32 = 3;

/// Where a path style holds each of its fields.
const END_CAP_SHIFT: u32 = 2;
const START_CAP_SHIFT: u32 = 4;
const WINDING_EVEN_ODD: u32 = 1 << 6;
const DASHED: u32 = 1 << 7;
const TRIANGLE_WIDTH_SHIFT: u32 = 16;
const TRIANGLE_LENGTH_SHIFT: u32 = 24;

/// The bits a path style holds each of a triangle cap's sizes in.
const TRIANGLE_SIZE: u32 = 0xFF;

/// A triangle cap is measured in sixteenths of its line's width, its breadth
/// on each side of the line.
const TRIANGLE_UNIT: f64 = 16.0;

/// The mitre limit every Draw file is stroked with: PostScript's default,
/// which the format fixes.
const MITRE_LIMIT: f64 = 10.0;

/// A group's name, padded with spaces, in bytes.
const GROUP_NAME: usize = 12;

/// One in a transformed text's matrix, which holds sixteen bits of fraction.
const FIXED_ONE: f64 = 65536.0;

/// The bits of a text style its font number is in.
const FONT_NUMBER: u32 = 0xFF;

/// A transformed text set from right to left.
const RIGHT_TO_LEFT: u32 = 1 << 1;

/// The options object's size, in bytes.
const OPTIONS_SIZE: u32 = 88;

/// The options object's paper limits: shown, and the paper turned.
const PAPER_LIMITS_SHOWN: u32 = 1;
const LANDSCAPE: u32 = 1 << 4;

/// The A-series paper a Draw file's options can name, the largest number
/// RISC OS's Draw offers.
const MOST_A_PAPER: u32 = 5;

/// The generic family RISC OS's system font is set in, as Corpus is.
const MONOSPACE: &str = "monospace";

/// The generic family a font with no RISC OS family of its own falls back
/// to, as Homerton is.
const SANS_SERIF: &str = "sans-serif";

/// RISC OS's own font families: each with the generic kind the drawing names
/// it by, and the style its slanted faces are named with.
const FAMILIES: [(&str, &str, &str); 3] = [
    ("Trinity", "serif", ".Italic"),
    ("Homerton", SANS_SERIF, ".Oblique"),
    ("Corpus", MONOSPACE, ".Oblique"),
];

/// Whether `bytes` begin as a Draw file does.
#[must_use]
pub fn recognised(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// The generic family RISC OS's own font family `family` is set in —
/// Trinity serif, Homerton sans-serif, Corpus monospace — matched ignoring
/// case, or `None` for a family RISC OS does not ship.
#[must_use]
pub fn riscos_family_generic(family: &str) -> Option<&'static str> {
    FAMILIES
        .iter()
        .find(|(own, _, _)| own.eq_ignore_ascii_case(family))
        .map(|&(_, kind, _)| kind)
}

/// The colour a Draw colour word holds, or none.
const fn colour_of(word: u32) -> Option<Rgba> {
    if word == NO_COLOUR {
        return None;
    }
    Some(Rgb::from_riscos_word(word).opaque())
}

/// `colour` as a Draw colour word, its alpha set aside.
const fn word_of(colour: Rgba) -> u32 {
    Rgb::new(colour.r, colour.g, colour.b).riscos_word()
}

/// `value` as the two words RISC OS's floating point stores a double in,
/// first to last: the word holding its sign and exponent first.
fn fpa_words(value: f64) -> [u32; 2] {
    let bits = value.to_bits();
    let word = |bits: u64| u32::try_from(bits & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    [word(bits >> 32), word(bits)]
}

/// The double two words stored so hold.
fn fpa_double([high, low]: [u32; 2]) -> f64 {
    f64::from_bits(u64::from(high) << 32 | u64::from(low))
}

/// The A-series paper numbered `n`, A0 being the largest.
fn a_paper(n: u32) -> Option<&'static Paper> {
    PAPER.iter().find(|paper| {
        paper
            .name
            .strip_prefix('A')
            .and_then(|digits| digits.parse::<u32>().ok())
            == Some(n)
    })
}

/// How a RISC OS font sets text: the drawing's family list for it, its
/// weight and its posture.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Face {
    family: SharedStr,
    weight: u16,
    italic: bool,
}

/// The face a RISC OS font name — a family and its styles, dot apart, as in
/// `Trinity.Bold.Italic` — sets: one of RISC OS's own families by its kind,
/// any other by its name with sans-serif behind it. A character that would
/// break the family list's spelling is left out of the name.
fn face_of(name: &str) -> Result<Face, ReadError> {
    let mut parts = name.split('.').map(str::trim);
    let family = parts.next().unwrap_or("");
    let mut list = String::new();
    if list
        .try_reserve(family.len() + SANS_SERIF.len() + 2)
        .is_err()
    {
        return Err(ReadError::OutOfMemory);
    }
    if let Some(kind) = riscos_family_generic(family) {
        list.push_str(kind);
    } else {
        list.extend(family.chars().filter(|&c| !matches!(c, ',' | '"' | '\'')));
        if list.trim().is_empty() {
            list.clear();
        } else {
            list.push_str(", ");
        }
        list.push_str(SANS_SERIF);
    }
    let mut face = Face {
        family: SharedStr::try_from_string(list)?,
        weight: 400,
        italic: false,
    };
    for style in parts {
        if let Some(&(_, weight)) = WEIGHTS
            .iter()
            .find(|(names, _)| names.iter().any(|name| name.eq_ignore_ascii_case(style)))
        {
            face.weight = weight;
        }
        face.italic |= SLANTS.iter().any(|name| name.eq_ignore_ascii_case(style));
    }
    Ok(face)
}

/// The styles a RISC OS font's name gives its weight by, with the weight.
const WEIGHTS: [(&[&str], u16); 6] = [
    (&["thin"], 100),
    (&["light"], 300),
    (&["demi", "demibold", "semibold"], 600),
    (&["bold"], 700),
    (&["extrabold", "ultrabold"], 800),
    (&["black", "heavy", "ultra"], 900),
];

/// The styles a RISC OS font's name slants it by.
const SLANTS: [&str; 3] = ["italic", "oblique", "slanted"];

/// One of RISC OS's own fonts: a family of [`FAMILIES`], bold or not and
/// slanted or not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Font {
    family: usize,
    bold: bool,
    italic: bool,
}

impl Font {
    /// The font a face is set in — the family of the first entry of its
    /// family list RISC OS has a family of that name or kind for, Homerton
    /// where none is, bold from 600 up — and whether it was the list's first
    /// entry, so no family the list asked for first is lost.
    fn of(family: &str, weight: u16, italic: bool) -> (Self, bool) {
        let mut entries = family
            .split(',')
            .map(|name| name.trim().trim_matches(|c| c == '"' || c == '\''));
        let matching = |name: &str| {
            FAMILIES.iter().position(|(own, kind, _)| {
                own.eq_ignore_ascii_case(name) || kind.eq_ignore_ascii_case(name)
            })
        };
        let first = entries.next().and_then(matching);
        let own = first
            .or_else(|| entries.find_map(matching))
            .unwrap_or_else(|| {
                FAMILIES
                    .iter()
                    .position(|(_, kind, _)| *kind == SANS_SERIF)
                    .unwrap_or(0)
            });
        let font = Self {
            family: own,
            bold: weight >= 600,
            italic,
        };
        (font, first.is_some())
    }

    /// Its name, as a font table spells it — `Homerton.Bold.Oblique` — in
    /// the pieces it is written in.
    fn spelling(self) -> [&'static str; 3] {
        let (family, _, slant) = FAMILIES[self.family];
        [
            family,
            if self.bold { ".Bold" } else { ".Medium" },
            if self.italic { slant } else { "" },
        ]
    }
}

#[cfg(test)]
mod tests;

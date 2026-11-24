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

pub mod format;
mod read;
mod write;

use alloc::string::String;

use tairix_collections::SharedStr;
use tairix_colour::{Rgb, Rgba};

use crate::drawing::{Paper, PAPER};
use crate::ReadError;

use format::{MAGIC, NO_COLOUR};

pub use read::read;
pub use write::{write, Dropped, Written};

/// Draw units to the drawing's pixel: 640 to the point, 72 points and 96
/// pixels to the inch.
const UNITS_PER_PIXEL: f64 = 480.0;

/// A triangle cap is measured in sixteenths of its line's width, its breadth
/// on each side of the line.
const TRIANGLE_UNIT: f64 = 16.0;

/// The mitre limit every Draw file is stroked with: PostScript's default,
/// which the format fixes.
const MITRE_LIMIT: f64 = 10.0;

/// One in a transformed text's matrix, which holds sixteen bits of fraction.
const FIXED_ONE: f64 = 65536.0;

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
/// it by, and the slant its slanted faces have.
const FAMILIES: [(&str, &str, Slant); 3] = [
    ("Trinity", "serif", Slant::Italic),
    ("Homerton", SANS_SERIF, Slant::Oblique),
    ("Corpus", MONOSPACE, Slant::Oblique),
];

/// How a family's slanted faces slant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slant {
    /// Cursive letterforms of their own.
    Italic,
    /// The upright letterforms leaned.
    Oblique,
}

impl Slant {
    /// The style a font's name gives a face with this slant.
    const fn style(self) -> &'static str {
        match self {
            Self::Italic => ".Italic",
            Self::Oblique => ".Oblique",
        }
    }
}

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
    let (weight, italic) = styles(parts);
    Ok(Face {
        family: SharedStr::try_from_string(list)?,
        weight,
        italic,
    })
}

/// The weight and slant a font name's styles — the parts past its family —
/// give it: regular and upright unless a style says otherwise.
fn styles<'a>(parts: impl Iterator<Item = &'a str>) -> (u16, bool) {
    let mut weight = 400;
    let mut italic = false;
    for style in parts {
        if let Some(&(_, named)) = WEIGHTS
            .iter()
            .find(|(names, _)| names.iter().any(|name| name.eq_ignore_ascii_case(style)))
        {
            weight = named;
        }
        italic |= SLANTS.iter().any(|name| name.eq_ignore_ascii_case(style));
    }
    (weight, italic)
}

/// One of RISC OS's own fonts as the face it is set in: its family's
/// generic kind, its weight, and its slant, if it slants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiscosFace {
    /// The generic family, as [`riscos_family_generic`] gives it.
    pub generic: &'static str,
    /// The weight, from 100 to 900.
    pub weight: u16,
    /// Its family's slant, where it slants.
    pub slant: Option<Slant>,
}

/// The face RISC OS's own font `name` — a family and its styles, dot apart,
/// as in `Homerton.Bold.Oblique` — is set in; none for a family RISC OS does
/// not ship.
#[must_use]
pub fn riscos_face(name: &str) -> Option<RiscosFace> {
    let family = name.split('.').next()?.trim();
    let &(_, generic, slant) = FAMILIES
        .iter()
        .find(|(own, _, _)| own.eq_ignore_ascii_case(family))?;
    let (weight, slanted) = riscos_styles(name);
    Some(RiscosFace {
        generic,
        weight,
        slant: slanted.then_some(slant),
    })
}

/// The weight, from 100 to 900, and whether it slants, that the styles of
/// RISC OS font `name` — the parts past its family, as in
/// `Homerton.Bold.Oblique` — give it, whatever its family.
#[must_use]
pub fn riscos_styles(name: &str) -> (u16, bool) {
    styles(name.split('.').map(str::trim).skip(1))
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
            if self.italic { slant.style() } else { "" },
        ]
    }
}

#[cfg(test)]
mod tests;

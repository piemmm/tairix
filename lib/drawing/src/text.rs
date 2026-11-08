//! A text object: its content in its own style, spans of it set in runs of
//! their own, from a baseline origin in lines or along a path.
//!
//! The drawing holds text as text — its content and how it is set — never as
//! glyphs: glyph outlines come from the font service, and a renderer is
//! handed them for each text object it draws. Until they are known, a text
//! object's bounds are estimated generously from its faces, and no character
//! is set wider, or inks further, than the estimate allows, so culling by
//! bounds leaves no text out.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_bezier::{Affine, Bounds, FillRule, Path, Point};
use tairix_collections::{SharedSlice, SharedStr};
use tairix_util::mathf::fmin;

use crate::limits::{self, MAX_FAMILY_BYTES, MAX_TEXT_BYTES, MAX_TEXT_PATH_NODES};
use crate::shape::PathShape;
use crate::DrawingError;

mod layout;
mod runs;

pub use layout::{Caret, Painted, Pen, TextFace};
pub(crate) use runs::canonical;
pub use runs::{Run, RunPaint, RunStyle};

/// The advance a character whose glyph is not yet known is laid out with, in
/// ems: about a typical letter's.
const UNKNOWN_ADVANCE: f64 = 0.6;

/// How many spaces a tab advances by.
pub(crate) const TAB_SPACES: u8 = 4;

/// The distance between baselines a text is set with until it is told
/// another, in ems.
pub const DEFAULT_LINE_HEIGHT: f64 = 1.25;

/// The family a text names when it is told none: CSS's generic sans-serif.
pub const DEFAULT_FAMILY: &str = "sans-serif";

/// The widest a character is set, in ems, and so what the estimated bounds
/// allow each one.
const MOST_ADVANCE: f64 = 1.25;

/// How far, in ems, a glyph's ink may reach beside its advance's box, above
/// its baseline and below it: what the estimated bounds allow, and what a
/// glyph reaching further is drawn scaled down to.
const INK_BESIDE: f64 = 0.5;
const INK_ABOVE: f64 = 1.5;
const INK_BELOW: f64 = 0.5;

/// One character's outline, in ems from its pen position on the baseline,
/// y down, and how far the pen moves after it.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    outline: Path,
    advance: f64,
    /// What the outline inks, worked out once.
    ink: Bounds,
}

impl Glyph {
    /// The glyph of `outline`, filled non-zero, moving the pen `advance`
    /// ems on.
    #[must_use]
    pub fn new(outline: Path, advance: f64) -> Self {
        let ink = outline.bounds();
        Self {
            outline,
            advance,
            ink,
        }
    }

    /// Its outline, filled non-zero.
    #[must_use]
    pub const fn outline(&self) -> &Path {
        &self.outline
    }

    /// How far it moves the pen, in ems.
    #[must_use]
    pub const fn advance(&self) -> f64 {
        self.advance
    }

    /// What its outline inks.
    #[must_use]
    pub const fn ink(&self) -> Bounds {
        self.ink
    }

    /// The scale about its pen at which it, set `advance` ems wide, inks only
    /// where the estimated bounds allow: one for a glyph within its em, less
    /// for one reaching past.
    fn fitting(&self, advance: f64) -> f64 {
        if self.ink.is_empty() {
            return 1.0;
        }
        let reaches = [
            (self.ink.max.x, advance + INK_BESIDE),
            (-self.ink.min.x, INK_BESIDE),
            (-self.ink.min.y, INK_ABOVE),
            (self.ink.max.y, INK_BELOW),
        ];
        reaches
            .into_iter()
            .filter(|&(reach, allowed)| reach > allowed)
            .fold(1.0, |fit, (reach, allowed)| fmin(fit, allowed / reach))
    }
}

/// Where glyphs come from: the font service, through a cache, or a test.
pub trait Glyphs {
    /// `c` in the face of `family` nearest `weight` and the posture asked
    /// for, or `None` while it is not known.
    fn glyph(&self, family: &str, weight: u16, italic: bool, c: char) -> Option<Arc<Glyph>>;

    /// The characters of `text` are about to be asked for in `family` at
    /// `weight` and posture: a source that must fetch them may fetch them
    /// together, rather than one at a time as each is asked for.
    fn prepare(&self, _family: &str, _weight: u16, _italic: bool, _text: &str) {}
}

/// One glyph as text sets it.
#[derive(Clone, Debug)]
pub struct Placed {
    /// The glyph.
    pub glyph: Arc<Glyph>,
    /// The map from its ems into the text's own space.
    pub at: Affine,
    /// The index of the run it is set in, `None` for the text's own style.
    pub run: Option<u32>,
    /// Where its character starts in the content, in bytes.
    pub offset: usize,
}

/// What setting text came to.
#[derive(Clone, Debug, Default)]
pub struct Setting {
    /// Its glyphs, in order.
    pub glyphs: Vec<Placed>,
    /// Whether every glyph it needs was known.
    pub complete: bool,
}

impl Setting {
    /// The outlines of the glyphs `keep` keeps, in the text's own space, as
    /// one path.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the path cannot be held.
    pub fn outline(&self, keep: impl FnMut(&Placed) -> bool) -> Result<Path, DrawingError> {
        self.outline_of(0..self.glyphs.len(), keep)
    }

    /// The outlines of the glyphs of `range` that `keep` keeps, in the text's
    /// own space, as one path.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the path cannot be held.
    pub fn outline_of(
        &self,
        range: Range<usize>,
        mut keep: impl FnMut(&Placed) -> bool,
    ) -> Result<Path, DrawingError> {
        let mut path = Path::new();
        let glyphs = self.glyphs.get(range).unwrap_or_default();
        for placed in glyphs.iter().filter(|placed| keep(placed)) {
            let outline = placed.glyph.outline();
            path.subpaths.try_reserve(outline.subpaths.len())?;
            for subpath in &outline.subpaths {
                path.subpaths.push(subpath.transformed(placed.at)?);
            }
        }
        Ok(path)
    }

    /// Whether its glyph outlines, filled non-zero, cover `point` in the
    /// text's own space. Each glyph is asked in its own space, so nothing is
    /// copied; a placement that mirrors a glyph turns its winding round.
    pub(crate) fn covers(&self, point: Point) -> bool {
        let mut winding = 0_i64;
        for placed in &self.glyphs {
            let Some(back) = placed.at.invert() else {
                continue;
            };
            let turns = placed.glyph.outline().winding(back.map(point));
            winding += if placed.at.determinant() < 0.0 {
                -turns
            } else {
                turns
            };
        }
        FillRule::NonZero.inside(winding)
    }
}

/// Where each line stands against its alignment point: the origin's x, or
/// the start along a path.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextAlign {
    /// Each line starts there.
    #[default]
    Start,
    /// Each line is centred on it.
    Middle,
    /// Each line ends there.
    End,
}

/// Where a text's glyphs stand.
#[derive(Clone, Debug, PartialEq)]
pub enum Baseline {
    /// Lines set from the first line's baseline at `origin`, each the text's
    /// own size times `line_height` below the last.
    Lines {
        /// The first line's baseline, where the alignment places it.
        origin: Point,
        /// The distance between baselines, in ems of the text's own size.
        line_height: f64,
    },
    /// One line set along a path.
    Path(Arc<TextPath>),
}

/// The path a text is set along.
#[derive(Clone, Debug, PartialEq)]
pub struct TextPath {
    /// The path, in the text's own space.
    pub path: Arc<PathShape>,
    /// How far along the path the alignment point stands, in the object's
    /// units.
    pub start: f64,
}

/// A text object.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    /// The text, its lines separated by line feeds.
    pub content: SharedStr,
    /// The font families asked for, in order, as CSS writes a list.
    pub family: SharedStr,
    /// The font size: the em, in the object's units.
    pub size: f64,
    /// The weight, 1–1000, 400 regular and 700 bold.
    pub weight: u16,
    /// Whether it is set italic.
    pub italic: bool,
    /// Extra space after each character, in the object's units.
    pub letter_spacing: f64,
    /// How each line stands against its alignment point.
    pub align: TextAlign,
    /// Where its glyphs stand.
    pub baseline: Baseline,
    /// Spans of the content set otherwise than its own style.
    pub runs: SharedSlice<Run>,
}

impl Text {
    /// Whether every value is one a drawing may hold.
    ///
    /// # Errors
    ///
    /// [`DrawingError::TooComplex`] for content, a family list, runs or a
    /// path past their bounds, and [`DrawingError::Invalid`] for any other
    /// value outside its range, a control character in the content other than
    /// a line feed or a tab, or a line feed in text set along a path.
    pub fn check(&self) -> Result<(), DrawingError> {
        if self.content.len() > MAX_TEXT_BYTES || self.family.len() > MAX_FAMILY_BYTES {
            return Err(DrawingError::TooComplex);
        }
        let printable = self
            .content
            .chars()
            .all(|c| c == '\n' || c == '\t' || !c.is_control());
        let family_clean = !self.family.chars().any(char::is_control);
        let placed = match &self.baseline {
            Baseline::Lines {
                origin,
                line_height,
            } => {
                line_height.is_finite()
                    && (0.0..=100.0).contains(line_height)
                    && limits::point(*origin)
            }
            Baseline::Path(along) => {
                if along.path.node_count() > MAX_TEXT_PATH_NODES {
                    return Err(DrawingError::TooComplex);
                }
                along.path.check()?;
                limits::coordinate(along.start) && !self.content.contains('\n')
            }
        };
        let valid = printable
            && family_clean
            && limits::length(self.size)
            && self.size > 0.0
            && (1..=1000).contains(&self.weight)
            && limits::coordinate(self.letter_spacing)
            && placed;
        if !valid {
            return Err(DrawingError::Invalid);
        }
        self.check_runs()
    }

    /// Its lines, in order.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.content.split('\n')
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;

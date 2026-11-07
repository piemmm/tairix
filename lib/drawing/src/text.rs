//! A text object: lines of one style set from a baseline origin.
//!
//! The drawing holds text as text — its content and how it is set — never as
//! glyphs: glyph outlines come from the font service, and a renderer is
//! handed them for each text object it draws. Until they are known, a text
//! object's bounds are estimated generously from its size, and no character
//! is set wider, or inks further, than the estimate allows, so culling by
//! bounds leaves no text out.

use alloc::sync::Arc;

use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Path, Point};
use tairix_util::fallible;
use tairix_util::mathf::{fmax, fmin};

use crate::limits::{self, MAX_FAMILY_BYTES, MAX_TEXT_BYTES};
use crate::DrawingError;

/// The advance a character whose glyph is not yet known is laid out with, in
/// ems: about a typical letter's.
const UNKNOWN_ADVANCE: f64 = 0.6;

/// How many spaces a tab advances by.
const TAB_SPACES: f64 = 4.0;

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
}

/// What setting text came to.
#[derive(Clone, Debug, Default)]
pub struct Setting {
    /// Its glyphs, in order.
    pub glyphs: Vec<Placed>,
    /// Whether every glyph it needs was known.
    pub complete: bool,
}

/// Where each line stands against the origin's x.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextAlign {
    /// Each line starts at the origin.
    #[default]
    Start,
    /// Each line is centred on the origin.
    Middle,
    /// Each line ends at the origin.
    End,
}

/// A text object.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    /// The text, its lines separated by line feeds.
    pub content: Arc<str>,
    /// The font families asked for, in order, as CSS writes a list.
    pub family: Arc<str>,
    /// The font size: the em, in the object's units.
    pub size: f64,
    /// The weight, 1–1000, 400 regular and 700 bold.
    pub weight: u16,
    /// Whether it is set italic.
    pub italic: bool,
    /// Extra space after each character, in the object's units.
    pub letter_spacing: f64,
    /// The distance between baselines, in ems.
    pub line_height: f64,
    /// How each line stands against the origin.
    pub align: TextAlign,
    /// The first line's baseline, where the alignment places it.
    pub origin: Point,
}

impl Text {
    /// Whether every value is one a drawing may hold.
    ///
    /// # Errors
    ///
    /// [`DrawingError::TooComplex`] for content or a family list past their
    /// bounds, and [`DrawingError::Invalid`] for any other value outside its
    /// range or a control character in the content other than a line feed.
    pub fn check(&self) -> Result<(), DrawingError> {
        if self.content.len() > MAX_TEXT_BYTES || self.family.len() > MAX_FAMILY_BYTES {
            return Err(DrawingError::TooComplex);
        }
        let printable = self
            .content
            .chars()
            .all(|c| c == '\n' || c == '\t' || !c.is_control());
        let family_clean = !self.family.chars().any(char::is_control);
        let valid = printable
            && family_clean
            && limits::length(self.size)
            && self.size > 0.0
            && (1..=1000).contains(&self.weight)
            && limits::coordinate(self.letter_spacing)
            && self.line_height.is_finite()
            && (0.0..=100.0).contains(&self.line_height)
            && limits::point(self.origin);
        if valid {
            Ok(())
        } else {
            Err(DrawingError::Invalid)
        }
    }

    /// Its lines, in order.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.content.split('\n')
    }

    /// How far below the first baseline line `index`'s baseline lies.
    #[must_use]
    pub fn baseline_drop(&self, index: usize) -> f64 {
        let lines = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
        lines * self.line_height * self.size
    }

    /// Its glyphs, set line by line from its origin: each line aligned on the
    /// origin as asked, its characters `letter_spacing` apart.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the setting cannot be held.
    pub fn set(&self, glyphs: &dyn Glyphs) -> Result<Setting, DrawingError> {
        let mut setting = Setting {
            glyphs: Vec::new(),
            complete: true,
        };
        glyphs.prepare(&self.family, self.weight, self.italic, &self.content);
        let space = self.space(glyphs);
        let mut line_glyphs = Vec::new();
        for (index, line) in self.lines().enumerate() {
            setting.complete &= self.advances(glyphs, line, space, &mut line_glyphs)?;
            let mut pen = self.line_start(&line_glyphs);
            let baseline = self.origin.y + self.baseline_drop(index);
            if setting.glyphs.try_reserve(line_glyphs.len()).is_err() {
                return Err(DrawingError::OutOfMemory);
            }
            for (glyph, advance) in line_glyphs.drain(..) {
                if let Some(glyph) = glyph {
                    let scale = self.size * glyph.fitting(advance);
                    let at = Affine::scale(scale, scale).then(Affine::translate(pen, baseline));
                    setting.glyphs.push(Placed { glyph, at });
                }
                pen += advance * self.size + self.letter_spacing;
            }
        }
        Ok(setting)
    }

    /// Where the pen stands before the character at byte `at` of the
    /// content, on its line's baseline, the line set as [`set`](Self::set)
    /// sets it: where a caret there is drawn. A byte within a character
    /// stands for that character's start, and one past the end for the end.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the line cannot be measured.
    pub fn pen_at(&self, glyphs: &dyn Glyphs, at: usize) -> Result<Point, DrawingError> {
        let at = (0..=at.min(self.content.len()))
            .rev()
            .find(|&at| self.content.is_char_boundary(at))
            .unwrap_or(0);
        let before = &self.content[..at];
        let index = before.matches('\n').count();
        let line_begins = before.rfind('\n').map_or(0, |newline| newline + 1);
        let column = before[line_begins..].chars().count();
        let line = self.lines().nth(index).unwrap_or("");
        let mut advances = Vec::new();
        self.advances(glyphs, line, self.space(glyphs), &mut advances)?;
        let mut pen = self.line_start(&advances);
        for (_, advance) in advances.iter().take(column) {
            pen += advance * self.size + self.letter_spacing;
        }
        Ok(Point::new(pen, self.origin.y + self.baseline_drop(index)))
    }

    /// A space's advance, in ems.
    fn space(&self, glyphs: &dyn Glyphs) -> f64 {
        glyphs
            .glyph(&self.family, self.weight, self.italic, ' ')
            .map_or(UNKNOWN_ADVANCE, |glyph| bounded_advance(glyph.advance()))
    }

    /// Each character of `line` with its glyph and its advance in ems, into
    /// `out`: a tab as four of `space`, a character whose glyph is unknown as
    /// [`UNKNOWN_ADVANCE`]. Answers whether every glyph was known.
    fn advances(
        &self,
        glyphs: &dyn Glyphs,
        line: &str,
        space: f64,
        out: &mut Vec<(Option<Arc<Glyph>>, f64)>,
    ) -> Result<bool, DrawingError> {
        out.clear();
        if !fallible::reserve(out, line.chars().count()) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut complete = true;
        for c in line.chars() {
            if c == '\t' {
                out.push((None, space * TAB_SPACES));
                continue;
            }
            let glyph = glyphs.glyph(&self.family, self.weight, self.italic, c);
            complete &= glyph.is_some();
            let advance = glyph
                .as_ref()
                .map_or(UNKNOWN_ADVANCE, |glyph| bounded_advance(glyph.advance()));
            out.push((glyph, advance));
        }
        Ok(complete)
    }

    /// Where the pen starts a line of `advances`, the line aligned on the
    /// origin as asked.
    fn line_start(&self, advances: &[(Option<Arc<Glyph>>, f64)]) -> f64 {
        let gaps = f64::from(u32::try_from(advances.len().saturating_sub(1)).unwrap_or(u32::MAX));
        let width = advances
            .iter()
            .map(|(_, advance)| advance * self.size)
            .sum::<f64>()
            + gaps * self.letter_spacing;
        match self.align {
            TextAlign::Start => self.origin.x,
            TextAlign::Middle => self.origin.x - width / 2.0,
            TextAlign::End => self.origin.x - width,
        }
    }

    /// Bounds that hold every glyph it can draw, without knowing its glyphs:
    /// each character taken as the widest advance it can be set with, a tab
    /// as that many spaces, letter spacing of either sign, and the ink the
    /// setting allows a glyph about its advance, above the baseline and
    /// below it.
    #[must_use]
    pub fn estimated_bounds(&self) -> Bounds {
        let spacing = self.letter_spacing;
        let mut bounds = Bounds::EMPTY;
        for (index, line) in self.lines().enumerate() {
            let (characters, ems) = line.chars().fold((0.0, 0.0), |(count, ems), c| {
                let wide = if c == '\t' { TAB_SPACES } else { 1.0 };
                (count + 1.0, ems + wide * MOST_ADVANCE)
            });
            let gaps = fmax(characters - 1.0, 0.0);
            // The widest the line can be set, and how far spacing that draws
            // characters back can carry a pen behind where it began.
            let widest = ems * self.size + gaps * fmax(spacing, 0.0);
            let behind = gaps * fmin(spacing, 0.0);
            let leading = match self.align {
                TextAlign::Start => 0.0,
                TextAlign::Middle => 0.5,
                TextAlign::End => 1.0,
            };
            let beside = INK_BESIDE * self.size;
            let baseline = self.origin.y + self.baseline_drop(index);
            bounds = bounds.union(Bounds::new(
                Point::new(
                    self.origin.x - leading * widest + behind - beside,
                    baseline - INK_ABOVE * self.size,
                ),
                Point::new(
                    self.origin.x + (1.0 - leading) * widest - behind + beside,
                    baseline + INK_BELOW * self.size,
                ),
            ));
        }
        bounds
    }
}

/// A glyph's advance held to what the estimated bounds allow: a font
/// claiming more is set no wider.
fn bounded_advance(advance: f64) -> f64 {
    if advance.is_finite() {
        advance.clamp(0.0, MOST_ADVANCE)
    } else {
        UNKNOWN_ADVANCE
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use tairix_bezier::Point;

    use super::{Text, TextAlign};
    use crate::DrawingError;

    fn text(content: &str) -> Text {
        Text {
            content: Arc::from(content),
            family: Arc::from("sans-serif"),
            size: 10.0,
            weight: 400,
            italic: false,
            letter_spacing: 0.0,
            line_height: 1.2,
            align: TextAlign::Start,
            origin: Point::new(100.0, 100.0),
        }
    }

    #[test]
    fn estimated_bounds_hold_every_line() {
        let two = text("Hello\nWorld!");
        let bounds = two.estimated_bounds();
        assert!(bounds.min.y <= 100.0 - 10.0, "above the first baseline");
        assert!(bounds.max.y >= 112.0, "below the second");
        assert!(
            bounds.max.x >= 100.0 + 6.0 * 10.0,
            "as wide as the widest line"
        );
        let mut centred = text("Hello");
        centred.align = TextAlign::Middle;
        let centre = centred.estimated_bounds().centre();
        assert!((centre.x - 100.0).abs() < 1e-9, "centred on the origin");
    }

    #[test]
    fn estimated_bounds_hold_every_setting_of_every_glyph() {
        for content in ["\t\tW", "WWWW\n\tW", "abc", "a b\tc\nd"] {
            for spacing in [-20.0, -3.0, 0.0, 4.0] {
                for align in [TextAlign::Start, TextAlign::Middle, TextAlign::End] {
                    let mut wide = text(content);
                    wide.letter_spacing = spacing;
                    wide.align = align;
                    let estimate = wide.estimated_bounds().inflated(1e-9);
                    let set = wide.set(&Wide).expect("set");
                    assert!(!set.glyphs.is_empty());
                    for placed in &set.glyphs {
                        let inked = placed.glyph.ink().transformed(placed.at);
                        assert!(
                            estimate.contains_bounds(inked),
                            "{content:?} at {spacing} {align:?}: {inked:?} past {estimate:?}"
                        );
                    }
                }
            }
        }
    }

    /// Glyphs claiming three ems each and inking past every side of their
    /// em, as a hostile font might.
    struct Wide;

    impl crate::text::Glyphs for Wide {
        fn glyph(
            &self,
            _family: &str,
            _weight: u16,
            _italic: bool,
            _c: char,
        ) -> Option<Arc<super::Glyph>> {
            let mut ring = tairix_bezier::Subpath::new(Point::new(-0.9, -2.5));
            ring.elements.extend([
                tairix_bezier::PathEl::Line(Point::new(3.4, -2.5)),
                tairix_bezier::PathEl::Line(Point::new(3.4, 1.1)),
                tairix_bezier::PathEl::Line(Point::new(-0.9, 1.1)),
            ]);
            ring.closed = true;
            let outline = tairix_bezier::Path {
                subpaths: alloc::vec![ring],
            };
            Some(Arc::new(super::Glyph::new(outline, 3.0)))
        }
    }

    /// Every glyph an em wide but the narrow `i`, half one.
    struct Ems;

    impl crate::text::Glyphs for Ems {
        fn glyph(
            &self,
            _family: &str,
            _weight: u16,
            _italic: bool,
            c: char,
        ) -> Option<Arc<super::Glyph>> {
            (c != '?').then(|| {
                Arc::new(super::Glyph::new(
                    tairix_bezier::Path::new(),
                    if c == 'i' { 0.5 } else { 1.0 },
                ))
            })
        }
    }

    #[test]
    fn the_pen_stands_where_set_text_puts_the_next_character() {
        let mut two = text("ab\nhi c");
        two.letter_spacing = 1.0;
        let pen = |text: &Text, at| text.pen_at(&Ems, at).expect("measured");
        assert_eq!(pen(&two, 0), Point::new(100.0, 100.0));
        assert_eq!(
            pen(&two, 2),
            Point::new(100.0 + 2.0 * 11.0, 100.0),
            "the first line's end"
        );
        assert_eq!(
            pen(&two, 3),
            Point::new(100.0, 112.0),
            "the second line's start"
        );
        assert_eq!(
            pen(&two, 5),
            Point::new(100.0 + 10.0 + 1.0 + 5.0 + 1.0, 112.0)
        );
        assert_eq!(
            pen(&two, 99),
            pen(&two, two.content.len()),
            "past the end is the end"
        );
        let set = two.set(&Ems).expect("set");
        let c = set.glyphs.last().expect("the c").at.map(Point::ZERO);
        assert_eq!(c, pen(&two, two.content.len() - 1), "where set puts the c");
        let mut centred = text("ab");
        centred.align = TextAlign::Middle;
        assert_eq!(
            pen(&centred, 0),
            Point::new(90.0, 100.0),
            "half the line before the origin"
        );
        let unknown = text("é?");
        assert_eq!(
            pen(&unknown, 1),
            pen(&unknown, 0),
            "within a character is its start"
        );
        assert!(
            (pen(&unknown, 3).x - pen(&unknown, 2).x - 6.0).abs() < 1e-9,
            "an unknown glyph's advance"
        );
    }

    #[test]
    fn text_refuses_what_a_drawing_cannot_hold() {
        assert_eq!(text("ok\tfine").check(), Ok(()));
        assert_eq!(text("bell\u{7}").check(), Err(DrawingError::Invalid));
        let mut weightless = text("x");
        weightless.weight = 0;
        assert_eq!(weightless.check(), Err(DrawingError::Invalid));
        let long: alloc::string::String =
            core::iter::repeat_n('x', crate::limits::MAX_TEXT_BYTES + 1).collect();
        assert_eq!(text(&long).check(), Err(DrawingError::TooComplex));
    }
}

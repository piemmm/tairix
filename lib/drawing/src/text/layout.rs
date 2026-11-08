//! Setting a text: each character in its own face, lines from an origin or
//! one line along a path, where a caret stands, and the bounds that hold
//! every glyph before any is known.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_bezier::{Affine, Bounds, PathMeasure, Point};
use tairix_util::fallible;
use tairix_util::mathf::{fabs, fmax, fmin, hypot};

use super::{
    Baseline, Glyph, Glyphs, Placed, RunPaint, RunStyle, Setting, Text, TextAlign, TextPath,
    INK_ABOVE, INK_BELOW, INK_BESIDE, MOST_ADVANCE, TAB_SPACES, UNKNOWN_ADVANCE,
};
use crate::style::{Paint, Style};
use crate::DrawingError;

/// How far above its baseline a line's letters stand at their middle, in
/// ems: halfway between a common ascender and descender.
const LINE_MIDDLE: f64 = 0.3;

/// How one character is set: the text's own style with its run's overrides.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextFace<'a> {
    /// The font families asked for, in order.
    pub family: &'a str,
    /// The weight, 1–1000.
    pub weight: u16,
    /// Whether it is set italic.
    pub italic: bool,
    /// The font size, in the object's units.
    pub size: f64,
    /// Extra space after the character, in the object's units.
    pub spacing: f64,
    /// How far it is raised above the baseline, in the object's units.
    pub rise: f64,
    /// The index of the run setting it, `None` for the text's own style.
    pub run: Option<u32>,
}

impl TextFace<'_> {
    fn same_font(&self, other: &Self) -> bool {
        self.weight == other.weight && self.italic == other.italic && self.family == other.family
    }
}

/// One character as a line is laid out: its glyph, its advance in ems, its
/// face and where it starts in the content.
type Laid<'a> = (Option<Arc<Glyph>>, f64, TextFace<'a>, usize);

/// Where a caret before a character stands.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Caret {
    /// The pen, on the baseline.
    pub pen: Point,
    /// The unit direction the line runs in there.
    pub direction: Point,
    /// The size of the character it follows — of the one it precedes at a
    /// line's start — in the object's units.
    pub size: f64,
}

/// Where a line's pen stands before one of its characters.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pen<'a> {
    /// Where the character starts in the content, in bytes; the line's end
    /// for the pen past its last.
    pub offset: usize,
    /// The pen, on the baseline before any rise.
    pub point: Point,
    /// The unit direction the line runs in there.
    pub direction: Point,
    /// The face of the character — of the last for the pen past it.
    pub face: TextFace<'a>,
}

/// A run of a setting's glyphs painted alike: what the runs set of the fill
/// and the stroke, `None` where the object's own paints.
#[derive(Clone, Debug, PartialEq)]
pub struct Painted {
    /// The glyphs, as indices into [`Setting::glyphs`].
    pub glyphs: Range<usize>,
    /// What fills them in place of the object's fill.
    pub fill: Option<RunPaint>,
    /// What strokes them in place of the object's stroke paint.
    pub stroke: Option<RunPaint>,
}

impl Painted {
    /// `style` with these glyphs' paints in place of its own.
    #[must_use]
    pub fn style(&self, style: &Style) -> Style {
        repainted(style, self.fill, self.stroke)
    }
}

impl RunStyle {
    /// `style` with this run's paints in place of its own.
    #[must_use]
    pub fn painted(&self, style: &Style) -> Style {
        repainted(style, self.fill, self.stroke)
    }
}

/// `style` with `fill` and `stroke`, where given, in place of its own.
fn repainted(style: &Style, fill: Option<RunPaint>, stroke: Option<RunPaint>) -> Style {
    let mut painted = style.clone();
    if let Some(fill) = fill {
        painted.fill = fill.paint();
    }
    if let Some(stroke) = stroke {
        painted.stroke.paint = stroke.paint();
    }
    painted
}

impl Setting {
    /// Its glyphs in runs painted alike, in the order they are set: what a
    /// renderer paints one after the other.
    pub fn painted<'s>(&'s self, text: &'s Text) -> impl Iterator<Item = Painted> + 's {
        let paints = |placed: &Placed| {
            placed
                .run
                .and_then(|run| text.runs.get(run as usize))
                .map_or((None, None), |run| (run.style.fill, run.style.stroke))
        };
        let mut at = 0;
        core::iter::from_fn(move || {
            let first = self.glyphs.get(at)?;
            let (fill, stroke) = paints(first);
            let end = self.glyphs[at..]
                .iter()
                .position(|placed| paints(placed) != (fill, stroke))
                .map_or(self.glyphs.len(), |offset| at + offset);
            let painted = Painted {
                glyphs: at..end,
                fill,
                stroke,
            };
            at = end;
            Some(painted)
        })
    }
}

impl Text {
    /// The face of its own style.
    fn own_face(&self) -> TextFace<'_> {
        TextFace {
            family: &self.family,
            weight: self.weight,
            italic: self.italic,
            size: self.size,
            spacing: self.letter_spacing,
            rise: 0.0,
            run: None,
        }
    }

    /// The face run `index` sets its characters in, or its own style for
    /// `None`.
    #[must_use]
    pub fn face(&self, index: Option<u32>) -> TextFace<'_> {
        let own = self.own_face();
        let Some(run) = index.and_then(|index| self.runs.get(index as usize)) else {
            return own;
        };
        let style = &run.style;
        let size = style.size.unwrap_or(own.size);
        TextFace {
            family: style.family.as_deref().unwrap_or(own.family),
            weight: style.weight.unwrap_or(own.weight),
            italic: style.italic.unwrap_or(own.italic),
            size,
            spacing: style.letter_spacing.unwrap_or(own.spacing),
            rise: style.baseline_shift.unwrap_or(0.0) * size,
            run: index,
        }
    }

    /// Each character of `range` of the content, at its byte offset, with
    /// the face it is set in.
    pub fn faced(
        &self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, char, TextFace<'_>)> + '_ {
        let mut cursor = self.runs.partition_point(|run| run.end <= range.start);
        self.content[range.clone()]
            .char_indices()
            .map(move |(offset, c)| {
                let at = range.start + offset;
                while self.runs.get(cursor).is_some_and(|run| run.end <= at) {
                    cursor += 1;
                }
                let run = self
                    .runs
                    .get(cursor)
                    .filter(|run| run.holds(at))
                    .and_then(|_| u32::try_from(cursor).ok());
                (at, c, self.face(run))
            })
    }

    /// Each line's byte range in the content, in order: what
    /// [`pens`](Self::pens) measures a line by.
    pub fn line_ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        let mut start = 0;
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            let rest = &self.content[start..];
            let line = if let Some(end) = rest.find('\n') {
                start..start + end
            } else {
                done = true;
                start..self.content.len()
            };
            start = line.end + 1;
            Some(line)
        })
    }

    /// How far below the first baseline line `index`'s baseline lies: lines
    /// keep the text's own size and line height whatever their runs set.
    #[must_use]
    pub fn baseline_drop(&self, index: usize) -> f64 {
        match &self.baseline {
            Baseline::Lines { line_height, .. } => {
                let lines = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
                lines * line_height * self.size
            }
            Baseline::Path(_) => 0.0,
        }
    }

    /// How far before the alignment point a line starts, as a share of its
    /// width.
    const fn leading(&self) -> f64 {
        match self.align {
            TextAlign::Start => 0.0,
            TextAlign::Middle => 0.5,
            TextAlign::End => 1.0,
        }
    }

    /// Ask `glyphs` for each run of characters in one font together.
    fn prepare(&self, glyphs: &dyn Glyphs) {
        let mut segment: Option<(usize, TextFace<'_>)> = None;
        for (at, _, face) in self.faced(0..self.content.len()) {
            match segment {
                Some((_, held)) if held.same_font(&face) => {}
                Some((start, held)) => {
                    glyphs.prepare(
                        held.family,
                        held.weight,
                        held.italic,
                        &self.content[start..at],
                    );
                    segment = Some((at, face));
                }
                None => segment = Some((at, face)),
            }
        }
        if let Some((start, held)) = segment {
            glyphs.prepare(
                held.family,
                held.weight,
                held.italic,
                &self.content[start..],
            );
        }
    }

    /// Each character of `range` with its glyph, its advance in ems and its
    /// face, into `out`: a tab as four of its face's space, a character whose
    /// glyph is unknown as [`UNKNOWN_ADVANCE`]. Answers whether every glyph
    /// was known.
    fn laid<'t>(
        &'t self,
        glyphs: &dyn Glyphs,
        range: Range<usize>,
        out: &mut Vec<Laid<'t>>,
    ) -> Result<bool, DrawingError> {
        out.clear();
        if !fallible::reserve(out, self.content[range.clone()].chars().count()) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut complete = true;
        for (at, c, face) in self.faced(range) {
            if c == '\t' {
                let space = glyphs
                    .glyph(face.family, face.weight, face.italic, ' ')
                    .map_or(UNKNOWN_ADVANCE, |glyph| bounded_advance(glyph.advance()));
                out.push((None, space * f64::from(TAB_SPACES), face, at));
                continue;
            }
            let glyph = glyphs.glyph(face.family, face.weight, face.italic, c);
            complete &= glyph.is_some();
            let advance = glyph
                .as_ref()
                .map_or(UNKNOWN_ADVANCE, |glyph| bounded_advance(glyph.advance()));
            out.push((glyph, advance, face, at));
        }
        Ok(complete)
    }

    /// Its glyphs: each line aligned on the origin and set from it, or its
    /// one line along its path, every character in its own face and
    /// `letter_spacing` past the last.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the setting cannot be held.
    pub fn set(&self, glyphs: &dyn Glyphs) -> Result<Setting, DrawingError> {
        self.prepare(glyphs);
        let mut setting = Setting {
            glyphs: Vec::new(),
            complete: true,
        };
        let mut line = Vec::new();
        match &self.baseline {
            Baseline::Lines { origin, .. } => {
                for (index, range) in self.line_ranges().enumerate() {
                    setting.complete &= self.laid(glyphs, range, &mut line)?;
                    let mut pen = origin.x - self.leading() * width(&line);
                    let baseline = origin.y + self.baseline_drop(index);
                    if !fallible::reserve(&mut setting.glyphs, line.len()) {
                        return Err(DrawingError::OutOfMemory);
                    }
                    for (glyph, advance, face, offset) in line.drain(..) {
                        if let Some(glyph) = glyph {
                            let scale = face.size * glyph.fitting(advance);
                            let at = Affine::scale(scale, scale)
                                .then(Affine::translate(pen, baseline - face.rise));
                            setting.glyphs.push(Placed {
                                glyph,
                                at,
                                run: face.run,
                                offset,
                            });
                        }
                        pen += advance * face.size + face.spacing;
                    }
                }
            }
            Baseline::Path(along) => {
                let measure = along.measure()?;
                setting.complete &= self.laid(glyphs, 0..self.content.len(), &mut line)?;
                let mut pen = along.start - self.leading() * width(&line);
                if !fallible::reserve(&mut setting.glyphs, line.len()) {
                    return Err(DrawingError::OutOfMemory);
                }
                for (glyph, advance, face, offset) in line.drain(..) {
                    let wide = advance * face.size;
                    if let (Some(glyph), Some((point, direction))) =
                        (glyph, measure.at(pen + wide / 2.0))
                    {
                        let scale = face.size * glyph.fitting(advance);
                        let at = Affine::scale(scale, scale)
                            .then(Affine::translate(-wide / 2.0, -face.rise))
                            .then(turned(direction))
                            .then(Affine::translate(point.x, point.y));
                        setting.glyphs.push(Placed {
                            glyph,
                            at,
                            run: face.run,
                            offset,
                        });
                    }
                    pen += wide + face.spacing;
                }
            }
        }
        Ok(setting)
    }

    /// Where a caret before the character at byte `at` stands, the text set
    /// as [`set`](Self::set) sets it. A byte within a character stands for
    /// that character's start, and one past the end for the end; on a path,
    /// a caret past either end stands at that end.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the line cannot be measured.
    pub fn caret(&self, glyphs: &dyn Glyphs, at: usize) -> Result<Caret, DrawingError> {
        let at = self.boundary(at);
        let (index, range) = self
            .line_ranges()
            .enumerate()
            .find(|(_, range)| at <= range.end)
            .unwrap_or((0, 0..self.content.len()));
        let column = self.content[range.start..at].chars().count();
        let pens = self.pens(glyphs, index, range)?;
        let pen = pens[column.min(pens.len() - 1)];
        let size = column
            .checked_sub(1)
            .and_then(|before| pens.get(before))
            .map_or(pen.face.size, |before| before.face.size);
        Ok(Caret {
            pen: pen.point,
            direction: pen.direction,
            size,
        })
    }

    /// The pen before each character of line `index`, over `range` of the
    /// content, and last the pen past its end, the text set as
    /// [`set`](Self::set) sets it: on the baseline, before any rise, held to
    /// a path's ends.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the line cannot be measured.
    pub fn pens(
        &self,
        glyphs: &dyn Glyphs,
        index: usize,
        range: Range<usize>,
    ) -> Result<Vec<Pen<'_>>, DrawingError> {
        let mut line = Vec::new();
        self.laid(glyphs, range.clone(), &mut line)?;
        let mut pens = Vec::new();
        if !fallible::reserve(&mut pens, line.len() + 1) {
            return Err(DrawingError::OutOfMemory);
        }
        let start = -self.leading() * width(&line);
        let measure = match &self.baseline {
            Baseline::Path(along) => Some((along, along.measure()?)),
            Baseline::Lines { .. } => None,
        };
        let place = |distance: f64| match (&self.baseline, &measure) {
            (_, Some((along, measure))) => {
                let held = (along.start + start + distance).clamp(0.0, measure.length());
                measure
                    .at(held)
                    .unwrap_or((along.first_point(), Point::new(1.0, 0.0)))
            }
            (Baseline::Lines { origin, .. }, None) => (
                Point::new(
                    origin.x + start + distance,
                    origin.y + self.baseline_drop(index),
                ),
                Point::new(1.0, 0.0),
            ),
            (Baseline::Path(_), None) => (Point::ZERO, Point::new(1.0, 0.0)),
        };
        let mut along = 0.0;
        for &(_, advance, face, offset) in &line {
            let (point, direction) = place(along);
            pens.push(Pen {
                offset,
                point,
                direction,
                face,
            });
            along += advance * face.size + face.spacing;
        }
        let (point, direction) = place(along);
        pens.push(Pen {
            offset: range.end,
            point,
            direction,
            face: line.last().map_or_else(|| self.own_face(), |laid| laid.2),
        });
        Ok(pens)
    }

    /// The character boundary nearest `point`, in the text's own space, the
    /// text set as [`set`](Self::set) sets it: on the line whose letters
    /// stand nearest the point's height, the boundary nearest along it; on a
    /// path, the boundary whose pen is nearest.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the line cannot be measured.
    pub fn offset_at(&self, glyphs: &dyn Glyphs, point: Point) -> Result<usize, DrawingError> {
        let (index, range, across) = match &self.baseline {
            Baseline::Path(_) => (0, 0..self.content.len(), false),
            Baseline::Lines { origin, .. } => {
                let height = |index: usize| {
                    let middle = origin.y + self.baseline_drop(index) - LINE_MIDDLE * self.size;
                    fabs(middle - point.y)
                };
                let (index, range) = self
                    .line_ranges()
                    .enumerate()
                    .min_by(|(a, _), (b, _)| height(*a).total_cmp(&height(*b)))
                    .unwrap_or((0, 0..self.content.len()));
                (index, range, true)
            }
        };
        let pens = self.pens(glyphs, index, range)?;
        let distance = |pen: &Pen<'_>| {
            if across {
                fabs(pen.point.x - point.x)
            } else {
                pen.point.distance(point)
            }
        };
        Ok(pens
            .iter()
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .map_or(0, |pen| pen.offset))
    }

    /// Byte `at` moved back to a character boundary within the content.
    fn boundary(&self, at: usize) -> usize {
        (0..=at.min(self.content.len()))
            .rev()
            .find(|&at| self.content.is_char_boundary(at))
            .unwrap_or(0)
    }

    /// Bounds that hold every glyph it can draw, without knowing its glyphs:
    /// each character taken at the widest advance its face can be set with,
    /// a tab as that many spaces, its letter spacing of either sign and its
    /// rise, and the ink the setting allows a glyph about its advance, above
    /// the baseline and below it.
    #[must_use]
    pub fn estimated_bounds(&self) -> Bounds {
        match &self.baseline {
            Baseline::Lines { .. } => self
                .line_ranges()
                .enumerate()
                .fold(Bounds::EMPTY, |bounds, (index, range)| {
                    bounds.union(self.estimated_line_bounds(index, range))
                }),
            Baseline::Path(along) => {
                let reach = self
                    .faced(0..self.content.len())
                    .map(|(_, _, face)| {
                        let rise = face.rise.abs() / face.size;
                        face.size
                            * hypot(
                                MOST_ADVANCE / 2.0 + INK_BESIDE,
                                fmax(INK_ABOVE, INK_BELOW) + rise,
                            )
                    })
                    .fold(0.0, fmax);
                along.hull().inflated(reach)
            }
        }
    }

    /// What [`estimated_bounds`](Self::estimated_bounds) allows the line
    /// `index` over `range` of the content, the text set in lines.
    pub(crate) fn estimated_line_bounds(&self, index: usize, range: Range<usize>) -> Bounds {
        let own = self.own_face();
        let mut widest = 0.0;
        let mut behind = 0.0;
        let mut biggest = 0.0;
        let mut above = 0.0;
        let mut below = 0.0;
        let mut last_spacing = None;
        for (_, c, face) in self.faced(range) {
            if let Some(spacing) = last_spacing {
                widest += fmax(spacing, 0.0);
                behind += fmin(spacing, 0.0);
            }
            let wide = if c == '\t' {
                f64::from(TAB_SPACES)
            } else {
                1.0
            };
            widest += wide * MOST_ADVANCE * face.size;
            biggest = fmax(biggest, face.size);
            above = fmax(above, INK_ABOVE * face.size + fmax(face.rise, 0.0));
            below = fmax(below, INK_BELOW * face.size + fmax(-face.rise, 0.0));
            last_spacing = Some(face.spacing);
        }
        if last_spacing.is_none() {
            biggest = own.size;
            above = INK_ABOVE * own.size;
            below = INK_BELOW * own.size;
        }
        let origin = match &self.baseline {
            Baseline::Lines { origin, .. } => *origin,
            Baseline::Path(along) => along.first_point(),
        };
        let leading = self.leading();
        let beside = INK_BESIDE * biggest;
        let baseline = origin.y + self.baseline_drop(index);
        Bounds::new(
            Point::new(
                origin.x - leading * widest + behind - beside,
                baseline - above,
            ),
            Point::new(
                origin.x + (1.0 - leading) * widest - behind + beside,
                baseline + below,
            ),
        )
    }

    /// Its estimated bounds grown by how far its stroke can reach.
    #[must_use]
    pub fn painted_bounds(&self, style: &Style) -> Bounds {
        self.estimated_bounds().inflated(self.stroke_reach(style))
    }

    /// How far beyond its glyphs a stroke set as `style` sets it can reach:
    /// the object's, or a run's colour at the object's stroke width where the
    /// object's own stroke paints nothing.
    #[must_use]
    pub fn stroke_reach(&self, style: &Style) -> f64 {
        let reach = style.stroke.reach();
        let run_colour = self.runs.iter().find_map(|run| match run.style.stroke {
            Some(RunPaint::Colour(colour)) => Some(colour),
            Some(RunPaint::None) | None => None,
        });
        match run_colour {
            Some(colour) => {
                let mut stroke = style.stroke.clone();
                stroke.paint = Paint::Colour(colour);
                fmax(reach, stroke.reach())
            }
            None => reach,
        }
    }

    /// Whether a run paints any of its glyphs otherwise than the object.
    #[must_use]
    pub fn repaints(&self) -> bool {
        self.runs
            .iter()
            .any(|run| run.style.fill.is_some() || run.style.stroke.is_some())
    }
}

impl TextPath {
    /// Its path, measured along its length.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the path or its measurements cannot
    /// be held.
    pub fn measure(&self) -> Result<PathMeasure, DrawingError> {
        Ok(PathMeasure::new(&self.path.to_path()?)?)
    }

    /// Where its path begins.
    #[must_use]
    pub fn first_point(&self) -> Point {
        self.path
            .contours
            .iter()
            .find_map(|contour| contour.nodes.first())
            .map_or(Point::ZERO, |node| node.point)
    }

    /// Bounds of every node and handle of its path, which hold the path.
    #[must_use]
    pub fn hull(&self) -> Bounds {
        self.path
            .contours
            .iter()
            .flat_map(|contour| &contour.nodes)
            .flat_map(|node| [Some(node.point), node.before, node.after])
            .flatten()
            .fold(Bounds::EMPTY, |bounds, point| {
                bounds.union(Bounds::new(point, point))
            })
    }
}

/// How wide a laid line is: every advance, and the spacing between each
/// character and the next.
fn width(line: &[Laid<'_>]) -> f64 {
    let spaced: f64 = line
        .iter()
        .map(|(_, advance, face, _)| advance * face.size + face.spacing)
        .sum();
    spaced - line.last().map_or(0.0, |(_, _, face, _)| face.spacing)
}

/// The rotation that turns the x axis to the unit `direction`.
const fn turned(direction: Point) -> Affine {
    Affine {
        a: direction.x,
        b: direction.y,
        c: -direction.y,
        d: direction.x,
        e: 0.0,
        f: 0.0,
    }
}

/// A glyph's advance held to what the estimated bounds allow: a font
/// claiming more is set no wider.
pub(super) fn bounded_advance(advance: f64) -> f64 {
    if advance.is_finite() {
        advance.clamp(0.0, MOST_ADVANCE)
    } else {
        UNKNOWN_ADVANCE
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;

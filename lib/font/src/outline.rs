//! The font service's outline replies as owned values: the one place the
//! wire form becomes an owned one, which every client of the protocol reads
//! a reply through, so none can read a reply the others would read
//! differently.

use alloc::vec::Vec;

use tairix_abi::font_ipc::{decode_outlines_reply, GlyphOutline, GlyphSegment, OutlineBatch};
use tairix_bezier::offset::{grown, Join};
use tairix_bezier::{FillRule, Path, PathEl, Point, QuadBez, Subpath};

/// The longest a synthetic bold's round joins may reach, in strokes, and the
/// tolerance its curves are grown to, in ems.
const BOLD_MITER_LIMIT: f64 = 4.0;
const BOLD_TOLERANCE: f64 = 1e-3;

/// The metrics of the face a batch of outlines is drawn from, in its own
/// font units.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceMetrics {
    /// The em.
    pub units_per_em: u32,
    /// The ascender.
    pub ascent: i32,
    /// The descender's magnitude.
    pub descent: i32,
    /// The extra leading.
    pub line_gap: i32,
}

/// One decoded outline record, owned so it outlives the reply frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnedOutline {
    /// The resolved face's em.
    pub units_per_em: u32,
    /// The pen advance in that face's font units.
    pub advance: f64,
    /// A synthetic bold stroke the face could not furnish, as a fraction of
    /// the em.
    pub synthetic_bold: f64,
    /// A synthetic oblique shear the face could not furnish.
    pub synthetic_shear: f64,
    /// The closed contours, in font units with y up.
    pub contours: Vec<OwnedContour>,
}

impl OwnedOutline {
    /// The outline as a path in ems from the pen on the baseline, y up: its
    /// quadratics raised to cubics, and the oblique shear and the bold its
    /// face could not furnish applied, filled non-zero as TrueType fills.
    /// None for an outline of no em, or one the host has no room for.
    #[must_use]
    pub fn path(&self) -> Option<Path> {
        let em = f64::from(self.units_per_em);
        if em <= 0.0 {
            return None;
        }
        let shear = self.synthetic_shear;
        let place = |(x, y): (f64, f64)| Point::new((x + shear * y) / em, y / em);
        let mut path = Path::new();
        path.subpaths.try_reserve(self.contours.len()).ok()?;
        for contour in &self.contours {
            let mut subpath = Subpath::new(place(contour.start));
            subpath.elements.try_reserve(contour.segments.len()).ok()?;
            let mut from = subpath.start;
            for segment in &contour.segments {
                let element = match *segment {
                    OwnedSegment::Line { to } => PathEl::Line(place(to)),
                    OwnedSegment::Quadratic { control, to } => {
                        let curve = QuadBez::new(from, place(control), place(to)).raised();
                        PathEl::Cubic(curve.p1, curve.p2, curve.p3)
                    }
                };
                from = element.end();
                subpath.elements.push(element);
            }
            subpath.closed = true;
            path.subpaths.push(subpath);
        }
        if self.synthetic_bold > 0.0 {
            path = grown(
                &path,
                FillRule::NonZero,
                self.synthetic_bold / 2.0,
                Join::Round,
                BOLD_MITER_LIMIT,
                BOLD_TOLERANCE,
            )
            .ok()?;
        }
        Some(path)
    }

    /// The pen advance in ems; none for an outline of no em.
    #[must_use]
    pub fn advance_em(&self) -> Option<f64> {
        let em = f64::from(self.units_per_em);
        (em > 0.0).then(|| self.advance / em)
    }
}

/// One closed contour of an owned outline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnedContour {
    /// Where the contour begins and ends.
    pub start: (f64, f64),
    /// The segments, in order.
    pub segments: Vec<OwnedSegment>,
}

/// One segment of an owned contour.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OwnedSegment {
    /// A straight line.
    Line {
        /// Where the segment ends.
        to: (f64, f64),
    },
    /// A quadratic Bézier.
    Quadratic {
        /// The off-curve control point.
        control: (f64, f64),
        /// Where the segment ends.
        to: (f64, f64),
    },
}

/// The requested family's own geometry and the run of outlines answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutlineReply {
    /// The primary face's em.
    pub units_per_em: u32,
    /// The primary face's ascender, in its own font units.
    pub ascent: i32,
    /// The primary face's descender magnitude, in its own font units.
    pub descent: i32,
    /// The primary face's extra leading, in its own font units.
    pub line_gap: i32,
    /// The glyphs answered, a prefix of the run in order.
    pub glyphs: Vec<OwnedOutline>,
}

impl OutlineReply {
    /// The metrics of the face the batch is drawn from.
    #[must_use]
    pub const fn face(&self) -> FaceMetrics {
        FaceMetrics {
            units_per_em: self.units_per_em,
            ascent: self.ascent,
            descent: self.descent,
            line_gap: self.line_gap,
        }
    }

    /// The reply `frame` answering a run of `asked` scalars, owned; none for
    /// a frame that does not decode or a batch answering more than was asked,
    /// which the reply bound alone cannot rule out.
    #[must_use]
    pub fn decode(frame: &[u8], asked: usize) -> Option<Self> {
        let batch = decode_outlines_reply(frame).ok()?;
        if batch.glyphs().len() > asked {
            return None;
        }
        Some(Self::from_batch(&batch))
    }

    /// Copy a decoded batch out of the frame it borrows, so it outlives the
    /// receive buffer.
    ///
    /// The one place the wire form becomes an owned one: the client's own
    /// fetch and the build-time verification that drives the service
    /// directly both go through it, so neither can read a reply the other
    /// would read differently.
    #[must_use]
    pub fn from_batch(batch: &OutlineBatch<'_>) -> Self {
        Self {
            units_per_em: batch.units_per_em,
            ascent: batch.ascent,
            descent: batch.descent,
            line_gap: batch.line_gap,
            glyphs: batch.glyphs().iter().map(owned_outline).collect(),
        }
    }
}

/// Copy one borrowed outline record out of the reply frame.
fn owned_outline(glyph: &GlyphOutline<'_>) -> OwnedOutline {
    OwnedOutline {
        units_per_em: glyph.units_per_em,
        advance: glyph.advance.to_f64(),
        synthetic_bold: glyph.synth.bold_fraction(),
        synthetic_shear: glyph.synth.shear(),
        contours: glyph
            .contours()
            .map(|contour| OwnedContour {
                start: (contour.start.0.to_f64(), contour.start.1.to_f64()),
                segments: contour
                    .segments()
                    .map(|segment| match segment {
                        GlyphSegment::Line { to } => OwnedSegment::Line {
                            to: (to.0.to_f64(), to.1.to_f64()),
                        },
                        GlyphSegment::Quadratic { control, to } => OwnedSegment::Quadratic {
                            control: (control.0.to_f64(), control.1.to_f64()),
                            to: (to.0.to_f64(), to.1.to_f64()),
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}

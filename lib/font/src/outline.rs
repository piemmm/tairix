//! The font service's outline protocol as every client speaks it: the one
//! request, the one paging of a run through it, and the one place a reply
//! becomes an owned value, so no client can ask or read differently from
//! the others.

use alloc::vec::Vec;

use tairix_abi::font_ipc::{
    decode_outlines_reply, FamilyKey, FontRequest, FontStretch, FontStyle, FontWeight,
    GlyphOutline, GlyphRun, GlyphSegment, FONT_MAX_GLYPH_RUN,
};
use tairix_bezier::offset::{grown, Join};
use tairix_bezier::{FillRule, Path, PathEl, Point, QuadBez, Subpath};

/// The miter limit a synthetic bold is grown under, which its round joins
/// never consult, and the tolerance its curves are grown to, in ems.
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
    /// The primary face's metrics, in its own font units.
    pub face: FaceMetrics,
    /// The glyphs answered, a prefix of the run in order.
    pub glyphs: Vec<OwnedOutline>,
}

impl OutlineReply {
    /// The reply `frame` answering a run of `asked` scalars, owned; none for
    /// a frame that does not decode or a batch answering more than was asked,
    /// which the reply bound alone cannot rule out.
    fn decode(frame: &[u8], asked: usize) -> Option<Self> {
        let batch = decode_outlines_reply(frame).ok()?;
        if batch.glyphs().len() > asked {
            return None;
        }
        Some(Self {
            face: FaceMetrics {
                units_per_em: batch.units_per_em,
                ascent: batch.ascent,
                descent: batch.descent,
                line_gap: batch.line_gap,
            },
            glyphs: batch.glyphs().iter().map(owned_outline).collect(),
        })
    }
}

/// Ask the font service once for the outlines of a prefix of `scalars` from
/// `family`, instanced at `instance`: `call` carries the request frame and
/// leaves the reply at the head of `reply`, answering its length.
///
/// None for a run the protocol will not carry, a failed call, a length past
/// the buffer, or a frame that does not decode or answers more than was asked.
pub fn ask_outlines(
    scalars: &[char],
    family: FamilyKey,
    instance: (FontWeight, FontStyle, FontStretch),
    reply: &mut Vec<u8>,
    call: impl FnOnce(&[u8], &mut Vec<u8>) -> Option<usize>,
) -> Option<OutlineReply> {
    let (weight, style, stretch) = instance;
    let request = FontRequest::Outlines {
        family,
        scalars: GlyphRun::new(scalars).ok()?,
        weight,
        style,
        stretch,
    }
    .to_le_bytes();
    let len = call(&request, reply)?;
    OutlineReply::decode(reply.get(..len)?, scalars.len())
}

/// Fetch the outline of every scalar of `scalars`, in order: `fetch` is asked
/// for runs of at most [`FONT_MAX_GLYPH_RUN`] and, as a reply answers a prefix
/// of its run, again for whatever it leaves; `land` takes each reply with the
/// index of the first scalar it answers. Both are handed `context`.
///
/// None once a fetch fails or answers nothing or more than was asked, so the
/// caller draws nothing rather than glyphs paired with the wrong characters.
pub fn page_outlines<C: ?Sized>(
    context: &mut C,
    scalars: &[char],
    mut fetch: impl FnMut(&mut C, &[char]) -> Option<OutlineReply>,
    mut land: impl FnMut(&mut C, usize, OutlineReply),
) -> Option<()> {
    let mut at = 0;
    while at < scalars.len() {
        let run = &scalars[at..scalars.len().min(at + FONT_MAX_GLYPH_RUN)];
        let reply = fetch(context, run)?;
        let answered = reply.glyphs.len();
        if answered == 0 || answered > run.len() {
            return None;
        }
        land(context, at, reply);
        at += answered;
    }
    Some(())
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

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use tairix_abi::font_ipc::{
        FamilyKey, FontRequest, FontStretch, FontStyle, FontUnits, FontWeight, GlyphRun,
        OutlineBatchWriter, OutlineSource, Synthesis, FONT_MAX_GLYPH_RUN, FONT_MAX_OUTLINE_REPLY,
    };

    use super::{ask_outlines, page_outlines, FaceMetrics, OutlineReply, OwnedOutline};

    /// The instance every request here asks for.
    const INSTANCE: (FontWeight, FontStyle, FontStretch) =
        (FontWeight::BOLD, FontStyle::Italic, FontStretch::NORMAL);

    /// [`ask_outlines`] for `run` from the monospace family, through `call`.
    fn ask(
        run: &[char],
        call: impl FnOnce(&[u8], &mut Vec<u8>) -> Option<usize>,
    ) -> Option<OutlineReply> {
        ask_outlines(run, FamilyKey::MONO, INSTANCE, &mut Vec::new(), call)
    }

    /// A frame in `reply` answering `glyphs` scalars with outlines of no ink,
    /// and its length.
    fn frame(reply: &mut Vec<u8>, glyphs: usize) -> Option<usize> {
        reply.resize(FONT_MAX_OUTLINE_REPLY, 0);
        let mut writer = OutlineBatchWriter::new(reply, 1000, 800, 200, 0).ok()?;
        let blank = OutlineSource {
            units_per_em: 1000,
            advance: FontUnits::from_raw(0),
            synth: Synthesis::NONE,
            contours: &[],
        };
        for _ in 0..glyphs {
            writer.push(&blank).ok()?;
        }
        writer.finish().ok()
    }

    /// A reply answering `glyphs` scalars.
    fn answering(glyphs: usize) -> OutlineReply {
        OutlineReply {
            glyphs: vec![OwnedOutline::default(); glyphs],
            ..OutlineReply::default()
        }
    }

    #[test]
    fn a_request_names_its_run_and_instance_and_its_reply_is_read_whole() {
        let run = ['a', 'b'];
        let answered = ask(&run, |request, reply| {
            let asked = FontRequest::Outlines {
                family: FamilyKey::MONO,
                scalars: GlyphRun::new(&run).ok()?,
                weight: INSTANCE.0,
                style: INSTANCE.1,
                stretch: INSTANCE.2,
            };
            assert_eq!(FontRequest::from_bytes(request), Ok(asked));
            frame(reply, 1)
        })
        .expect("answered");
        assert_eq!(
            answered.face,
            FaceMetrics {
                units_per_em: 1000,
                ascent: 800,
                descent: 200,
                line_gap: 0,
            }
        );
        assert_eq!(answered.glyphs.len(), 1);
    }

    #[test]
    fn a_run_the_protocol_cannot_carry_or_a_reply_it_did_not_ask_for_is_refused() {
        let mut sent = 0;
        let long = ['a'; FONT_MAX_GLYPH_RUN + 1];
        for run in [&[][..], &long[..]] {
            let asked = ask(run, |_, _| {
                sent += 1;
                None
            });
            assert_eq!(asked, None);
        }
        assert_eq!(sent, 0, "neither run is sent");
        assert_eq!(
            ask(&['a'], |_, reply| Some(reply.len() + 1)),
            None,
            "a length past the buffer"
        );
        assert_eq!(
            ask(&['a'], |_, reply| frame(reply, 2)),
            None,
            "a frame answering more than was asked"
        );
    }

    #[test]
    fn what_a_reply_leaves_is_asked_for_again_from_where_it_stopped() {
        let mut asked = Vec::new();
        let mut landed = Vec::new();
        let paged = page_outlines(
            &mut landed,
            &['a', 'b', 'c'],
            |_, run| {
                asked.push(run.to_vec());
                Some(answering(1))
            },
            |landed, first, _| landed.push(first),
        );
        assert_eq!(paged, Some(()));
        assert_eq!(asked, [vec!['a', 'b', 'c'], vec!['b', 'c'], vec!['c']]);
        assert_eq!(landed, [0, 1, 2]);
    }

    #[test]
    fn a_run_longer_than_one_request_carries_is_asked_for_in_runs_it_does() {
        let long = ['x'; FONT_MAX_GLYPH_RUN + 1];
        let mut asked = Vec::new();
        let paged = page_outlines(
            &mut asked,
            &long,
            |asked, run| {
                asked.push(run.len());
                Some(answering(run.len()))
            },
            |_, _, _| {},
        );
        assert_eq!(paged, Some(()));
        assert_eq!(asked, [FONT_MAX_GLYPH_RUN, 1]);
    }

    #[test]
    fn a_reply_answering_more_than_was_asked_is_refused_rather_than_sliced_past() {
        let mut landed = 0;
        let paged = page_outlines(
            &mut landed,
            &['a', 'b'],
            |_, run| Some(answering(run.len() + 1)),
            |landed, _, _| *landed += 1,
        );
        assert_eq!(paged, None);
        assert_eq!(landed, 0, "nothing of a corrupt reply is kept");
    }

    #[test]
    fn a_reply_answering_nothing_is_refused_rather_than_asked_for_forever() {
        let mut fetches = 0;
        let paged = page_outlines(
            &mut fetches,
            &['a'],
            |fetches, _| {
                *fetches += 1;
                Some(answering(0))
            },
            |_, _, _| {},
        );
        assert_eq!(paged, None);
        assert_eq!(fetches, 1);
    }
}

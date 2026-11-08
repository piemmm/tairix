//! Glyphs as each thread may have them.
//!
//! A worker that draws text, or turns it into paths, fetches the outlines it
//! needs ([`Fetched`]) and so brings them into the font client's own bounded
//! cache. The event loop never waits on the font service: it reads only what
//! has already arrived there ([`Arrived`]), so text a worker has drawn is
//! picked, framed and edited by the glyphs it was drawn with, and text whose
//! glyphs are still on their way by its estimated box. Either way the
//! outlines are made into [`Glyph`]s once each for as long as one task lasts
//! ([`Lettering`]), and no set outlives its task.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::font_ipc::{FamilyKey, FontStretch, FontStyle, FontWeight};
use tairix_collections::{try_arc, OrderedMap};
use tairix_drawing::{Glyph, Glyphs};
use tairix_font::OwnedOutline;

/// Where a set of glyphs takes its outlines from.
pub trait OutlineSource {
    /// The outline of `scalar` in `family` at `weight` and `style`, if this
    /// source has it.
    fn outline(
        &self,
        family: FamilyKey,
        scalar: char,
        weight: FontWeight,
        style: FontStyle,
    ) -> Option<OwnedOutline>;

    /// The outlines of `scalars` onto `out` in order, `None` for each this
    /// source does not have: one at a time unless the source can do better.
    /// `out` has room for them all.
    fn outlines(
        &self,
        family: FamilyKey,
        scalars: &[char],
        weight: FontWeight,
        style: FontStyle,
        out: &mut Vec<Option<OwnedOutline>>,
    ) {
        out.extend(
            scalars
                .iter()
                .map(|&scalar| self.outline(family, scalar, weight, style)),
        );
    }
}

/// The outlines a fetch has already brought into the font client's cache:
/// what the event loop may read, never asking the service.
#[derive(Copy, Clone, Debug, Default)]
pub struct Arrived;

impl OutlineSource for Arrived {
    fn outline(
        &self,
        family: FamilyKey,
        scalar: char,
        weight: FontWeight,
        style: FontStyle,
    ) -> Option<OwnedOutline> {
        tairix_font::cached_outline(scalar, family, weight, style, FontStretch::default())
    }
}

/// The outlines the font service answers, through the client's cache: what
/// a worker, which may wait, reads.
#[derive(Copy, Clone, Debug, Default)]
pub struct Fetched;

impl OutlineSource for Fetched {
    fn outline(
        &self,
        family: FamilyKey,
        scalar: char,
        weight: FontWeight,
        style: FontStyle,
    ) -> Option<OwnedOutline> {
        let reply =
            tairix_font::outline_run(&[scalar], family, weight, style, FontStretch::default())?;
        reply.glyphs.into_iter().next()
    }

    /// As many runs as the service answers them in, each a prefix of what is
    /// left: those of the cache's already hold no round trip at all.
    fn outlines(
        &self,
        family: FamilyKey,
        scalars: &[char],
        weight: FontWeight,
        style: FontStyle,
        out: &mut Vec<Option<OwnedOutline>>,
    ) {
        let mut rest = scalars;
        while !rest.is_empty() {
            let Some(reply) =
                tairix_font::outline_run(rest, family, weight, style, FontStretch::default())
            else {
                break;
            };
            let answered = reply.glyphs.len().min(rest.len());
            if answered == 0 {
                break;
            }
            out.extend(reply.glyphs.into_iter().take(answered).map(Some));
            rest = &rest[answered..];
        }
        out.extend(rest.iter().map(|_| None));
    }
}

/// A glyph's identity within a set: its family, weight, posture and
/// character.
type GlyphKey = (FamilyKey, u16, bool, char);

/// Glyphs made from `S`'s outlines, each once, for as long as the set lives.
pub struct Lettering<S> {
    source: S,
    /// Each family name asked for, and the installed family it names.
    families: RefCell<OrderedMap<String, FamilyKey>>,
    made: RefCell<OrderedMap<GlyphKey, Option<Arc<Glyph>>>>,
}

impl<S: OutlineSource> Lettering<S> {
    /// An empty set over `source`.
    #[must_use]
    pub const fn new(source: S) -> Self {
        Self {
            source,
            families: RefCell::new(OrderedMap::new()),
            made: RefCell::new(OrderedMap::new()),
        }
    }

    /// The installed family `family` names, remembered where there is room.
    fn family(&self, family: &str) -> FamilyKey {
        if let Some(&key) = self.families.borrow().get(family) {
            return key;
        }
        let key = family_key(family);
        let mut name = String::new();
        if name.try_reserve(family.len()).is_ok() {
            name.push_str(family);
            // A set that cannot remember a family names it again when asked.
            let _ = self.families.borrow_mut().try_insert(name, key);
        }
        key
    }

    /// `outline` made a glyph and remembered as `key`'s, where there is room.
    fn make(&self, key: GlyphKey, outline: Option<&OwnedOutline>) -> Option<Arc<Glyph>> {
        // A glyph the heap cannot hold is not known yet, as one not fetched
        // is not; a set that cannot remember one makes it again when asked.
        let made = outline
            .and_then(glyph_of)
            .and_then(|glyph| try_arc(glyph).ok());
        let _ = self.made.borrow_mut().try_insert(key, made.clone());
        made
    }
}

/// The font service's terms for a weight and posture.
fn instance(weight: u16, italic: bool) -> (FontWeight, FontStyle) {
    let style = if italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    (FontWeight::new(weight).unwrap_or_default(), style)
}

impl<S: OutlineSource> Glyphs for Lettering<S> {
    fn glyph(&self, family: &str, weight: u16, italic: bool, c: char) -> Option<Arc<Glyph>> {
        let family = self.family(family);
        let key = (family, weight, italic, c);
        if let Some(made) = self.made.borrow().get(&key) {
            return made.clone();
        }
        let (weighed, style) = instance(weight, italic);
        let outline = self.source.outline(family, c, weighed, style);
        self.make(key, outline.as_ref())
    }

    fn prepare(&self, family: &str, weight: u16, italic: bool, text: &str) {
        let family = self.family(family);
        let mut wanted = Vec::new();
        {
            let made = self.made.borrow();
            let unmade = |c: &char| {
                *c != '\n' && *c != '\t' && made.get(&(family, weight, italic, *c)).is_none()
            };
            // Text set again, as each tile it crosses sets it, costs no copy.
            if !text.chars().any(|c| unmade(&c)) || wanted.try_reserve(text.len()).is_err() {
                return;
            }
            wanted.extend(text.chars().filter(unmade));
        }
        wanted.sort_unstable();
        wanted.dedup();
        let mut outlines = Vec::new();
        if wanted.is_empty() || outlines.try_reserve(wanted.len()).is_err() {
            return;
        }
        let (weighed, style) = instance(weight, italic);
        self.source
            .outlines(family, &wanted, weighed, style, &mut outlines);
        for (&c, outline) in wanted.iter().zip(&outlines) {
            self.make((family, weight, italic, c), outline.as_ref());
        }
    }
}

/// The installed family `family` names: a generic name its family, any
/// other the family of that key, sans-serif where it names none.
#[must_use]
pub fn family_key(family: &str) -> FamilyKey {
    let mut spelt = [0u8; tairix_abi::font_ipc::FONT_FAMILY_KEY_LEN];
    let bare = family.trim().trim_matches(|c| c == '"' || c == '\'');
    let lowered = match spelt.get_mut(..bare.len()) {
        Some(room) => {
            for (slot, byte) in room.iter_mut().zip(bare.bytes()) {
                *slot = if byte == b' ' {
                    b'-'
                } else {
                    byte.to_ascii_lowercase()
                };
            }
            core::str::from_utf8(room).unwrap_or("")
        }
        // Too long to be any family's key.
        None => "",
    };
    let named = match lowered {
        "serif" | "times" | "times-new-roman" | "georgia" | "trinity" => "noto-serif",
        "monospace" | "mono" | "courier" | "courier-new" | "corpus" => "mono",
        "sans-serif" | "sans" | "helvetica" | "arial" | "homerton" | "system-ui" => "noto-sans",
        other => other,
    };
    FamilyKey::new(named)
        .or_else(|_| FamilyKey::new("noto-sans"))
        .unwrap_or(FamilyKey::MONO)
}

/// The glyph a font service outline is: its contours in ems from the pen on
/// the baseline, y down, with the oblique shear and the bold a face could not
/// furnish applied; `None` for an outline of no em.
#[must_use]
pub fn glyph_of(outline: &OwnedOutline) -> Option<Glyph> {
    use tairix_bezier::offset::{grown, Join};
    use tairix_bezier::{FillRule, Path, PathEl, Point, QuadBez, Subpath};
    use tairix_font::OwnedSegment;

    let em = f64::from(outline.units_per_em);
    if em <= 0.0 {
        return None;
    }
    let shear = outline.synthetic_shear;
    let place = |(x, y): (f64, f64)| Point::new((x + shear * y) / em, -y / em);
    let mut path = Path::new();
    path.subpaths.try_reserve(outline.contours.len()).ok()?;
    for contour in &outline.contours {
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
    if outline.synthetic_bold > 0.0 {
        path = grown(
            &path,
            FillRule::NonZero,
            outline.synthetic_bold / 2.0,
            Join::Round,
            4.0,
            1e-3,
        )
        .ok()?;
    }
    Some(Glyph::new(path, outline.advance / em))
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use tairix_abi::font_ipc::{FamilyKey, FontStyle, FontWeight};
    use tairix_bezier::Point;
    use tairix_drawing::Glyphs;
    use tairix_font::{OwnedContour, OwnedOutline, OwnedSegment};

    use super::{family_key, glyph_of, Lettering, OutlineSource};

    #[test]
    fn an_outline_comes_out_in_ems_downward_and_sheared() {
        let square = OwnedOutline {
            units_per_em: 1000,
            advance: 600.0,
            synthetic_bold: 0.0,
            synthetic_shear: 0.25,
            contours: alloc::vec![OwnedContour {
                start: (0.0, 0.0),
                segments: alloc::vec![
                    OwnedSegment::Line { to: (500.0, 0.0) },
                    OwnedSegment::Quadratic {
                        control: (500.0, 1000.0),
                        to: (0.0, 1000.0),
                    },
                ],
            }],
        };
        let glyph = glyph_of(&square).expect("a glyph");
        assert!((glyph.advance() - 0.6).abs() < 1e-12);
        let subpath = &glyph.outline().subpaths[0];
        assert_eq!(subpath.start, Point::ZERO);
        assert_eq!(
            subpath.elements[1].end(),
            Point::new(0.25, -1.0),
            "up is y down, and leans right"
        );
        let empty = OwnedOutline {
            units_per_em: 0,
            ..square
        };
        assert!(glyph_of(&empty).is_none());
    }

    /// A source that counts what it is asked, answering a glyph for `a`.
    struct Counting(Cell<usize>);

    impl OutlineSource for Counting {
        fn outline(
            &self,
            _family: FamilyKey,
            scalar: char,
            _weight: FontWeight,
            _style: FontStyle,
        ) -> Option<OwnedOutline> {
            self.0.set(self.0.get() + 1);
            (scalar == 'a').then(|| OwnedOutline {
                units_per_em: 1000,
                advance: 500.0,
                ..OwnedOutline::default()
            })
        }
    }

    /// A source that counts its batches, answering every character.
    struct Batching(Cell<usize>, Cell<usize>);

    impl OutlineSource for Batching {
        fn outline(
            &self,
            _family: FamilyKey,
            _scalar: char,
            _weight: FontWeight,
            _style: FontStyle,
        ) -> Option<OwnedOutline> {
            self.0.set(self.0.get() + 1);
            Some(OwnedOutline {
                units_per_em: 1000,
                advance: 500.0,
                ..OwnedOutline::default()
            })
        }

        fn outlines(
            &self,
            _family: FamilyKey,
            scalars: &[char],
            _weight: FontWeight,
            _style: FontStyle,
            out: &mut alloc::vec::Vec<Option<OwnedOutline>>,
        ) {
            self.1.set(self.1.get() + 1);
            out.extend(scalars.iter().map(|_| {
                Some(OwnedOutline {
                    units_per_em: 1000,
                    advance: 500.0,
                    ..OwnedOutline::default()
                })
            }));
        }
    }

    #[test]
    fn text_set_from_a_set_fetches_its_characters_together() {
        let lettering = Lettering::new(Batching(Cell::new(0), Cell::new(0)));
        let text = tairix_drawing::Text {
            family: tairix_drawing::SharedStr::try_from_str("Sans").expect("held"),
            ..crate::testing::text("hello\tworld\nhello", Point::ZERO)
        };
        let setting = text.set(&lettering).expect("set");
        assert!(setting.complete);
        assert_eq!(lettering.source.1.get(), 1, "one batch");
        assert_eq!(lettering.source.0.get(), 1, "and the space alone apart");
        text.set(&lettering).expect("set again");
        assert_eq!(lettering.source.1.get(), 1, "nothing left to fetch");
    }

    #[test]
    fn a_set_asks_its_source_once_a_glyph_and_names_families_as_the_service_does() {
        let lettering = Lettering::new(Counting(Cell::new(0)));
        for _ in 0..3 {
            let a = lettering.glyph("Sans", 400, false, 'a').expect("an a");
            assert!((a.advance() - 0.5).abs() < 1e-12);
            assert!(lettering.glyph("Sans", 400, false, 'b').is_none());
        }
        assert_eq!(
            lettering.source.0.get(),
            2,
            "once for each, its absence remembered"
        );
        lettering.glyph("Sans", 700, false, 'a');
        assert_eq!(
            lettering.source.0.get(),
            3,
            "another weight is another glyph"
        );
        assert_eq!(family_key("Times New Roman"), family_key("serif"));
        assert_eq!(family_key("'Courier'"), FamilyKey::MONO);
        assert_eq!(
            family_key("no-such-face/.."),
            family_key("sans-serif"),
            "never a path"
        );
    }
}

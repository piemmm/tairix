//! RISC OS's own families as TAIRiX faces. A ROM font's glyphs are base
//! encoding 0's, each the face's outline of the first character UTF-8
//! reaches that glyph from, fetched from the font service when the font is
//! found, so its data does not change after; its metrics are the outlines',
//! in the thousandths of an em a metrics file holds.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::size_of;

use tairix_abi::font_ipc::{FamilyKey, FontStretch, FontStyle, FontWeight};
use tairix_bezier::{Path, PathEl};
use tairix_drawing::riscos::{self, Slant};
use tairix_font::{ask_outlines, page_outlines, FaceMetrics};
use tairix_util::mathf::{ceil, floor, round_i32};

use crate::desktop;
use crate::metrics;
use crate::rom_encodings::{BASE0, BASE0_GLYPHS};
use crate::units;

/// Thousandths of an em an em has.
const MILLEMS: f64 = units::MILLEMS as f64;

/// A glyph of a face, as a font measures and draws it.
#[derive(Debug)]
pub(crate) struct Shape {
    /// Where it moves the pen, in thousandths of an em.
    pub(crate) advance: i32,
    /// Its box in thousandths of an em; nought for a glyph with no ink.
    pub(crate) bbox: metrics::Box,
    /// Its outline in ems, y up, filled non-zero.
    pub(crate) path: Path,
}

/// The service failed or answered what was not asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unanswered;

/// A ROM font's face and its glyphs.
#[derive(Debug)]
pub(crate) struct Face {
    /// Each base glyph's shape, none for a glyph no character reaches or one
    /// not asked for.
    shapes: Vec<Option<Box<Shape>>>,
    /// The face the service drew from: its em, ascender and descender.
    metrics: FaceMetrics,
    /// The slant the service gave a face that has none of its own, as a
    /// fraction of the height.
    slant: f64,
}

/// What the face of RISC OS's family `identifier` is asked for by.
struct Asking {
    family: FamilyKey,
    weight: FontWeight,
    style: FontStyle,
}

impl Asking {
    /// The face ROM font `identifier` asks for: one of RISC OS's own
    /// families by its kind, or the desktop font in `desktop`.
    fn of(identifier: &[u8], desktop: FamilyKey) -> Option<Self> {
        let name = core::str::from_utf8(identifier).ok()?;
        let (family, weight, slant) = match riscos::riscos_face(name) {
            Some(face) => (FamilyKey::new(face.generic).ok()?, face.weight, face.slant),
            None if desktop::names(name) => {
                let (weight, slanted) = riscos::riscos_styles(name);
                (desktop, weight, slanted.then_some(Slant::Oblique))
            }
            None => return None,
        };
        let style = match slant {
            None => FontStyle::Normal,
            Some(Slant::Italic) => FontStyle::Italic,
            Some(Slant::Oblique) => FontStyle::Oblique,
        };
        Some(Self {
            family,
            weight: FontWeight::new(weight).ok()?,
            style,
        })
    }
}

impl Face {
    /// Whether `identifier` is one of RISC OS's own fonts or the desktop
    /// font's, which a face stands for.
    pub(crate) fn exists(identifier: &[u8]) -> bool {
        core::str::from_utf8(identifier)
            .is_ok_and(|name| riscos::riscos_face(name).is_some() || desktop::names(name))
    }

    /// The face of ROM font `identifier`, the desktop font's in the family
    /// `desktop`, each glyph of `wanted` asked of the font service through
    /// `relay` a run of characters at a time.
    pub(crate) fn load(
        identifier: &[u8],
        desktop: FamilyKey,
        relay: &mut dyn FnMut(&[u8], &mut Vec<u8>) -> bool,
        wanted: impl Iterator<Item = u32>,
    ) -> Result<Self, Unanswered> {
        let asking = Asking::of(identifier, desktop).ok_or(Unanswered)?;
        let mut shapes = Vec::new();
        shapes
            .try_reserve_exact(BASE0_GLYPHS)
            .map_err(|_| Unanswered)?;
        shapes.extend((0..BASE0_GLYPHS).map(|_| None));
        let mut asked = alloc::vec![false; BASE0_GLYPHS];
        let mut asking_for: Vec<(usize, char)> = Vec::new();
        for glyph in wanted.filter_map(|g| usize::try_from(g).ok()) {
            let Some(first) = BASE0.get(glyph).and_then(|characters| characters.first()) else {
                continue;
            };
            if core::mem::replace(&mut asked[glyph], true) {
                continue;
            }
            if let Some(character) = char::from_u32(*first) {
                asking_for.try_reserve(1).map_err(|_| Unanswered)?;
                asking_for.push((glyph, character));
            }
        }
        let mut face = Self {
            shapes,
            metrics: FaceMetrics::default(),
            slant: 0.0,
        };
        let characters: Vec<char> = asking_for.iter().map(|&(_, character)| character).collect();
        let instance = (asking.weight, asking.style, FontStretch::NORMAL);
        let mut frame = Vec::new();
        page_outlines(
            &mut face,
            &characters,
            |_, run| {
                ask_outlines(
                    run,
                    asking.family,
                    instance,
                    &mut frame,
                    |request, reply| {
                        reply.clear();
                        relay(request, reply).then_some(reply.len())
                    },
                )
            },
            |face, first, answered| {
                face.metrics = answered.face;
                face.slant = answered.glyphs[0].synthetic_shear;
                for (&(glyph, _), outline) in asking_for.iter().skip(first).zip(&answered.glyphs) {
                    face.shapes[glyph] = shape(outline).map(Box::new);
                }
            },
        )
        .ok_or(Unanswered)?;
        Ok(face)
    }

    /// Glyph `glyph`'s shape, where the face has it.
    pub(crate) fn shape(&self, glyph: u32) -> Option<&Shape> {
        self.shapes.get(usize::try_from(glyph).ok()?)?.as_deref()
    }

    /// The box holding every glyph it has, in thousandths of an em.
    pub(crate) fn bbox(&self) -> metrics::Box {
        self.shapes
            .iter()
            .flatten()
            .map(|shape| shape.bbox)
            .reduce(metrics::Box::union)
            .unwrap_or_default()
    }

    /// The face's ascender and descender, in thousandths of an em, the
    /// descender below the baseline.
    pub(crate) fn extent(&self) -> (i32, i32) {
        let em = f64::from(self.metrics.units_per_em);
        if em <= 0.0 {
            return (0, 0);
        }
        let millems = |units: i32| round_i32(f64::from(units) * MILLEMS / em);
        (millems(self.metrics.ascent), -millems(self.metrics.descent))
    }

    /// The slant the service gave it, in thousandths of an em across an em
    /// up: what a metrics file's italic offset holds.
    pub(crate) fn italic(&self) -> i32 {
        round_i32(self.slant * MILLEMS)
    }

    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        let shapes = self.shapes.iter().flatten().map(|shape| {
            let elements: usize = shape.path.subpaths.iter().map(|s| s.elements.len()).sum();
            size_of::<Shape>() + elements * size_of::<PathEl>()
        });
        self.shapes.len() * size_of::<Option<Box<Shape>>>() + shapes.sum::<usize>()
    }
}

/// An outline as a glyph's shape, its advance and box rounded to the
/// thousandths of an em a metrics file holds: the box outward.
fn shape(outline: &tairix_font::OwnedOutline) -> Option<Shape> {
    let path = outline.path()?;
    let advance = round_i32(outline.advance_em()? * MILLEMS);
    let bounds = path.bounds();
    let bbox = if path.subpaths.is_empty() {
        metrics::Box::default()
    } else {
        metrics::Box {
            x0: round_i32(floor(bounds.min.x * MILLEMS)),
            y0: round_i32(floor(bounds.min.y * MILLEMS)),
            x1: round_i32(ceil(bounds.max.x * MILLEMS)),
            y1: round_i32(ceil(bounds.max.y * MILLEMS)),
        }
    };
    Some(Shape {
        advance,
        bbox,
        path,
    })
}

#[cfg(test)]
mod tests;

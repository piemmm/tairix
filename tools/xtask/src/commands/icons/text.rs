//! Lettering, set from the faces the tree ships as outlines the canvas fills.

use std::path::Path;

use tairix_fontface::{AxisSetting, Face, OutlineSegment};

use super::canvas::{Contour, Mask};

/// The sans face labels are set in (SIL OFL 1.1).
const SANS: &str = "lib/font/assets/inter/Inter-Variable.ttf";

/// The serif face a type specimen is set in (SIL OFL 1.1).
const SERIF: &str = "lib/font/assets/noto-serif/NotoSerif-Variable.ttf";

/// Chords each quadratic of an outline is cut into.
const CHORDS: u32 = 24;

/// Which face, at which weight on its `wght` axis.
#[derive(Copy, Clone, Debug)]
pub enum Typeface {
    Sans(f32),
    Serif(f32),
}

/// Lettering set and placed: its outline and the box its ink fills.
pub struct Lettering {
    pub mask: Mask,
    /// Left, top, right, bottom, in units.
    pub ink: (f64, f64, f64, f64),
}

/// The faces, read once from the tree.
pub struct Fonts {
    sans: Vec<u8>,
    serif: Vec<u8>,
}

impl Fonts {
    /// Read the faces from the workspace at `root`.
    pub fn load(root: &Path) -> Result<Self, String> {
        let read = |relative: &str| {
            std::fs::read(root.join(relative)).map_err(|e| format!("icons: {relative}: {e}"))
        };
        Ok(Self {
            sans: read(SANS)?,
            serif: read(SERIF)?,
        })
    }

    /// `text` in `typeface` at `size` units to the em, `tracking` units
    /// between letters, its ink centred on `centre`.
    pub fn set(
        &self,
        typeface: Typeface,
        text: &str,
        size: f64,
        tracking: f64,
        centre: (f64, f64),
    ) -> Result<Lettering, String> {
        let (bytes, weight) = match typeface {
            Typeface::Sans(weight) => (&self.sans, weight),
            Typeface::Serif(weight) => (&self.serif, weight),
        };
        let face = Face::parse_instance(
            bytes,
            &[AxisSetting {
                tag: *b"wght",
                value: weight,
            }],
        )
        .map_err(|e| format!("icons: a face would not open: {e:?}"))?;
        let scale = size / f64::from(face.units_per_em());
        let mut contours: Vec<Contour> = Vec::new();
        let mut pen = 0.0;
        for character in text.chars() {
            let glyph = face
                .glyph_for(u32::from(character))
                .ok_or_else(|| format!("icons: no glyph for {character:?}"))?;
            let outline = face
                .glyph_outline(glyph)
                .map_err(|e| format!("icons: the outline of {character:?}: {e:?}"))?;
            for contour in outline {
                contours.push(flattened(&contour.start, &contour.segments, pen, scale));
            }
            let advance = face
                .advance(glyph)
                .map_err(|e| format!("icons: the advance of {character:?}: {e:?}"))?;
            pen += f64::from(advance) * scale + tracking;
        }
        let ink = bounds(&contours).ok_or_else(|| format!("icons: {text:?} draws nothing"))?;
        let (dx, dy) = (
            centre.0 - f64::midpoint(ink.0, ink.2),
            centre.1 - f64::midpoint(ink.1, ink.3),
        );
        for contour in &mut contours {
            for point in contour.iter_mut() {
                *point = (point.0 + dx, point.1 + dy);
            }
        }
        Ok(Lettering {
            mask: Mask::of(&contours),
            ink: (ink.0 + dx, ink.1 + dy, ink.2 + dx, ink.3 + dy),
        })
    }
}

/// A glyph contour in font units, y up, as points in units with y down, its
/// origin `pen` units along.
fn flattened(start: &(f64, f64), segments: &[OutlineSegment], pen: f64, scale: f64) -> Contour {
    let place = |(x, y): (f64, f64)| (pen + x * scale, -y * scale);
    let mut out = vec![place(*start)];
    let mut from = *start;
    for segment in segments {
        match *segment {
            OutlineSegment::Line { to } => {
                out.push(place(to));
                from = to;
            }
            OutlineSegment::Quadratic { control, to } => {
                for i in 1..=CHORDS {
                    let t = f64::from(i) / f64::from(CHORDS);
                    let u = 1.0 - t;
                    let x = u * u * from.0 + 2.0 * u * t * control.0 + t * t * to.0;
                    let y = u * u * from.1 + 2.0 * u * t * control.1 + t * t * to.1;
                    out.push(place((x, y)));
                }
                from = to;
            }
        }
    }
    out
}

/// The box every point of `contours` lies in.
fn bounds(contours: &[Contour]) -> Option<(f64, f64, f64, f64)> {
    let mut points = contours.iter().flatten();
    let &(x, y) = points.next()?;
    Some(points.fold((x, y, x, y), |(l, t, r, b), &(x, y)| {
        (l.min(x), t.min(y), r.max(x), b.max(y))
    }))
}

//! Shadings (ISO 32000-2 §8.7.4.5): axial and radial shadings read as
//! gradients, their functions sampled into stops.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Point};
use tairix_collections::SharedSlice;
use tairix_colour::Rgba;
use tairix_util::fallible::try_push;

use crate::colour::{Space, Srgb};
use crate::display::{Gradient, GradientKind, Stop};
use crate::document::Document;
use crate::function::{numbers, Function, Values, Work, MAX_COMPONENTS};
use crate::object::{Dict, Object};
use crate::page::Rect;
use crate::PdfError;

/// The most stops a sampled gradient is given.
pub(crate) const MAX_STOPS: usize = 256;

/// How far, in sRGB levels, a gradient's straight run between two stops may
/// stray from its function before another stop is placed between them.
const TOLERANCE: f64 = 0.5 / 255.0;

/// How many times a run between two stops is halved at most.
const MAX_REFINEMENT: u32 = 8;

/// A shading read as a gradient in its own space, and the box it is held to.
pub(crate) struct Shading {
    pub(crate) gradient: Gradient,
    pub(crate) bbox: Option<Rect>,
}

/// The one-input function, or functions one a component, a shading's colour
/// comes from.
struct Colours {
    functions: Vec<Function>,
}

impl Colours {
    fn load(document: &Document<'_>, object: &Object) -> Result<Self, PdfError> {
        let object = document.resolve(object)?;
        let listed: &[Object] = match object.as_array() {
            Some(listed) => listed,
            None => core::slice::from_ref(&*object),
        };
        if listed.is_empty() || listed.len() > MAX_COMPONENTS {
            return Err(PdfError::Malformed);
        }
        let mut functions = Vec::new();
        functions.try_reserve_exact(listed.len())?;
        for function in listed {
            let function = Function::load(document, function)?;
            if function.inputs() != 1 {
                return Err(PdfError::Malformed);
            }
            functions.push(function);
        }
        Ok(Self { functions })
    }

    fn at(&self, t: f64, left: bool, work: &Work) -> Result<Values, PdfError> {
        let evaluate = |function: &Function, outputs: &mut Values| {
            if left {
                function.evaluate_left(&[t], work, outputs)
            } else {
                function.evaluate(&[t], work, outputs)
            }
        };
        let mut each = Values::new();
        match self.functions.as_slice() {
            [one] => evaluate(one, &mut each)?,
            many => {
                let mut one = Values::new();
                for function in many {
                    evaluate(function, &mut one)?;
                    let _ = each.try_push(one.first().copied().unwrap_or(0.0));
                }
            }
        }
        Ok(each)
    }

    /// Where any of its functions' pieces meet, ascending.
    fn bounds(&self) -> Result<Vec<f64>, PdfError> {
        let mut bounds = Vec::new();
        for function in &self.functions {
            for &bound in function.bounds() {
                try_push(&mut bounds, bound)?;
            }
        }
        bounds.sort_unstable_by(f64::total_cmp);
        bounds.dedup();
        Ok(bounds)
    }
}

/// The shading `object` is, as a gradient, where it is axial or radial, its
/// colour space named in `spaces`; `None` for the other types, which no
/// gradient holds.
pub(crate) fn shading(
    document: &Document<'_>,
    object: &Object,
    spaces: Option<&Dict>,
) -> Result<Option<Shading>, PdfError> {
    let object = document.resolve(object)?;
    let dict = object.as_dict().ok_or(PdfError::Malformed)?;
    let radial = match document.get(dict, b"ShadingType")?.as_int() {
        Some(2) => false,
        Some(3) => true,
        Some(1 | 4..=7) => return Ok(None),
        _ => return Err(PdfError::Malformed),
    };
    let space = Space::load(document, &*document.get(dict, b"ColorSpace")?, spaces)?;
    if matches!(*space, Space::Pattern(_)) {
        return Err(PdfError::Malformed);
    }
    let coords = numbers(document, dict, b"Coords")?.ok_or(PdfError::Malformed)?;
    let kind = match (radial, coords.as_slice()) {
        (false, &[x0, y0, x1, y1]) => GradientKind::Axial {
            start: Point::new(x0, y0),
            end: Point::new(x1, y1),
        },
        (true, &[x0, y0, r0, x1, y1, r1]) if r0 >= 0.0 && r1 >= 0.0 => GradientKind::Radial {
            start: Point::new(x0, y0),
            start_radius: r0,
            end: Point::new(x1, y1),
            end_radius: r1,
        },
        _ => return Err(PdfError::Malformed),
    };
    let domain = match numbers(document, dict, b"Domain")?.as_deref() {
        Some(&[t0, t1]) => (t0, t1),
        None => (0.0, 1.0),
        Some(_) => return Err(PdfError::Malformed),
    };
    let extend = match document.get(dict, b"Extend")?.as_array() {
        Some([start, end]) => (
            document.resolve(start)?.as_bool().unwrap_or(false),
            document.resolve(end)?.as_bool().unwrap_or(false),
        ),
        _ => (false, false),
    };
    let colours = Colours::load(document, &*document.get(dict, b"Function")?)?;
    let Some(stops) = stops(&colours, &space, domain, document.work())? else {
        return Ok(None);
    };
    let bbox = match numbers(document, dict, b"BBox")?.as_deref() {
        Some(&[x0, y0, x1, y1]) => Some(Rect::spanning(x0, y0, x1, y1)),
        _ => None,
    };
    Ok(Some(Shading {
        gradient: Gradient {
            kind,
            stops: SharedSlice::try_from_vec(stops)?,
            extend,
            transform: Affine::IDENTITY,
        },
        bbox,
    }))
}

/// The stops a shading's colours make over `domain`: one at each end and at
/// each place its functions' pieces meet — two where they meet in a sharp
/// step — and between them as many as keep the straight runs within the
/// tolerance of the functions, while the stops and the halving depth allow.
/// `None` where its pieces meet in more places than a gradient has stops.
fn stops(
    colours: &Colours,
    space: &Arc<Space>,
    (t0, t1): (f64, f64),
    work: &Work,
) -> Result<Option<Vec<Stop>>, PdfError> {
    let colour = |t: f64, left: bool| -> Result<Srgb, PdfError> {
        Ok(space
            .srgb(&colours.at(t, left, work)?, work)?
            .unwrap_or([0.0; 3]))
    };
    // A domain may run downwards, so a run leaves each place on the side
    // below it rather than above.
    let descending = t1 < t0;
    let mut breaks = Vec::new();
    try_push(&mut breaks, t0)?;
    let mut interior = colours.bounds()?;
    interior.retain(|&bound| (bound - t0) * (t1 - bound) > 0.0);
    // Each place pieces meet may take two stops.
    if 2 * (interior.len() + 2) > MAX_STOPS {
        return Ok(None);
    }
    if descending {
        interior.reverse();
    }
    breaks.try_reserve(interior.len() + 1)?;
    breaks.extend(interior);
    breaks.push(t1);
    let mut out = Vec::new();
    let span = t1 - t0;
    let offset = |t: f64| {
        if span.abs() < f64::MIN_POSITIVE {
            0.0
        } else {
            ((t - t0) / span).clamp(0.0, 1.0)
        }
    };
    for pair in breaks.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (from, to) = (colour(a, descending)?, colour(b, !descending)?);
        let repeated = out.last().is_some_and(|last: &Stop| {
            last.colour == rgba(from) && (last.offset - offset(a)).abs() <= f64::EPSILON
        });
        if !repeated {
            try_push(
                &mut out,
                Stop {
                    offset: offset(a),
                    colour: rgba(from),
                },
            )?;
        }
        refine(
            &mut out,
            &colour,
            (a, from),
            (b, to),
            MAX_REFINEMENT,
            &offset,
        )?;
        try_push(
            &mut out,
            Stop {
                offset: offset(b),
                colour: rgba(to),
            },
        )?;
    }
    Ok(Some(out))
}

/// Stops strictly between `a` and `b` wherever the straight run between them
/// strays from the colours, halving the run at most `depth` more times.
fn refine(
    out: &mut Vec<Stop>,
    colour: &impl Fn(f64, bool) -> Result<Srgb, PdfError>,
    (a, from): (f64, Srgb),
    (b, to): (f64, Srgb),
    depth: u32,
    offset: &impl Fn(f64) -> f64,
) -> Result<(), PdfError> {
    if depth == 0 || out.len() + 2 >= MAX_STOPS {
        return Ok(());
    }
    let mut straight = true;
    for share in [0.25, 0.5, 0.75] {
        let actual = colour(a + (b - a) * share, false)?;
        straight &= (0..3).all(|channel| {
            (actual[channel] - (from[channel] + (to[channel] - from[channel]) * share)).abs()
                <= TOLERANCE
        });
    }
    if straight {
        return Ok(());
    }
    let middle = f64::midpoint(a, b);
    let at_middle = colour(middle, false)?;
    refine(
        out,
        colour,
        (a, from),
        (middle, at_middle),
        depth - 1,
        offset,
    )?;
    try_push(
        out,
        Stop {
            offset: offset(middle),
            colour: rgba(at_middle),
        },
    )?;
    refine(out, colour, (middle, at_middle), (b, to), depth - 1, offset)
}

/// `colour` as an opaque 8-bit colour.
pub(crate) fn rgba(colour: Srgb) -> Rgba {
    let [r, g, b] = colour.map(level);
    Rgba::new(r, g, b, u8::MAX)
}

/// A channel `0..=1` as an 8-bit level.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "held within 0..=255 and rounded before the cast"
)]
pub(crate) fn level(channel: f64) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
#[path = "shading_tests.rs"]
mod tests;

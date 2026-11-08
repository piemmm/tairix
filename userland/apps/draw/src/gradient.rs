//! Gradients as the inspector makes and edits them: fitted to an object's
//! own box, turned, and their stops added, moved, recoloured, removed and
//! reversed. Each answers a new gradient or stop list; none alters one in
//! place.

use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Point};
use tairix_collections::try_arc;
use tairix_colour::{Lab, Rgba};
use tairix_drawing::limits::MAX_STOPS;
use tairix_drawing::{DrawingError, Gradient, GradientKind, Paint, Spread, Stop, Style};
use tairix_raster::{Color, GradientStop, SpreadMethod};
use tairix_util::fallible;
use tairix_util::mathf::{atan2, cos, fabs, fmax, sin, sqrt};

use tairix_drawing::PAPER_WHITE;

/// The least half-length or radius a fitted gradient reaches, in the
/// object's units, so an object with no width or height still has a
/// gradient that runs.
const LEAST_REACH: f64 = 0.5;

/// The CIE lightness of a mid grey, halfway between black and white as the
/// eye sees them.
const MID_LIGHTNESS: f64 = 50.0;

/// How a gradient spreads: along a line, or out from a centre.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Form {
    /// Along a line this many degrees anticlockwise from rightward.
    Linear(f64),
    /// Out from a centre.
    Radial,
}

/// A gradient of `form` fitted to `bounds` in its object's own space, running
/// `stops` and continuing past its ends by `spread`. A linear one crosses the
/// box at its angle so its ends meet the box's farthest corners; a radial one
/// reaches from the box's centre to its corners.
#[must_use]
pub fn fitted(form: Form, stops: Vec<Stop>, spread: Spread, bounds: Bounds) -> Gradient {
    let (centre, width, height) = if bounds.is_empty() {
        (Point::ZERO, 0.0, 0.0)
    } else {
        (bounds.centre(), bounds.width(), bounds.height())
    };
    let kind = match form {
        Form::Linear(degrees) => {
            let turn = degrees.to_radians();
            // The canvas runs downward, so anticlockwise is up the page.
            let along = Point::new(cos(turn), -sin(turn));
            let reach = f64::midpoint(fabs(width * along.x), fabs(height * along.y));
            let half = along * fmax(reach, LEAST_REACH);
            GradientKind::Linear {
                start: centre - half,
                end: centre + half,
            }
        }
        Form::Radial => GradientKind::Radial {
            centre,
            radius: fmax(sqrt(width * width + height * height) / 2.0, LEAST_REACH),
            focus: centre,
        },
    };
    Gradient {
        kind,
        stops,
        spread,
        transform: Affine::IDENTITY,
    }
}

/// How `gradient` spreads, a linear one's angle read in its object's space.
#[must_use]
pub fn form_of(gradient: &Gradient) -> Form {
    match gradient.kind {
        GradientKind::Linear { start, end } => {
            let run = gradient.transform.map(end) - gradient.transform.map(start);
            Form::Linear(normalised(-atan2(run.y, run.x).to_degrees()))
        }
        GradientKind::Radial { .. } => Form::Radial,
    }
}

/// `degrees` brought into `(-180, 180]`.
#[must_use]
pub fn normalised(degrees: f64) -> f64 {
    let turned = degrees % 360.0;
    if turned > 180.0 {
        turned - 360.0
    } else if turned <= -180.0 {
        turned + 360.0
    } else {
        turned
    }
}

/// The stops a gradient chosen in place of `colour` runs: from it to white,
/// or to black where it is lighter than a mid grey.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn first_stops(colour: Rgba) -> Result<Vec<Stop>, DrawingError> {
    let far = if Lab::from_rgb(colour.without_alpha()).l > MID_LIGHTNESS {
        Rgba::new(0, 0, 0, 255)
    } else {
        PAPER_WHITE
    };
    let mut stops = Vec::new();
    if !fallible::reserve(&mut stops, 2) {
        return Err(DrawingError::OutOfMemory);
    }
    stops.push(Stop {
        offset: 0.0,
        colour,
    });
    stops.push(Stop {
        offset: 1.0,
        colour: far,
    });
    Ok(stops)
}

/// The renderer's ramp through `stops` from 0 to 1 along x, so what samples
/// it blends them as an object painted with them is blended.
#[must_use]
pub fn ramp(stops: &[Stop]) -> Option<tairix_raster::Gradient> {
    Some(tairix_raster::Gradient {
        kind: tairix_raster::GradientKind::Linear,
        stops: fallible::collected(
            stops.len(),
            stops.iter().map(|stop| GradientStop {
                offset: stop.offset,
                color: Color::from(stop.colour),
            }),
        )?,
        spread: SpreadMethod::Pad,
        to_gradient: Affine::IDENTITY,
    })
}

/// The colour `stops` give at `offset`, as the renderer blends them.
#[must_use]
pub fn colour_at(stops: &[Stop], offset: f64) -> Option<Rgba> {
    let Color { r, g, b, a } = ramp(stops)?.sample((offset, 0.0));
    Some(Rgba::new(r, g, b, a))
}

/// `stops` copied, for an edit to change.
fn copied(stops: &[Stop]) -> Result<Vec<Stop>, DrawingError> {
    fallible::copied(stops).ok_or(DrawingError::OutOfMemory)
}

/// `stops` with one more at `offset`, coloured as the gradient is there, and
/// where it stands among them.
///
/// # Errors
///
/// [`DrawingError::TooComplex`] where `stops` already holds [`MAX_STOPS`],
/// and [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn added(stops: &[Stop], offset: f64) -> Result<(Vec<Stop>, usize), DrawingError> {
    if stops.len() >= MAX_STOPS {
        return Err(DrawingError::TooComplex);
    }
    let offset = offset.clamp(0.0, 1.0);
    let colour = colour_at(stops, offset).ok_or(DrawingError::OutOfMemory)?;
    let at = stops.partition_point(|stop| stop.offset <= offset);
    let mut more = Vec::new();
    if !fallible::reserve(&mut more, stops.len() + 1) {
        return Err(DrawingError::OutOfMemory);
    }
    more.extend_from_slice(&stops[..at]);
    more.push(Stop { offset, colour });
    more.extend_from_slice(&stops[at..]);
    Ok((more, at))
}

/// `stops` with the one at `index` moved to `offset`, held between its
/// neighbours so the offsets stay in order.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn moved(stops: &[Stop], index: usize, offset: f64) -> Result<Vec<Stop>, DrawingError> {
    let mut stops = copied(stops)?;
    let least = index
        .checked_sub(1)
        .and_then(|before| stops.get(before))
        .map_or(0.0, |stop| stop.offset);
    let most = stops.get(index + 1).map_or(1.0, |stop| stop.offset);
    if let Some(stop) = stops.get_mut(index) {
        stop.offset = offset.clamp(least, most);
    }
    Ok(stops)
}

/// `stops` with the one at `index` coloured `colour`.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn recoloured(stops: &[Stop], index: usize, colour: Rgba) -> Result<Vec<Stop>, DrawingError> {
    let mut stops = copied(stops)?;
    if let Some(stop) = stops.get_mut(index) {
        stop.colour = colour;
    }
    Ok(stops)
}

/// `stops` without the one at `index`, or nothing where that would leave a
/// gradient fewer than its two ends.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn removed(stops: &[Stop], index: usize) -> Result<Option<Vec<Stop>>, DrawingError> {
    if stops.len() <= 2 || index >= stops.len() {
        return Ok(None);
    }
    let mut stops = copied(stops)?;
    stops.remove(index);
    Ok(Some(stops))
}

/// `stops` run the other way.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn reversed(stops: &[Stop]) -> Result<Vec<Stop>, DrawingError> {
    fallible::collected(
        stops.len(),
        stops.iter().rev().map(|stop| Stop {
            offset: 1.0 - stop.offset,
            colour: stop.colour,
        }),
    )
    .ok_or(DrawingError::OutOfMemory)
}

/// Whether two forms are the same: both radial, or linear at angles that
/// differ by no more than rounding does.
fn same_form(a: Form, b: Form) -> bool {
    match (a, b) {
        (Form::Linear(a), Form::Linear(b)) => fabs(normalised(a - b)) < 1e-9,
        (Form::Radial, Form::Radial) => true,
        _ => false,
    }
}

/// `paint` made a gradient of `form` across the box `bounds` answers: a
/// gradient of that form already is kept as it is, one of another made
/// again in `form` with its stops and spread, and a colour or nothing made
/// the start of a gradient of its own.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn formed(
    paint: &Paint,
    form: Form,
    bounds: &dyn Fn() -> Bounds,
) -> Result<Paint, DrawingError> {
    let gradient = match paint {
        Paint::Gradient(gradient) if same_form(form_of(gradient), form) => {
            return Ok(paint.clone());
        }
        Paint::Gradient(gradient) => {
            fitted(form, copied(&gradient.stops)?, gradient.spread, bounds())
        }
        Paint::Colour(colour) => fitted(form, first_stops(*colour)?, Spread::Pad, bounds()),
        Paint::None => fitted(
            form,
            first_stops(Rgba::new(0, 0, 0, 255))?,
            Spread::Pad,
            bounds(),
        ),
    };
    Ok(Paint::Gradient(try_arc(gradient)?))
}

/// `paint` run through `stops` and continued by `spread`: a gradient keeps
/// where it lies, and anything else becomes one of `form` across the box
/// `bounds` answers.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn restopped(
    paint: &Paint,
    (stops, spread, form): (&[Stop], Spread, Form),
    bounds: &dyn Fn() -> Bounds,
) -> Result<Paint, DrawingError> {
    let gradient = match paint {
        Paint::Gradient(gradient) => Gradient {
            kind: gradient.kind,
            stops: copied(stops)?,
            spread,
            transform: gradient.transform,
        },
        Paint::Colour(_) | Paint::None => fitted(form, copied(stops)?, spread, bounds()),
    };
    Ok(Paint::Gradient(try_arc(gradient)?))
}

/// `paint` made a gradient laid from `start` to `end` in its object's own
/// space: radial about `start` out to `end` where `radial` asks or it is
/// radial already, else linear along the line. A gradient keeps its stops
/// and spread; a colour or nothing becomes the start of one of its own.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn laid(
    paint: &Paint,
    (start, end): (Point, Point),
    radial: bool,
) -> Result<Paint, DrawingError> {
    let (stops, spread, round) = match paint {
        Paint::Gradient(gradient) => (
            copied(&gradient.stops)?,
            gradient.spread,
            matches!(gradient.kind, GradientKind::Radial { .. }),
        ),
        Paint::Colour(colour) => (first_stops(*colour)?, Spread::Pad, false),
        Paint::None => (first_stops(Rgba::new(0, 0, 0, 255))?, Spread::Pad, false),
    };
    let kind = if radial || round {
        GradientKind::Radial {
            centre: start,
            radius: fmax(start.distance(end), LEAST_REACH),
            focus: start,
        }
    } else {
        GradientKind::Linear { start, end }
    };
    Ok(Paint::Gradient(try_arc(Gradient {
        kind,
        stops,
        spread,
        transform: Affine::IDENTITY,
    })?))
}

/// `style` with each gradient it paints with fitted to `bounds`, its stops,
/// spread and form kept: the style an object drawn in it takes.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the stops cannot be held.
pub fn fitted_style(style: &Style, bounds: Bounds) -> Result<Style, DrawingError> {
    let mut style = style.clone();
    for paint in [&mut style.fill, &mut style.stroke.paint] {
        if let Paint::Gradient(gradient) = paint {
            let refitted = fitted(
                form_of(gradient),
                copied(&gradient.stops)?,
                gradient.spread,
                bounds,
            );
            *paint = Paint::Gradient(try_arc(refitted)?);
        }
    }
    Ok(style)
}

#[cfg(test)]
#[path = "gradient_tests.rs"]
mod tests;

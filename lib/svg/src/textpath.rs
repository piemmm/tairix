//! `<textPath>`: the path a text is set along, where along it the text
//! starts, and the point and direction a distance along it gives a glyph.

use tairix_bezier::{Path, PathMeasure, Point};

use crate::error::SvgError;
use crate::number::{parse_number, viewport_diagonal};
use crate::pathdata::parse_path_curves;
use crate::shape::{shape_curves, shape_geometry};
use crate::style::{parse_length_in, Lengths};
use crate::transform::parse_transform;
use crate::xml::Element;

/// The path the `<textPath>` `element` is set along, in the text's user
/// space: its own `path` data where it states any, else the `<path>` or
/// basic shape `target` its link names — as SVG 2 makes that shape a path,
/// its own transform applied — with the length the element names states for
/// itself (`pathLength`). `None` where it names nothing a text can be set
/// along, or states empty data, which SVG draws none of its text along.
///
/// `viewport` is what a shape's percentage lengths resolve against.
///
/// # Errors
///
/// [`SvgError::TooComplex`] past `max_commands` commands or points,
/// [`SvgError::OutOfMemory`] when the path cannot be held, and what the
/// path, shape, transform and number grammars refuse.
pub fn geometry(
    element: &Element<'_>,
    target: Option<&Element<'_>>,
    viewport: (f64, f64),
    max_commands: usize,
) -> Result<Option<(Path, Option<f64>)>, SvgError> {
    if let Some(data) = element.attr("path") {
        let path = parse_path_curves(data, max_commands)?;
        return Ok((!path.subpaths.is_empty()).then_some((path, None)));
    }
    let Some(target) = target else {
        return Ok(None);
    };
    let Some(shape) = shape_geometry(target, viewport, max_commands)? else {
        return Ok(None);
    };
    let mut path = shape_curves(&shape, max_commands)?;
    if path.subpaths.is_empty() {
        return Ok(None);
    }
    if let Some(transform) = target.attr("transform") {
        path = path
            .transformed(parse_transform(transform)?)
            .map_err(|_| SvgError::OutOfMemory)?;
    }
    let stated = target.attr("pathLength").map(parse_number).transpose()?;
    Ok(Some((path, stated.filter(|stated| *stated > 0.0))))
}

/// How far along a path `length` long a `<textPath>` `element` set at
/// `font_size` starts its pen: its `startOffset` as a percentage of the
/// length, or as a length — in the path's own units where it `stated` its
/// length.
///
/// # Errors
///
/// [`SvgError::InvalidNumber`] for an offset outside the grammar or one that
/// is not finite.
pub fn start(
    element: &Element<'_>,
    font_size: f64,
    length: f64,
    stated: Option<f64>,
    viewport: (f64, f64),
) -> Result<f64, SvgError> {
    let Some(offset) = element.attr("startOffset").map(str::trim) else {
        return Ok(0.0);
    };
    let start = if let Some(percent) = offset.strip_suffix('%') {
        parse_number(percent)? / 100.0 * length
    } else {
        let lengths = Lengths {
            viewport: viewport_diagonal(viewport),
            font_size,
        };
        let along = parse_length_in(offset, lengths)?;
        stated.map_or(along, |stated| along * length / stated)
    };
    if start.is_finite() {
        Ok(start)
    } else {
        Err(SvgError::InvalidNumber)
    }
}

/// A path measured to set text along, run from its end for `side="right"`.
pub(crate) struct Along {
    measure: PathMeasure,
    reversed: bool,
}

impl Along {
    /// `path`, measured.
    pub(crate) fn new(path: &Path, reversed: bool) -> Result<Self, SvgError> {
        Ok(Self {
            measure: PathMeasure::new(path).map_err(|_| SvgError::OutOfMemory)?,
            reversed,
        })
    }

    /// Its length.
    pub(crate) const fn length(&self) -> f64 {
        self.measure.length()
    }

    /// The point `distance` along it as its text runs, and the unit
    /// direction the text runs in there; `None` off either end.
    pub(crate) fn at(&self, distance: f64) -> Option<(Point, Point)> {
        if self.reversed {
            let (point, direction) = self.measure.at(self.length() - distance)?;
            Some((point, direction * -1.0))
        } else {
            self.measure.at(distance)
        }
    }

    /// As [`at`](Self::at), but beyond either end straight on along that
    /// end's direction, so what bends along the path stays whole past it.
    pub(crate) fn extended(&self, distance: f64) -> Option<(Point, Point)> {
        let held = distance.clamp(0.0, self.length());
        let (point, direction) = self.at(held)?;
        Some((point + direction * (distance - held), direction))
    }
}

#[cfg(test)]
#[path = "textpath_tests.rs"]
mod tests;

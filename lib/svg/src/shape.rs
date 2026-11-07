//! SVG's basic shapes: their attributes resolved into geometry, and that
//! geometry flattened into the one contour currency.
//!
//! [`shape_geometry`] reads a rectangle, circle, ellipse, line, polyline,
//! polygon or path into the geometry its attributes state, every default and
//! clamp SVG prescribes applied, so a reader that keeps shapes as shapes — the
//! drawing editor's — and the decoder that flattens them ([`shape_subpaths`])
//! share one reading of the rules. Flattened, every shape is [`SubPath`]s in
//! user space, so everything downstream sees a single kind of geometry.

use alloc::vec::Vec;

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_bezier::flatten::flatten_ellipse_arc;

use crate::error::SvgError;
use crate::geom::{Point, SubPath, Vertices};
use crate::number::{parse_length, Numbers};
use crate::pathdata::parse_path_data;
use crate::xml::Element;

/// Whether `name` is one of the elements this module draws.
#[must_use]
pub fn is_shape(name: &str) -> bool {
    matches!(
        name,
        "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon"
    )
}

/// Whether markers are drawn on `name`.
///
/// The four shapes whose vertices the author wrote. A `<rect>`, `<circle>`,
/// or `<ellipse>` has none of its own: its outline is *this* module's
/// flattening, so its "vertices" would be tolerance artefacts that move with
/// the scale the shape is drawn at, and a marker would slide along the
/// outline as the asset was rasterised larger.
#[must_use]
pub fn takes_markers(name: &str) -> bool {
    matches!(name, "path" | "line" | "polyline" | "polygon")
}

/// A basic shape's geometry as its attributes state it, in its own user
/// space.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeGeometry<'a> {
    /// A `<path>`'s `d`, read by [`crate::pathdata`].
    Path(&'a str),
    /// A `<rect>`: its corner radii resolved (`auto` and an absent radius
    /// taking the other's) and each held to half its side.
    Rect {
        /// The left edge.
        x: f64,
        /// The top edge.
        y: f64,
        /// The width, not negative.
        width: f64,
        /// The height, not negative.
        height: f64,
        /// The corners' horizontal radius.
        rx: f64,
        /// The corners' vertical radius.
        ry: f64,
    },
    /// A `<circle>` or `<ellipse>`.
    Ellipse {
        /// The centre.
        centre: Point,
        /// The radii, not negative; a circle's are equal.
        radii: Point,
    },
    /// A `<line>`.
    Line {
        /// Where it starts.
        from: Point,
        /// Where it ends.
        to: Point,
    },
    /// A `<polyline>`, or a `<polygon>` when `closed`.
    Polyline {
        /// Its points, in order.
        points: Vec<Point>,
        /// Whether it is a polygon.
        closed: bool,
    },
}

/// The geometry `node` states, or `None` when it is not a basic shape.
///
/// `viewport` is what percentage lengths resolve against, and `max_points`
/// bounds a poly-shape's points.
///
/// # Errors
/// Returns the parse error of a malformed attribute, [`SvgError::InvalidNumber`]
/// for a negative extent or radius, which is not geometry the author can have
/// meant, and [`SvgError::TooComplex`] for more than `max_points` points.
pub fn shape_geometry<'a>(
    node: &'a Element<'a>,
    viewport: (f64, f64),
    max_points: usize,
) -> Result<Option<ShapeGeometry<'a>>, SvgError> {
    let geometry = match node.name {
        "path" => ShapeGeometry::Path(node.attr("d").unwrap_or("")),
        "rect" => rect_geometry(node, viewport)?,
        "circle" => {
            let r = length(node, "r", diagonal(viewport))?;
            if r < 0.0 {
                return Err(SvgError::InvalidNumber);
            }
            ShapeGeometry::Ellipse {
                centre: (
                    length(node, "cx", viewport.0)?,
                    length(node, "cy", viewport.1)?,
                ),
                radii: (r, r),
            }
        }
        "ellipse" => {
            let (rx, ry) = radii(node, viewport)?;
            ShapeGeometry::Ellipse {
                centre: (
                    length(node, "cx", viewport.0)?,
                    length(node, "cy", viewport.1)?,
                ),
                radii: (rx, ry),
            }
        }
        "line" => ShapeGeometry::Line {
            from: (
                length(node, "x1", viewport.0)?,
                length(node, "y1", viewport.1)?,
            ),
            to: (
                length(node, "x2", viewport.0)?,
                length(node, "y2", viewport.1)?,
            ),
        },
        "polyline" | "polygon" => ShapeGeometry::Polyline {
            points: poly_points(node, max_points)?,
            closed: node.name == "polygon",
        },
        _ => return Ok(None),
    };
    Ok(Some(geometry))
}

/// Flatten one basic shape into sub-paths in its own user space.
///
/// A shape that encloses nothing — a zero extent, a zero radius — yields an
/// empty list rather than an error, because SVG defines those as simply not
/// rendering.
///
/// `vertices` collects where markers go, for the shapes that take them
/// ([`takes_markers`]). The others leave it untouched, so a rectangle pays
/// nothing for a feature it cannot carry.
///
/// # Errors
/// As [`shape_geometry`], and [`SvgError::TooComplex`] once the shape would
/// exceed `max_points`.
pub fn shape_subpaths(
    node: &Element<'_>,
    viewport: (f64, f64),
    tolerance: f64,
    max_points: usize,
    vertices: Option<&mut Vertices>,
) -> Result<Vec<SubPath>, SvgError> {
    let Some(geometry) = shape_geometry(node, viewport, max_points)? else {
        return Ok(Vec::new());
    };
    match geometry {
        ShapeGeometry::Path("") => Ok(Vec::new()),
        ShapeGeometry::Path(data) => parse_path_data(data, tolerance, max_points, vertices),
        ShapeGeometry::Rect {
            x,
            y,
            width,
            height,
            rx,
            ry,
        } => Ok(rect_outline(x, y, width, height, (rx, ry), tolerance)),
        ShapeGeometry::Ellipse { centre, radii } => Ok(full_ellipse(centre, radii, tolerance)),
        ShapeGeometry::Line { from, to } => {
            if let Some(vertices) = vertices {
                vertices.move_to(from)?;
                vertices.line_to(to)?;
            }
            Ok(alloc::vec![SubPath::open(alloc::vec![from, to])])
        }
        ShapeGeometry::Polyline { points, closed } => {
            if points.is_empty() {
                return Ok(Vec::new());
            }
            // Every point of a poly-shape is a command vertex, and its
            // closure joins the two ends exactly as a path's `Z` does — so
            // the same builder states the rule once for both.
            if let Some(vertices) = vertices {
                let mut walk = points.iter().copied();
                if let Some(first) = walk.next() {
                    vertices.move_to(first)?;
                    for point in walk {
                        vertices.line_to(point)?;
                    }
                    if closed {
                        vertices.close()?;
                    }
                }
            }
            Ok(alloc::vec![SubPath { points, closed }])
        }
    }
}

/// One length attribute, resolved against `basis`, defaulting to zero.
fn length(node: &Element<'_>, name: &str, basis: f64) -> Result<f64, SvgError> {
    match node.attr(name) {
        Some(text) => parse_length(text, basis),
        None => Ok(0.0),
    }
}

/// One radius attribute, which may also be the keyword `auto` meaning "use
/// the other axis".
fn radius(node: &Element<'_>, name: &str, basis: f64) -> Result<Option<f64>, SvgError> {
    match node.attr(name) {
        None | Some("auto") => Ok(None),
        Some(text) => parse_length(text, basis).map(Some),
    }
}

/// An element's `rx` and `ry`, an absent or `auto` one taking the other's,
/// both refused when negative.
fn radii(node: &Element<'_>, viewport: (f64, f64)) -> Result<(f64, f64), SvgError> {
    let (rx, ry) = match (
        radius(node, "rx", viewport.0)?,
        radius(node, "ry", viewport.1)?,
    ) {
        (None, None) => (0.0, 0.0),
        (Some(rx), None) => (rx, rx),
        (None, Some(ry)) => (ry, ry),
        (Some(rx), Some(ry)) => (rx, ry),
    };
    if rx < 0.0 || ry < 0.0 {
        return Err(SvgError::InvalidNumber);
    }
    Ok((rx, ry))
}

/// The length a radius with no axis of its own resolves a percentage against.
fn diagonal(viewport: (f64, f64)) -> f64 {
    tairix_util::mathf::sqrt(f64::midpoint(
        viewport.0 * viewport.0,
        viewport.1 * viewport.1,
    ))
}

/// `<rect>`'s geometry, with SVG's rounded-corner rules: neither radius may
/// exceed half its side — SVG clamps rather than letting the corners meet and
/// invert.
fn rect_geometry<'a>(
    node: &Element<'_>,
    viewport: (f64, f64),
) -> Result<ShapeGeometry<'a>, SvgError> {
    let x = length(node, "x", viewport.0)?;
    let y = length(node, "y", viewport.1)?;
    let width = length(node, "width", viewport.0)?;
    let height = length(node, "height", viewport.1)?;
    if width < 0.0 || height < 0.0 {
        return Err(SvgError::InvalidNumber);
    }
    let (rx, ry) = radii(node, viewport)?;
    Ok(ShapeGeometry::Rect {
        x,
        y,
        width,
        height,
        rx: rx.min(width / 2.0),
        ry: ry.min(height / 2.0),
    })
}

/// A rectangle's outline, its corners flattened quarter ellipses.
fn rect_outline(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    (rx, ry): (f64, f64),
    tolerance: f64,
) -> Vec<SubPath> {
    if w == 0.0 || h == 0.0 {
        return Vec::new();
    }
    if rx == 0.0 || ry == 0.0 {
        return alloc::vec![SubPath::closed(alloc::vec![
            (x, y),
            (x + w, y),
            (x + w, y + h),
            (x, y + h),
        ])];
    }
    let mut outline = Vec::new();
    outline.push((x + rx, y));
    outline.push((x + w - rx, y));
    let corners = [
        ((x + w - rx, y + ry), -FRAC_PI_2),
        ((x + w - rx, y + h - ry), 0.0),
        ((x + rx, y + h - ry), FRAC_PI_2),
        ((x + rx, y + ry), PI),
    ];
    for (index, (centre, start)) in corners.into_iter().enumerate() {
        flatten_ellipse_arc(
            centre.into(),
            (rx, ry).into(),
            0.0,
            start,
            FRAC_PI_2,
            tolerance,
            |point| outline.push(point.tuple()),
        );
        // The straight side that follows each corner but the last, which the
        // closing of the contour supplies.
        match index {
            0 => outline.push((x + w, y + h - ry)),
            1 => outline.push((x + rx, y + h)),
            2 => outline.push((x, y + ry)),
            _ => {}
        }
    }
    alloc::vec![SubPath::closed(outline)]
}

/// A whole ellipse as one closed contour, or nothing when it has no area.
fn full_ellipse(centre: Point, radii: Point, tolerance: f64) -> Vec<SubPath> {
    if radii.0 == 0.0 || radii.1 == 0.0 {
        return Vec::new();
    }
    let mut outline = alloc::vec![(centre.0 + radii.0, centre.1)];
    flatten_ellipse_arc(
        centre.into(),
        radii.into(),
        0.0,
        0.0,
        TAU,
        tolerance,
        |point| {
            outline.push(point.tuple());
        },
    );
    // The sweep closes on its own start point, which the contour's implicit
    // closure already supplies.
    if outline.len() > 1 {
        outline.pop();
    }
    alloc::vec![SubPath::closed(outline)]
}

/// The points of a `<polyline>` or `<polygon>`.
fn poly_points(node: &Element<'_>, max_points: usize) -> Result<Vec<Point>, SvgError> {
    let Some(text) = node.attr("points") else {
        return Ok(Vec::new());
    };
    let mut numbers = Numbers::new(text);
    let mut list = Vec::new();
    while let Some(x) = numbers.take()? {
        if list.len() == max_points {
            return Err(SvgError::TooComplex);
        }
        // An odd trailing coordinate ends the list in SVG rather than
        // invalidating it, but a malformed one is still refused.
        let Some(y) = numbers.take()? else {
            break;
        };
        list.push((x, y));
    }
    Ok(list)
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;

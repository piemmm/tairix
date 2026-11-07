//! The `d` attribute: SVG's path grammar, its commands, and flattening them.
//!
//! A path is a run of commands, each optionally repeated by simply writing
//! another parameter set after it, with coordinates that may be run together
//! wherever a sign or a second decimal point makes the boundary unambiguous
//! (`M0-4L2-6`, `.5.5`). [`path_commands`] reads all of that into commands
//! whose coordinates are absolute and whose shorthand is spelled out, so a
//! reader that keeps the curves — the drawing editor's — and the decoder that
//! flattens them ([`parse_path_data`]) share one reading of the grammar.
//!
//! Flattening is `tairix_bezier`'s, the one flattener every consumer draws
//! through.

use alloc::vec::Vec;

use core::f64::consts::PI;

use tairix_bezier::flatten::{
    clamp_tolerance, flatten_cubic, flatten_ellipse_arc, flatten_quadratic,
};
use tairix_bezier::{CubicBez, EllipseArc, QuadBez};

use crate::error::SvgError;
use crate::geom::{delta, Point, SubPath, Vertices};
use crate::number::Numbers;

/// One command of a path, absolute and with its shorthand spelled out: `H`
/// and `V` are lines, `S` and `T` carry the control their reflection
/// implies, and an arc's rotation is in radians.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PathCommand {
    /// Begin a sub-path at the point.
    MoveTo(Point),
    /// A straight segment to the point.
    LineTo(Point),
    /// A cubic Bézier through the two controls to the last point.
    CubicTo(Point, Point, Point),
    /// A quadratic Bézier through the control to the point.
    QuadTo(Point, Point),
    /// SVG's endpoint elliptical arc to `to`.
    ArcTo {
        /// The ellipse's radii, as written.
        radii: Point,
        /// The turn of its x axis, in radians.
        x_rotation: f64,
        /// The large-arc flag.
        large: bool,
        /// The sweep flag.
        sweep: bool,
        /// Where it ends.
        to: Point,
    },
    /// Close the sub-path; the pen returns to where it began.
    Close,
}

/// The commands of a `d` attribute, in order ([`path_commands`]).
///
/// Each item is a command or the reason the path is malformed; after a
/// refusal nothing further is read, so a path is taken whole or not at all.
pub struct PathCommands<'a> {
    numbers: Numbers<'a>,
    previous: Option<char>,
    started: bool,
    finished: bool,
    start: Point,
    cursor: Point,
    /// The previous command's second cubic control point, which `S` reflects.
    cubic_control: Option<Point>,
    /// The previous command's quadratic control point, which `T` reflects.
    quadratic_control: Option<Point>,
}

/// Read the `d` attribute `d` as commands.
///
/// A path must begin with a `moveto`; a bare parameter set repeats the
/// command before it, a `moveto`'s as a `lineto`; a `closepath` takes no
/// parameters and so cannot be repeated implicitly.
#[must_use]
pub fn path_commands(d: &str) -> PathCommands<'_> {
    PathCommands {
        numbers: Numbers::new(d),
        previous: None,
        started: false,
        finished: false,
        start: (0.0, 0.0),
        cursor: (0.0, 0.0),
        cubic_control: None,
        quadratic_control: None,
    }
}

impl Iterator for PathCommands<'_> {
    type Item = Result<PathCommand, SvgError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.numbers.is_exhausted() {
            return None;
        }
        let read = self.read();
        if read.is_err() {
            self.finished = true;
        }
        Some(read)
    }
}

impl PathCommands<'_> {
    /// Read one command and move the pen past it.
    fn read(&mut self) -> Result<PathCommand, SvgError> {
        let letter = match self.numbers.take_letter() {
            Some(letter) => {
                self.previous = Some(letter);
                letter
            }
            None => match self.previous {
                Some('M') => 'L',
                Some('m') => 'l',
                Some(letter) => letter,
                None => return Err(SvgError::UnsupportedPath),
            },
        };
        if !self.started && !matches!(letter, 'M' | 'm') {
            return Err(SvgError::UnsupportedPath);
        }
        let relative = letter.is_ascii_lowercase();
        let command = match letter.to_ascii_uppercase() {
            'M' => PathCommand::MoveTo(self.point(relative)?),
            'L' => PathCommand::LineTo(self.point(relative)?),
            'H' => {
                let x = self.numbers.required()?;
                let x = if relative { self.cursor.0 + x } else { x };
                PathCommand::LineTo((x, self.cursor.1))
            }
            'V' => {
                let y = self.numbers.required()?;
                let y = if relative { self.cursor.1 + y } else { y };
                PathCommand::LineTo((self.cursor.0, y))
            }
            'C' => {
                let c1 = self.point(relative)?;
                let c2 = self.point(relative)?;
                PathCommand::CubicTo(c1, c2, self.point(relative)?)
            }
            'S' => {
                let c1 = reflect(self.cursor, self.cubic_control);
                let c2 = self.point(relative)?;
                PathCommand::CubicTo(c1, c2, self.point(relative)?)
            }
            'Q' => {
                let control = self.point(relative)?;
                PathCommand::QuadTo(control, self.point(relative)?)
            }
            'T' => {
                let control = reflect(self.cursor, self.quadratic_control);
                PathCommand::QuadTo(control, self.point(relative)?)
            }
            'A' => {
                let rx = self.numbers.required()?;
                let ry = self.numbers.required()?;
                let rotation = self.numbers.required()?;
                let large = self.numbers.required_flag()?;
                let sweep = self.numbers.required_flag()?;
                PathCommand::ArcTo {
                    radii: (rx, ry),
                    x_rotation: rotation * PI / 180.0,
                    large,
                    sweep,
                    to: self.point(relative)?,
                }
            }
            'Z' => {
                // A closepath consumes nothing, so it can never repeat.
                self.previous = None;
                PathCommand::Close
            }
            _ => return Err(SvgError::UnsupportedPath),
        };
        self.advance(command);
        Ok(command)
    }

    /// A parameter pair as an absolute point.
    fn point(&mut self, relative: bool) -> Result<Point, SvgError> {
        let (x, y) = (self.numbers.required()?, self.numbers.required()?);
        Ok(if relative {
            (self.cursor.0 + x, self.cursor.1 + y)
        } else {
            (x, y)
        })
    }

    /// Move the pen past `command`, keeping only the control the next
    /// command of the same kind may reflect.
    fn advance(&mut self, command: PathCommand) {
        self.cubic_control = None;
        self.quadratic_control = None;
        match command {
            PathCommand::MoveTo(point) => {
                self.started = true;
                self.start = point;
                self.cursor = point;
            }
            PathCommand::LineTo(to) | PathCommand::ArcTo { to, .. } => self.cursor = to,
            PathCommand::CubicTo(_, c2, to) => {
                self.cubic_control = Some(c2);
                self.cursor = to;
            }
            PathCommand::QuadTo(control, to) => {
                self.quadratic_control = Some(control);
                self.cursor = to;
            }
            PathCommand::Close => self.cursor = self.start,
        }
    }
}

/// `control` mirrored about `about`, or `about` itself when there is no
/// control point to mirror.
fn reflect(about: Point, control: Option<Point>) -> Point {
    match control {
        Some(previous) => (2.0 * about.0 - previous.0, 2.0 * about.1 - previous.1),
        None => about,
    }
}

/// Parse a `d` attribute into flattened sub-paths in user space.
///
/// `tolerance` is the greatest distance a flattened curve may depart from the
/// true one, in the same user units as the coordinates. `max_points` bounds
/// the whole path: a document that would exceed it is refused rather than
/// allowed to allocate without end.
///
/// A sub-path of a single point (`M10,10Z`) is kept: it draws nothing when
/// filled but is a legitimate round-capped dot when stroked, so the decision
/// belongs to the caller.
///
/// `vertices` collects the *command* vertices and the path's true direction
/// through each, which markers are placed at. It is opt-in because only a
/// shape that references a marker has any use for them.
///
/// # Errors
/// Returns [`SvgError::UnsupportedPath`] for an unknown command or a path
/// that does not begin with a `moveto`, [`SvgError::InvalidNumber`] for a
/// malformed or missing parameter, and [`SvgError::TooComplex`] once
/// `max_points` would be exceeded. A path that fails anywhere is refused
/// whole; nothing half-parsed is drawn.
pub fn parse_path_data(
    d: &str,
    tolerance: f64,
    max_points: usize,
    vertices: Option<&mut Vertices>,
) -> Result<Vec<SubPath>, SvgError> {
    let mut builder = Builder::new(clamp_tolerance(tolerance), max_points, vertices);
    for command in path_commands(d) {
        match command? {
            PathCommand::MoveTo(point) => builder.move_to(point)?,
            PathCommand::LineTo(point) => builder.line_to(point)?,
            PathCommand::CubicTo(c1, c2, to) => builder.cubic_to(c1, c2, to)?,
            PathCommand::QuadTo(control, to) => builder.quadratic_to(control, to)?,
            PathCommand::ArcTo {
                radii,
                x_rotation,
                large,
                sweep,
                to,
            } => builder.arc_to(radii, x_rotation, large, sweep, to)?,
            PathCommand::Close => builder.close()?,
        }
    }
    Ok(builder.finish())
}

/// The sub-paths built so far and the pen state the next command starts from.
struct Builder<'v> {
    subpaths: Vec<SubPath>,
    current: Vec<Point>,
    start: Point,
    cursor: Point,
    /// Where the command vertices go, for a caller that draws markers.
    vertices: Option<&'v mut Vertices>,
    tolerance: f64,
    points_left: usize,
}

impl Builder<'_> {
    fn new(tolerance: f64, max_points: usize, vertices: Option<&mut Vertices>) -> Builder<'_> {
        Builder {
            subpaths: Vec::new(),
            current: Vec::new(),
            start: (0.0, 0.0),
            cursor: (0.0, 0.0),
            vertices,
            tolerance,
            points_left: max_points,
        }
    }

    /// Begin a new sub-path at `point`.
    fn move_to(&mut self, point: Point) -> Result<(), SvgError> {
        self.flush(false);
        self.start = point;
        self.cursor = point;
        self.charge()?;
        self.current.push(point);
        if let Some(vertices) = &mut self.vertices {
            vertices.move_to(point)?;
        }
        Ok(())
    }

    /// Extend the current sub-path to `point`.
    fn line_to(&mut self, point: Point) -> Result<(), SvgError> {
        self.push(point)?;
        if let Some(vertices) = &mut self.vertices {
            vertices.line_to(point)?;
        }
        Ok(())
    }

    /// Extend the current sub-path along a cubic curve.
    fn cubic_to(&mut self, c1: Point, c2: Point, to: Point) -> Result<(), SvgError> {
        let curve = CubicBez::new(self.cursor.into(), c1.into(), c2.into(), to.into());
        let mut points = Vec::new();
        flatten_cubic(curve, self.tolerance, |point| points.push(point.tuple()));
        self.extend(points)?;
        let (leaving, arriving) = curve.tangents();
        self.curve_vertex(to, leaving.tuple(), arriving.tuple())
    }

    /// Extend the current sub-path along a quadratic curve.
    fn quadratic_to(&mut self, control: Point, to: Point) -> Result<(), SvgError> {
        let curve = QuadBez::new(self.cursor.into(), control.into(), to.into());
        let mut points = Vec::new();
        flatten_quadratic(curve, self.tolerance, |point| points.push(point.tuple()));
        self.extend(points)?;
        let (leaving, arriving) = curve.tangents();
        self.curve_vertex(to, leaving.tuple(), arriving.tuple())
    }

    /// Extend the current sub-path along an elliptical arc.
    fn arc_to(
        &mut self,
        radii: Point,
        rotation: f64,
        large: bool,
        sweep: bool,
        to: Point,
    ) -> Result<(), SvgError> {
        let from = self.cursor;
        let mut points = Vec::new();
        let arc =
            EllipseArc::from_svg(from.into(), to.into(), radii.into(), rotation, large, sweep);
        let tangents = match arc {
            // A zero radius, or an arc that ends where it began, degrades to
            // a straight line, exactly as the specification requires.
            None => {
                points.push(to);
                let chord = delta(from, to);
                (chord, chord)
            }
            Some(arc) => {
                flatten_ellipse_arc(
                    arc.centre,
                    arc.radii,
                    rotation,
                    arc.start,
                    arc.sweep,
                    self.tolerance,
                    |point| points.push(point.tuple()),
                );
                // The parameterisation reproduces the endpoint only to
                // rounding; landing exactly on it keeps a following segment
                // continuous.
                if let Some(last) = points.last_mut() {
                    *last = to;
                }
                let (leaving, arriving) = arc.tangents();
                (leaving.tuple(), arriving.tuple())
            }
        };
        self.extend(points)?;
        self.curve_vertex(to, tangents.0, tangents.1)
    }

    /// Close the current sub-path; the pen returns to where it began.
    fn close(&mut self) -> Result<(), SvgError> {
        self.flush(true);
        self.cursor = self.start;
        if let Some(vertices) = &mut self.vertices {
            vertices.close()?;
        }
        Ok(())
    }

    /// Record the command vertex a curved segment ends at.
    fn curve_vertex(&mut self, to: Point, leaving: Point, arriving: Point) -> Result<(), SvgError> {
        match &mut self.vertices {
            Some(vertices) => vertices.curve_to(to, leaving, arriving),
            None => Ok(()),
        }
    }

    /// Take one point from the budget.
    fn charge(&mut self) -> Result<(), SvgError> {
        if self.points_left == 0 {
            return Err(SvgError::TooComplex);
        }
        self.points_left -= 1;
        Ok(())
    }

    /// Append one point, charging it against the budget.
    ///
    /// A segment that follows a `closepath` without an intervening `moveto`
    /// begins a fresh sub-path at the point the pen returned to, so the
    /// contour does not silently lose its first vertex.
    fn push(&mut self, point: Point) -> Result<(), SvgError> {
        if self.current.is_empty() {
            self.charge()?;
            self.current.push(self.cursor);
        }
        self.charge()?;
        self.current.push(point);
        self.cursor = point;
        Ok(())
    }

    /// Append a flattened run of points.
    fn extend(&mut self, points: Vec<Point>) -> Result<(), SvgError> {
        for point in points {
            self.push(point)?;
        }
        Ok(())
    }

    /// End the current sub-path, if there is one.
    fn flush(&mut self, closed: bool) {
        if self.current.is_empty() {
            return;
        }
        let points = core::mem::take(&mut self.current);
        self.subpaths.push(SubPath { points, closed });
    }

    /// The finished sub-paths.
    fn finish(mut self) -> Vec<SubPath> {
        self.flush(false);
        self.subpaths
    }
}

#[cfg(test)]
#[path = "pathdata_tests.rs"]
mod tests;

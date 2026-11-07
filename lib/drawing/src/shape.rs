//! An object's geometry: an editable path, or a live shape that keeps the
//! parameters it was drawn by until it is made a path.

use alloc::sync::Arc;
use alloc::vec::Vec;

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_bezier::{Bounds, EllipseArc, Path, PathEl, Point, Subpath};
use tairix_util::fallible;
use tairix_util::mathf::fmin;

use crate::limits::{self, MAX_PATH_NODES, MAX_STAR_POINTS};
use crate::DrawingError;

/// How the node tool keeps a path node's two handles when one moves.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum NodeJoin {
    /// Each handle moves alone.
    #[default]
    Corner,
    /// The handles stay on one line through the node, each its own length.
    Smooth,
    /// The handles stay opposite and equally long.
    Symmetric,
}

/// One node of a path: a point the path passes through, and the handles the
/// curves either side of it leave and arrive along. A side with no handle is
/// straight unless the neighbouring node's handle bends it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PathNode {
    /// Where the path passes.
    pub point: Point,
    /// The control the segment arriving here was drawn from.
    pub before: Option<Point>,
    /// The control the segment leaving here is drawn towards.
    pub after: Option<Point>,
    /// How its handles are kept.
    pub join: NodeJoin,
}

impl PathNode {
    /// A corner node at `point` with no handles.
    #[must_use]
    pub const fn corner(point: Point) -> Self {
        Self {
            point,
            before: None,
            after: None,
            join: NodeJoin::Corner,
        }
    }
}

/// One run of a path's nodes, open or closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    /// Its nodes, in order.
    pub nodes: Vec<PathNode>,
    /// Whether a segment joins its last node back to its first.
    pub closed: bool,
}

/// An editable path: its contours.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathShape {
    /// Its contours.
    pub contours: Vec<Contour>,
}

impl PathShape {
    /// How many nodes it holds.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.contours
            .iter()
            .map(|contour| contour.nodes.len())
            .sum()
    }

    /// Whether every point is one a drawing may hold, and it holds no more
    /// than [`MAX_PATH_NODES`].
    ///
    /// # Errors
    ///
    /// [`DrawingError::TooComplex`] past the bound, [`DrawingError::Invalid`]
    /// for a point that is not.
    pub fn check(&self) -> Result<(), DrawingError> {
        if self.node_count() > MAX_PATH_NODES {
            return Err(DrawingError::TooComplex);
        }
        let held = |point: Option<Point>| point.is_none_or(limits::point);
        let all = self
            .contours
            .iter()
            .flat_map(|c| &c.nodes)
            .all(|node| limits::point(node.point) && held(node.before) && held(node.after));
        if all {
            Ok(())
        } else {
            Err(DrawingError::Invalid)
        }
    }

    /// These contours with the node at `index` of contour `contour` replaced
    /// by `node`.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] where no node stands there, and
    /// [`DrawingError::OutOfMemory`] when the copy cannot be held.
    pub fn with_node(
        &self,
        contour: usize,
        index: usize,
        node: PathNode,
    ) -> Result<Self, DrawingError> {
        let there = self
            .contours
            .get(contour)
            .is_some_and(|held| index < held.nodes.len());
        if !there {
            return Err(DrawingError::Invalid);
        }
        let mut contours = Vec::new();
        if !fallible::reserve(&mut contours, self.contours.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        for (at, each) in self.contours.iter().enumerate() {
            let mut nodes = fallible::copied(&each.nodes).ok_or(DrawingError::OutOfMemory)?;
            if at == contour {
                nodes[index] = node;
            }
            contours.push(Contour {
                nodes,
                closed: each.closed,
            });
        }
        Ok(Self { contours })
    }

    /// The path these contours trace.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when it cannot be held.
    pub fn to_path(&self) -> Result<Path, DrawingError> {
        let mut path = Path::new();
        if !fallible::reserve(&mut path.subpaths, self.contours.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        for contour in &self.contours {
            let Some(first) = contour.nodes.first() else {
                continue;
            };
            let mut subpath = Subpath::new(first.point);
            if !fallible::reserve(&mut subpath.elements, contour.nodes.len()) {
                return Err(DrawingError::OutOfMemory);
            }
            for pair in contour.nodes.windows(2) {
                subpath.elements.push(segment(&pair[0], &pair[1]));
            }
            let count = contour.nodes.len();
            if contour.closed && count > 1 {
                let closing = segment(&contour.nodes[count - 1], first);
                // A straight closing side is the sub-path's own join back;
                // a curved one is drawn explicitly.
                if matches!(closing, PathEl::Cubic(..)) {
                    subpath.elements.push(closing);
                }
            }
            subpath.closed = contour.closed;
            path.subpaths.push(subpath);
        }
        Ok(path)
    }

    /// `path` as editable contours: each step's controls become handles — a
    /// control lying on its own node none — and a node whose handles lie
    /// opposite each other is smooth, symmetric where they are as long.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when it cannot be held.
    pub fn from_path(path: &Path) -> Result<Self, DrawingError> {
        let mut shape = Self::default();
        if !fallible::reserve(&mut shape.contours, path.subpaths.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        for subpath in &path.subpaths {
            let mut nodes: Vec<PathNode> = Vec::new();
            if !fallible::reserve(&mut nodes, subpath.elements.len() + 1) {
                return Err(DrawingError::OutOfMemory);
            }
            nodes.push(PathNode::corner(subpath.start));
            for element in &subpath.elements {
                match *element {
                    PathEl::Line(to) => nodes.push(PathNode::corner(to)),
                    PathEl::Cubic(c1, c2, to) => {
                        // A control on its own node is a handle of no length:
                        // no handle at all.
                        if let Some(last) = nodes.last_mut() {
                            last.after = (c1 != last.point).then_some(c1);
                        }
                        nodes.push(PathNode {
                            before: (c2 != to).then_some(c2),
                            ..PathNode::corner(to)
                        });
                    }
                }
            }
            // A closed contour returning to its start has one node there.
            if subpath.closed && nodes.len() > 1 && nodes[nodes.len() - 1].point == subpath.start {
                if let Some(last) = nodes.pop() {
                    nodes[0].before = last.before;
                }
            }
            for node in &mut nodes {
                node.join = inferred_join(node);
            }
            shape.contours.push(Contour {
                nodes,
                closed: subpath.closed,
            });
        }
        Ok(shape)
    }
}

/// The segment from node `a` to node `b`.
fn segment(a: &PathNode, b: &PathNode) -> PathEl {
    if a.after.is_none() && b.before.is_none() {
        PathEl::Line(b.point)
    } else {
        PathEl::Cubic(
            a.after.unwrap_or(a.point),
            b.before.unwrap_or(b.point),
            b.point,
        )
    }
}

/// The join a node's handles show: smooth where they lie on one line through
/// it on opposite sides, symmetric where they are also as long, else a
/// corner.
fn inferred_join(node: &PathNode) -> NodeJoin {
    let (Some(before), Some(after)) = (node.before, node.after) else {
        return NodeJoin::Corner;
    };
    let (incoming, outgoing) = (node.point - before, after - node.point);
    let (Some(a), Some(b)) = (incoming.normalised(), outgoing.normalised()) else {
        return NodeJoin::Corner;
    };
    if a.dot(b) < 0.999_999 || a.cross(b).abs() > 1e-6 {
        return NodeJoin::Corner;
    }
    let (la, lb) = (incoming.length(), outgoing.length());
    if (la - lb).abs() <= 1e-9 * (la + lb) {
        NodeJoin::Symmetric
    } else {
        NodeJoin::Smooth
    }
}

/// A rectangle with rounded corners, `rx` and `ry` held to half its width and
/// height as SVG holds them.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RectShape {
    /// Its left edge.
    pub x: f64,
    /// Its top edge.
    pub y: f64,
    /// Its width.
    pub width: f64,
    /// Its height.
    pub height: f64,
    /// The corners' horizontal radius.
    pub rx: f64,
    /// The corners' vertical radius.
    pub ry: f64,
}

/// Which part of an ellipse an arc draws.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ArcKind {
    /// The arc alone, open.
    #[default]
    Open,
    /// The arc closed through the centre.
    Pie,
    /// The arc closed straight across.
    Chord,
}

/// The part of an ellipse drawn, by angle clockwise as drawn from the
/// positive x axis, in degrees.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ArcPart {
    /// Where the arc starts.
    pub start: f64,
    /// Where it ends.
    pub end: f64,
    /// How it is closed.
    pub kind: ArcKind,
}

/// An ellipse, or an arc of one.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EllipseShape {
    /// Its centre.
    pub centre: Point,
    /// Its radii.
    pub radii: Point,
    /// The part drawn, or `None` for the whole.
    pub arc: Option<ArcPart>,
}

/// A regular star, or a regular polygon.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StarShape {
    /// Its centre.
    pub centre: Point,
    /// How many points it has.
    pub points: u32,
    /// The distance of its points from the centre.
    pub outer: f64,
    /// The distance of the valleys between its points, as a share of the
    /// outer distance.
    pub inner_ratio: f64,
    /// The turn of its first point from straight up, in degrees clockwise.
    pub rotation: f64,
    /// How far each corner is rounded, `0..=1`.
    pub rounding: f64,
    /// Whether it is a polygon: points with no valleys between.
    pub polygon: bool,
}

/// An object's geometry.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// An editable path.
    Path(Arc<PathShape>),
    /// A rectangle.
    Rect(RectShape),
    /// An ellipse or an arc.
    Ellipse(EllipseShape),
    /// A star or a polygon.
    Star(StarShape),
}

impl Shape {
    /// The path nodes it holds: none for a live shape, which is held as its
    /// parameters.
    #[must_use]
    pub fn node_count(&self) -> usize {
        match self {
            Self::Path(path) => path.node_count(),
            Self::Rect(_) | Self::Ellipse(_) | Self::Star(_) => 0,
        }
    }

    /// Whether every value is one a drawing may hold.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] or [`DrawingError::TooComplex`] for the
    /// first value that is not.
    pub fn check(&self) -> Result<(), DrawingError> {
        let valid = match self {
            Self::Path(path) => return path.check(),
            Self::Rect(rect) => {
                limits::coordinate(rect.x)
                    && limits::coordinate(rect.y)
                    && [rect.width, rect.height, rect.rx, rect.ry]
                        .iter()
                        .all(|&value| limits::length(value))
            }
            Self::Ellipse(ellipse) => {
                limits::point(ellipse.centre)
                    && limits::length(ellipse.radii.x)
                    && limits::length(ellipse.radii.y)
                    && ellipse.arc.is_none_or(|arc| {
                        arc.start.is_finite()
                            && arc.end.is_finite()
                            && arc.start.abs() <= 1e6
                            && arc.end.abs() <= 1e6
                    })
            }
            Self::Star(star) => {
                if star.points > MAX_STAR_POINTS {
                    return Err(DrawingError::TooComplex);
                }
                limits::point(star.centre)
                    && star.points >= 3
                    && limits::length(star.outer)
                    && (0.0..=1.0).contains(&star.inner_ratio)
                    && (0.0..=1.0).contains(&star.rounding)
                    && star.rotation.is_finite()
                    && star.rotation.abs() <= 1e6
            }
        };
        if valid {
            Ok(())
        } else {
            Err(DrawingError::Invalid)
        }
    }

    /// The path it traces in its object's space.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when it cannot be held.
    pub fn to_path(&self) -> Result<Path, DrawingError> {
        match self {
            Self::Path(path) => path.to_path(),
            Self::Rect(rect) => Ok(rect_path(rect)),
            Self::Ellipse(ellipse) => Ok(ellipse_path(ellipse)),
            Self::Star(star) => star_path(star),
        }
    }

    /// The tight bounds of its outline, in its object's space.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the outline cannot be traced.
    pub fn bounds(&self) -> Result<Bounds, DrawingError> {
        match self {
            Self::Rect(rect) => Ok(Bounds::new(
                Point::new(rect.x, rect.y),
                Point::new(rect.x + rect.width, rect.y + rect.height),
            )),
            Self::Ellipse(EllipseShape {
                centre,
                radii,
                arc: None,
            }) => Ok(Bounds::new(*centre - *radii, *centre + *radii)),
            _ => Ok(self.to_path()?.bounds()),
        }
    }
}

/// The rectangle's outline, clockwise as drawn from the start of its top
/// edge, its corners quarter ellipses.
fn rect_path(rect: &RectShape) -> Path {
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
    let rx = fmin(rect.rx, w / 2.0);
    let ry = fmin(rect.ry, h / 2.0);
    let mut subpath = Subpath::new(Point::new(x + rx, y));
    if rx <= 0.0 || ry <= 0.0 {
        subpath.start = Point::new(x, y);
        for corner in [
            Point::new(x + w, y),
            Point::new(x + w, y + h),
            Point::new(x, y + h),
        ] {
            subpath.elements.push(PathEl::Line(corner));
        }
    } else {
        let corners = [
            (
                Point::new(x + w - rx, y + ry),
                -FRAC_PI_2,
                Point::new(x + w, y + h - ry),
            ),
            (
                Point::new(x + w - rx, y + h - ry),
                0.0,
                Point::new(x + rx, y + h),
            ),
            (
                Point::new(x + rx, y + h - ry),
                FRAC_PI_2,
                Point::new(x, y + ry),
            ),
            (Point::new(x + rx, y + ry), PI, Point::new(x + rx, y)),
        ];
        subpath
            .elements
            .push(PathEl::Line(Point::new(x + w - rx, y)));
        for (index, (centre, start, onward)) in corners.into_iter().enumerate() {
            let arc = EllipseArc {
                centre,
                radii: Point::new(rx, ry),
                x_rotation: 0.0,
                start,
                sweep: FRAC_PI_2,
            };
            arc.to_cubics(|c| subpath.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
            // The last corner returns to the start, which closing supplies.
            if index < 3 {
                subpath.elements.push(PathEl::Line(onward));
            }
        }
        let start = subpath.start;
        snap_last_to(&mut subpath, start);
    }
    subpath.closed = true;
    Path {
        subpaths: alloc::vec![subpath],
    }
}

/// The ellipse's outline, or the arc's, clockwise as drawn.
fn ellipse_path(ellipse: &EllipseShape) -> Path {
    let full = EllipseArc {
        centre: ellipse.centre,
        radii: ellipse.radii,
        x_rotation: 0.0,
        start: 0.0,
        sweep: TAU,
    };
    let Some(part) = ellipse.arc else {
        let mut subpath = Subpath::new(full.start_point());
        full.to_cubics(|c| subpath.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
        let start = subpath.start;
        snap_last_to(&mut subpath, start);
        subpath.closed = true;
        return Path {
            subpaths: alloc::vec![subpath],
        };
    };
    let start = part.start.to_radians();
    let mut sweep = (part.end - part.start).to_radians() % TAU;
    if sweep <= 0.0 {
        sweep += TAU;
    }
    let arc = EllipseArc {
        start,
        sweep,
        ..full
    };
    let mut subpath = Subpath::new(arc.start_point());
    arc.to_cubics(|c| subpath.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
    match part.kind {
        ArcKind::Open => {}
        ArcKind::Pie => {
            subpath.elements.push(PathEl::Line(ellipse.centre));
            subpath.closed = true;
        }
        ArcKind::Chord => subpath.closed = true,
    }
    Path {
        subpaths: alloc::vec![subpath],
    }
}

/// The star's outline, or the polygon's, clockwise as drawn from its first
/// point, each corner rounded as far as the star says.
fn star_path(star: &StarShape) -> Result<Path, DrawingError> {
    let vertices = if star.polygon {
        star.points
    } else {
        star.points.saturating_mul(2)
    };
    let mut corners: Vec<Point> = Vec::new();
    if !fallible::reserve(&mut corners, vertices as usize) {
        return Err(DrawingError::OutOfMemory);
    }
    let step = TAU / f64::from(vertices);
    let first = (star.rotation - 90.0).to_radians();
    for index in 0..vertices {
        let radius = if !star.polygon && index % 2 == 1 {
            star.outer * star.inner_ratio
        } else {
            star.outer
        };
        let angle = first + step * f64::from(index);
        corners.push(star.centre + Point::from_angle(angle) * radius);
    }
    let count = corners.len();
    let mut contour = Contour {
        nodes: Vec::new(),
        closed: true,
    };
    if !fallible::reserve(&mut contour.nodes, count) {
        return Err(DrawingError::OutOfMemory);
    }
    for index in 0..count {
        let here = corners[index];
        let before = corners[(index + count - 1) % count];
        let after = corners[(index + 1) % count];
        let node = if star.rounding > 0.0 {
            let tangent = (after - before).normalised().unwrap_or(Point::ZERO);
            let back = here.distance(before) * star.rounding / 2.0;
            let ahead = here.distance(after) * star.rounding / 2.0;
            PathNode {
                point: here,
                before: Some(here - tangent * back),
                after: Some(here + tangent * ahead),
                join: NodeJoin::Smooth,
            }
        } else {
            PathNode::corner(here)
        };
        contour.nodes.push(node);
    }
    PathShape {
        contours: alloc::vec![contour],
    }
    .to_path()
}

/// Land a sub-path's last step exactly on `point`, which a run of arcs
/// reaches only to rounding.
fn snap_last_to(subpath: &mut Subpath, point: Point) {
    if let Some(last) = subpath.elements.last_mut() {
        *last = match *last {
            PathEl::Line(_) => PathEl::Line(point),
            PathEl::Cubic(c1, c2, _) => PathEl::Cubic(c1, c2, point),
        };
    }
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;

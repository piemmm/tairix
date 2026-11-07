//! The tools, and the shapes a drag with one makes.
//!
//! Each creating tool turns the two ends of a drag — where it began and
//! where the pointer is — into a shape: Shift constrains it (a square, a
//! circle, a line at a multiple of 15°), Alt draws it out from the point the
//! drag began rather than from a corner.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::window_ipc::AppMenuSymbol;
use tairix_bezier::fit::{fit, Joint};
use tairix_bezier::{Path, PathEl, Point, Subpath};
use tairix_drawing::shape::{EllipseShape, RectShape, StarShape};
use tairix_drawing::{Contour, NodeJoin, PathNode, PathShape, Shape};
use tairix_util::mathf::{atan2, fabs, fmax, fmin, round};

/// A tool the canvas is worked with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Pick, move, scale and turn objects.
    Select,
    /// Move a path's nodes and handles.
    Nodes,
    /// Draw rectangles.
    Rectangle,
    /// Draw ellipses.
    Ellipse,
    /// Draw stars and polygons.
    Star,
    /// Draw straight lines.
    Line,
    /// Draw paths node by node.
    Pen,
    /// Draw freehand.
    Pencil,
    /// Set and edit text.
    Text,
    /// Take another object's style.
    Eyedropper,
    /// Zoom in or out.
    Zoom,
    /// Scroll the canvas.
    Hand,
}

impl Tool {
    /// Every tool, in the tool box's order.
    pub const ALL: [Self; 12] = [
        Self::Select,
        Self::Nodes,
        Self::Rectangle,
        Self::Ellipse,
        Self::Star,
        Self::Line,
        Self::Pen,
        Self::Pencil,
        Self::Text,
        Self::Eyedropper,
        Self::Zoom,
        Self::Hand,
    ];

    /// The symbol its button and menu row show.
    #[must_use]
    pub const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Select => AppMenuSymbol::ToolSelect,
            Self::Nodes => AppMenuSymbol::ToolNodes,
            Self::Rectangle => AppMenuSymbol::ToolRectangle,
            Self::Ellipse => AppMenuSymbol::ToolEllipse,
            Self::Star => AppMenuSymbol::ToolPolygon,
            Self::Line => AppMenuSymbol::ToolLine,
            Self::Pen => AppMenuSymbol::ToolPen,
            Self::Pencil => AppMenuSymbol::ToolPencil,
            Self::Text => AppMenuSymbol::ToolText,
            Self::Eyedropper => AppMenuSymbol::ToolEyedropper,
            Self::Zoom => AppMenuSymbol::ToolZoom,
            Self::Hand => AppMenuSymbol::ToolHand,
        }
    }

    /// What it is called.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Nodes => "Nodes",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Star => "Star and polygon",
            Self::Line => "Line",
            Self::Pen => "Pen",
            Self::Pencil => "Pencil",
            Self::Text => "Text",
            Self::Eyedropper => "Eyedropper",
            Self::Zoom => "Zoom",
            Self::Hand => "Hand",
        }
    }

    /// The letter that, pressed alone, chooses it.
    #[must_use]
    pub const fn key(self) -> char {
        match self {
            Self::Select => 'v',
            Self::Nodes => 'a',
            Self::Rectangle => 'r',
            Self::Ellipse => 'e',
            Self::Star => 's',
            Self::Line => 'l',
            Self::Pen => 'b',
            Self::Pencil => 'p',
            Self::Text => 't',
            Self::Eyedropper => 'i',
            Self::Zoom => 'z',
            Self::Hand => 'h',
        }
    }

    /// The tool a letter pressed alone chooses.
    #[must_use]
    pub fn for_key(key: char) -> Option<Self> {
        let key = key.to_ascii_lowercase();
        Self::ALL.into_iter().find(|tool| tool.key() == key)
    }

    /// What its button's tip says: its name and its key.
    #[must_use]
    pub fn tip(self) -> alloc::string::String {
        alloc::format!("{} ({})", self.name(), self.key().to_ascii_uppercase())
    }

    /// Where it sits in the tool box.
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&tool| tool == self).unwrap_or(0)
    }
}

/// How a drag is constrained.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Constraint {
    /// Equal sides, or a line at a multiple of 15°.
    pub even: bool,
    /// From the point the drag began outwards.
    pub centred: bool,
}

/// The corners a drag from `from` to `to` spans, as constrained.
fn span(from: Point, to: Point, constraint: Constraint) -> (Point, Point) {
    let mut reach = to - from;
    if constraint.even {
        let side = fmax(fabs(reach.x), fabs(reach.y));
        reach = Point::new(side.copysign(reach.x), side.copysign(reach.y));
    }
    if constraint.centred {
        (from - reach, from + reach)
    } else {
        (from, from + reach)
    }
}

/// The rectangle a drag draws, corners rounded by `radius`.
#[must_use]
pub fn rectangle(from: Point, to: Point, constraint: Constraint, radius: f64) -> Shape {
    let (a, b) = span(from, to, constraint);
    let width = fabs(b.x - a.x);
    let height = fabs(b.y - a.y);
    let radius = fmin(radius, fmin(width, height) / 2.0);
    Shape::Rect(RectShape {
        x: fmin(a.x, b.x),
        y: fmin(a.y, b.y),
        width,
        height,
        rx: radius,
        ry: radius,
    })
}

/// The ellipse a drag draws.
#[must_use]
pub fn ellipse(from: Point, to: Point, constraint: Constraint) -> Shape {
    let (a, b) = span(from, to, constraint);
    Shape::Ellipse(EllipseShape {
        centre: a.midpoint(b),
        radii: Point::new(fabs(b.x - a.x) / 2.0, fabs(b.y - a.y) / 2.0),
        arc: None,
    })
}

/// The star or polygon a drag from its centre draws: `points` points, the
/// inner radius `inner` of the outer, turned to the pointer — to the nearest
/// 15° when constrained.
#[must_use]
pub fn star(
    centre: Point,
    to: Point,
    points: u32,
    inner: f64,
    polygon: bool,
    constraint: Constraint,
) -> Shape {
    let reach = to - centre;
    let mut turn = atan2(reach.y, reach.x).to_degrees() + 90.0;
    if constraint.even {
        turn = round(turn / 15.0) * 15.0;
    }
    Shape::Star(StarShape {
        centre,
        points,
        outer: reach.length(),
        inner_ratio: inner,
        rotation: turn,
        rounding: 0.0,
        polygon,
    })
}

/// Where a line drawn from `from` ends, constrained to a multiple of 15°.
#[must_use]
pub fn line_end(from: Point, to: Point, constraint: Constraint) -> Point {
    if !constraint.even {
        return to;
    }
    let reach = to - from;
    let angle = round(atan2(reach.y, reach.x).to_degrees() / 15.0) * 15.0;
    from + Point::from_angle(angle.to_radians()) * reach.length()
}

/// A straight line from `from` to `to`, as a path.
///
/// # Errors
///
/// As the path refuses its nodes.
pub fn line(from: Point, to: Point) -> Result<Shape, tairix_drawing::DrawingError> {
    let mut open = Subpath::new(from);
    open.elements.push(PathEl::Line(to));
    Ok(Shape::Path(Arc::new(PathShape::from_path(&Path {
        subpaths: alloc::vec![open],
    })?)))
}

/// The path a freehand stroke through `points` comes to: curves fitted
/// within `tolerance`, corners sharper than 60° kept as corners.
///
/// # Errors
///
/// As the fit or the path refuses.
pub fn freehand(
    points: &[Point],
    tolerance: f64,
) -> Result<Option<Shape>, tairix_drawing::DrawingError> {
    let Some(&start) = points.first() else {
        return Ok(None);
    };
    let mut subpath = Subpath::new(start);
    let mut failed = false;
    let fitted = fit(points, tolerance, 60.0, |curve, _: Joint| {
        failed |= subpath.elements.try_reserve(1).is_err();
        if !failed {
            subpath
                .elements
                .push(PathEl::Cubic(curve.p1, curve.p2, curve.p3));
        }
    });
    if failed || fitted.is_err() {
        return Err(tairix_drawing::DrawingError::TooComplex);
    }
    if subpath.elements.is_empty() {
        return Ok(None);
    }
    Ok(Some(Shape::Path(Arc::new(PathShape::from_path(&Path {
        subpaths: alloc::vec![subpath],
    })?))))
}

/// The path the pen has set so far: its nodes, closed or not.
#[must_use]
pub fn pen_path(nodes: &[PathNode], closed: bool) -> Option<Shape> {
    if nodes.len() < 2 {
        return None;
    }
    let mut held = Vec::new();
    held.try_reserve(nodes.len()).ok()?;
    held.extend_from_slice(nodes);
    let shape = PathShape {
        contours: alloc::vec![Contour {
            nodes: held,
            closed
        }],
    };
    shape.check().is_ok().then(|| Shape::Path(Arc::new(shape)))
}

/// A pen node at `point` dragged out to `handle`: smooth with handles either
/// side when dragged, a corner when only clicked.
#[must_use]
pub fn pen_node(point: Point, handle: Option<Point>) -> PathNode {
    match handle.filter(|handle| handle.distance(point) > 0.0) {
        Some(after) => PathNode {
            point,
            before: Some(point * 2.0 - after),
            after: Some(after),
            join: NodeJoin::Symmetric,
        },
        None => PathNode::corner(point),
    }
}

#[cfg(test)]
mod tests {
    use tairix_bezier::Point;
    use tairix_drawing::Shape;

    use super::{
        ellipse, freehand, line_end, pen_node, pen_path, rectangle, star, Constraint, Tool,
    };

    #[test]
    fn a_drag_spans_its_shape_and_its_constraints_hold() {
        let plain = Constraint::default();
        let Shape::Rect(rect) =
            rectangle(Point::new(10.0, 10.0), Point::new(4.0, 30.0), plain, 0.0)
        else {
            panic!("a rectangle");
        };
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (4.0, 10.0, 6.0, 20.0)
        );
        let even = Constraint {
            even: true,
            centred: true,
        };
        let Shape::Ellipse(circle) = ellipse(Point::new(0.0, 0.0), Point::new(3.0, 5.0), even)
        else {
            panic!("an ellipse");
        };
        assert_eq!(circle.centre, Point::ZERO);
        assert_eq!(circle.radii, Point::new(5.0, 5.0));
        let end = line_end(
            Point::ZERO,
            Point::new(10.0, 1.0),
            Constraint {
                even: true,
                centred: false,
            },
        );
        assert!(fabs_lt(end.y, 1e-9), "snapped flat: {end:?}");
        let Shape::Star(shape) = star(Point::ZERO, Point::new(0.0, -10.0), 5, 0.5, false, plain)
        else {
            panic!("a star");
        };
        assert!((shape.outer - 10.0).abs() < 1e-9 && shape.rotation.abs() < 1e-9);
    }

    fn fabs_lt(value: f64, bound: f64) -> bool {
        value.abs() < bound
    }

    #[test]
    fn freehand_and_pen_paths_come_out_whole() {
        let points: alloc::vec::Vec<Point> = (0..50)
            .map(|i| Point::new(f64::from(i), f64::from(i % 7)))
            .collect();
        assert!(matches!(freehand(&points, 1.0), Ok(Some(Shape::Path(_)))));
        assert!(matches!(freehand(&[], 1.0), Ok(None)));
        let nodes = [
            pen_node(Point::ZERO, None),
            pen_node(Point::new(10.0, 0.0), Some(Point::new(15.0, 5.0))),
        ];
        assert!(
            pen_path(&nodes[..1], false).is_none(),
            "one node is no path"
        );
        let Some(Shape::Path(path)) = pen_path(&nodes, false) else {
            panic!("a path");
        };
        let smooth = &path.contours[0].nodes[1];
        assert_eq!(
            smooth.before,
            Some(Point::new(5.0, -5.0)),
            "the handle mirrored"
        );
        assert_eq!(Tool::for_key('R'), Some(Tool::Rectangle));
        assert_eq!(Tool::ALL[Tool::Text.index()], Tool::Text);
    }
}

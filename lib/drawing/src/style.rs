//! How an object is painted: its fill, its stroke, and their paints.

use alloc::sync::Arc;
use alloc::vec::Vec;

pub use tairix_bezier::offset::{Cap, Join};
use tairix_bezier::{Affine, Bounds, FillRule, PathEl, Point, Subpath};
use tairix_colour::Rgba;
use tairix_util::mathf::fmax;

use crate::limits::{self, MAX_DASH_ENTRIES, MAX_STOPS};
use crate::DrawingError;

/// What a fill or a stroke is painted with.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Paint {
    /// Nothing: the fill or stroke is not drawn.
    #[default]
    None,
    /// One colour, its alpha the paint's opacity.
    Colour(Rgba),
    /// A gradient.
    Gradient(Arc<Gradient>),
}

impl Paint {
    /// Whether it paints anything at all.
    #[must_use]
    pub fn paints(&self) -> bool {
        match self {
            Self::None => false,
            Self::Colour(colour) => colour.a > 0,
            Self::Gradient(gradient) => gradient.stops.iter().any(|stop| stop.colour.a > 0),
        }
    }

    fn check(&self) -> Result<(), DrawingError> {
        match self {
            Self::None | Self::Colour(_) => Ok(()),
            Self::Gradient(gradient) => gradient.check(),
        }
    }
}

/// A gradient's colour stop.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stop {
    /// Where along the gradient it stands, `0..=1`.
    pub offset: f64,
    /// Its colour, straight alpha.
    pub colour: Rgba,
}

/// How a gradient continues past its ends.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Spread {
    /// The end colours carry on.
    #[default]
    Pad,
    /// The gradient runs back and forth.
    Reflect,
    /// The gradient starts again.
    Repeat,
}

/// A gradient's geometry, in its own space.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GradientKind {
    /// Colour varies along the line from `start` to `end`.
    Linear {
        /// Where offset zero lies.
        start: Point,
        /// Where offset one lies.
        end: Point,
    },
    /// Colour varies outwards from `focus` to the circle about `centre`.
    Radial {
        /// The centre of the circle offset one lies on.
        centre: Point,
        /// That circle's radius.
        radius: f64,
        /// Where offset zero lies.
        focus: Point,
    },
}

/// A colour gradient, in the space of the object it paints.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// Its geometry, in gradient space.
    pub kind: GradientKind,
    /// Its stops, offsets ascending.
    pub stops: Vec<Stop>,
    /// How it continues past its ends.
    pub spread: Spread,
    /// Gradient space into the object's space.
    pub transform: Affine,
}

impl Gradient {
    /// Whether every value is one a drawing may hold: geometry within the
    /// coordinate bound, a radius not negative, one stop at least and at
    /// most [`MAX_STOPS`], their offsets in `0..=1` and not decreasing.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] or [`DrawingError::TooComplex`] for the
    /// first value that is not.
    pub fn check(&self) -> Result<(), DrawingError> {
        match self.kind {
            GradientKind::Linear { start, end } => {
                if !limits::point(start) || !limits::point(end) {
                    return Err(DrawingError::Invalid);
                }
            }
            GradientKind::Radial {
                centre,
                radius,
                focus,
            } => {
                if !limits::point(centre) || !limits::point(focus) || !limits::length(radius) {
                    return Err(DrawingError::Invalid);
                }
            }
        }
        if self.stops.is_empty() {
            return Err(DrawingError::Invalid);
        }
        if self.stops.len() > MAX_STOPS {
            return Err(DrawingError::TooComplex);
        }
        let mut previous = 0.0;
        for stop in &self.stops {
            if !(0.0..=1.0).contains(&stop.offset) || stop.offset < previous {
                return Err(DrawingError::Invalid);
            }
            previous = stop.offset;
        }
        if limits::transform(self.transform) {
            Ok(())
        } else {
            Err(DrawingError::Invalid)
        }
    }
}

/// The shape of an arrowhead.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ArrowShape {
    /// No arrowhead.
    #[default]
    None,
    /// A solid triangle past the end, its base across it.
    Triangle,
    /// A barbed arrow past the end, notched at its base.
    Arrow,
    /// A disc about the end.
    Circle,
    /// A square about the end.
    Square,
    /// A diamond about the end.
    Diamond,
    /// A bar across the end.
    Bar,
}

/// An arrowhead at one end of an open stroke, sized in stroke widths.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Arrowhead {
    /// Its shape.
    pub shape: ArrowShape,
    /// Its breadth across the stroke, in stroke widths.
    pub width: f64,
    /// Its reach along the stroke, in stroke widths.
    pub length: f64,
}

impl Arrowhead {
    /// No arrowhead.
    pub const NONE: Self = Self {
        shape: ArrowShape::None,
        width: 3.0,
        length: 3.0,
    };

    /// Whether it draws anything.
    #[must_use]
    pub fn draws(&self) -> bool {
        self.shape != ArrowShape::None && self.width > 0.0 && self.length > 0.0
    }

    fn check(&self) -> Result<(), DrawingError> {
        let sized = |value: f64| value.is_finite() && (0.0..=100.0).contains(&value);
        if sized(self.width) && sized(self.length) {
            Ok(())
        } else {
            Err(DrawingError::Invalid)
        }
    }

    /// The outline it fills at `at`, the path arriving there running
    /// `direction`, for a stroke `stroke_width` wide; `None` when it draws
    /// nothing or the direction is unknown.
    #[must_use]
    pub fn outline(&self, at: Point, direction: Point, stroke_width: f64) -> Option<Subpath> {
        if !self.draws() {
            return None;
        }
        let along = direction.normalised()?;
        let across = along.perpendicular();
        let half = self.width * stroke_width / 2.0;
        let reach = self.length * stroke_width;
        let place = |forward: f64, side: f64| at + along * forward + across * side;
        let corners: Vec<Point> = match self.shape {
            ArrowShape::None => return None,
            ArrowShape::Triangle => {
                alloc::vec![place(0.0, -half), place(reach, 0.0), place(0.0, half)]
            }
            ArrowShape::Arrow => alloc::vec![
                place(-reach * 0.25, -half),
                place(reach, 0.0),
                place(-reach * 0.25, half),
                place(0.0, 0.0),
            ],
            ArrowShape::Square => alloc::vec![
                place(-half, -half),
                place(half, -half),
                place(half, half),
                place(-half, half),
            ],
            ArrowShape::Diamond => alloc::vec![
                place(-reach / 2.0, 0.0),
                place(0.0, -half),
                place(reach / 2.0, 0.0),
                place(0.0, half),
            ],
            ArrowShape::Bar => {
                let thick = stroke_width / 2.0;
                alloc::vec![
                    place(-thick, -half),
                    place(thick, -half),
                    place(thick, half),
                    place(-thick, half),
                ]
            }
            ArrowShape::Circle => {
                let arc = tairix_bezier::EllipseArc {
                    centre: at,
                    radii: Point::new(half, half),
                    x_rotation: 0.0,
                    start: 0.0,
                    sweep: core::f64::consts::TAU,
                };
                let mut disc = Subpath::new(arc.start_point());
                arc.to_cubics(|c| disc.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
                disc.closed = true;
                return Some(disc);
            }
        };
        let mut outline = Subpath::new(corners[0]);
        outline
            .elements
            .extend(corners[1..].iter().map(|&corner| PathEl::Line(corner)));
        outline.closed = true;
        Some(outline)
    }

    /// How far past the stroke's end and across it the arrowhead can
    /// reach, for a stroke `stroke_width` wide.
    #[must_use]
    pub fn reach(&self, stroke_width: f64) -> f64 {
        if self.draws() {
            fmax(self.width / 2.0, self.length) * stroke_width
        } else {
            0.0
        }
    }
}

/// An object's stroke.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    /// What it is painted with.
    pub paint: Paint,
    /// Its width.
    pub width: f64,
    /// How it turns corners.
    pub join: Join,
    /// The longest miter, in widths, before a miter join is bevelled; at
    /// least one.
    pub miter_limit: f64,
    /// How it ends.
    pub cap: Cap,
    /// Its dash pattern, alternately drawn and skipped; empty is solid.
    pub dashes: Arc<[f64]>,
    /// How far into the pattern the stroke begins.
    pub dash_offset: f64,
    /// The arrowhead at its start.
    pub start: Arrowhead,
    /// The arrowhead at its end.
    pub end: Arrowhead,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            paint: Paint::None,
            width: 1.0,
            join: Join::Miter,
            miter_limit: 4.0,
            cap: Cap::Butt,
            dashes: Arc::from(&[][..]),
            dash_offset: 0.0,
            start: Arrowhead::NONE,
            end: Arrowhead::NONE,
        }
    }
}

impl Stroke {
    /// Whether it paints anything at all.
    #[must_use]
    pub fn paints(&self) -> bool {
        self.width > 0.0 && self.paint.paints()
    }

    /// Whether `head` is drawn: an arrowhead is part of its stroke, so it is
    /// drawn only where the stroke is.
    #[must_use]
    pub fn draws_head(&self, head: &Arrowhead) -> bool {
        head.draws() && self.paints()
    }

    /// The one colour its arrowheads are filled with: SVG gives a marker no
    /// way to follow a gradient along the stroke, so a gradient's first
    /// stop stands for it.
    #[must_use]
    pub fn head_colour(&self) -> Rgba {
        match &self.paint {
            Paint::Colour(colour) => *colour,
            Paint::Gradient(gradient) => gradient
                .stops
                .first()
                .map_or(Rgba::new(0, 0, 0, 255), |stop| stop.colour),
            Paint::None => Rgba::new(0, 0, 0, 0),
        }
    }

    /// How far beyond the geometry the stroke can reach, arrowheads and
    /// miters included: what a conservative bound grows by.
    #[must_use]
    pub fn reach(&self) -> f64 {
        if !self.paints() {
            return 0.0;
        }
        let half = self.width / 2.0;
        let corner = match self.join {
            Join::Miter => half * fmax(self.miter_limit, 1.0),
            Join::Round | Join::Bevel => half,
        };
        // A square cap's corner lies half the width out on both axes.
        let cap = if self.cap == Cap::Square {
            half * core::f64::consts::SQRT_2
        } else {
            half
        };
        fmax(
            fmax(corner, cap),
            fmax(self.start.reach(self.width), self.end.reach(self.width)),
        )
    }

    fn check(&self) -> Result<(), DrawingError> {
        self.paint.check()?;
        let miter = self.miter_limit.is_finite() && (1.0..=1e4).contains(&self.miter_limit);
        let valid = limits::length(self.width) && miter && limits::coordinate(self.dash_offset);
        if !valid {
            return Err(DrawingError::Invalid);
        }
        if self.dashes.len() > MAX_DASH_ENTRIES {
            return Err(DrawingError::TooComplex);
        }
        if !self.dashes.iter().all(|&dash| limits::length(dash)) {
            return Err(DrawingError::Invalid);
        }
        self.start.check()?;
        self.end.check()
    }
}

/// Which of fill and stroke is painted first.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum PaintOrder {
    /// The fill, then the stroke over it.
    #[default]
    FillFirst,
    /// The stroke, then the fill over it.
    StrokeFirst,
}

/// How an object is painted.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// What fills it.
    pub fill: Paint,
    /// Which points its fill covers.
    pub fill_rule: FillRule,
    /// Its stroke.
    pub stroke: Stroke,
    /// Which is painted first.
    pub paint_order: PaintOrder,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: Paint::Colour(Rgba::new(0, 0, 0, 255)),
            fill_rule: FillRule::NonZero,
            stroke: Stroke::default(),
            paint_order: PaintOrder::FillFirst,
        }
    }
}

impl Style {
    /// Whether every value is one a drawing may hold.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] or [`DrawingError::TooComplex`] for the
    /// first value that is not.
    pub fn check(&self) -> Result<(), DrawingError> {
        self.fill.check()?;
        self.stroke.check()
    }

    /// `geometry`, the bounds of an object's outline in its own space, grown
    /// by everything the stroke can reach beyond it. An object that paints
    /// nothing keeps its outline's bounds, so it can still be found and
    /// chosen.
    #[must_use]
    pub fn painted_bounds(&self, geometry: Bounds) -> Bounds {
        geometry.inflated(self.stroke.reach())
    }
}

#[cfg(test)]
#[path = "style_tests.rs"]
mod tests;

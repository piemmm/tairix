//! Fitting a cursor's artwork to the pixel grid of the side it is drawn at.
//!
//! Stretching a design grid across an arbitrary pixel side puts each edge
//! wherever the ratio lands it, so at any ratio but a whole one an upright or
//! level edge falls part-way across a pixel and smears into a grey column.
//! [`Fit`] moves every such edge onto the nearest pixel boundary and carries
//! each other coordinate along between the edges either side of it, so
//! diagonals and curves keep their sub-pixel placement and no coordinate can
//! pass another.
//!
//! Distances are measured out from the hotspot. That makes the hotspot a
//! pixel corner at every side, and keeps artwork that is symmetric about it
//! symmetric, since an edge and its mirror image round the same distance.

use alloc::vec::Vec;

use core::cmp::Ordering;

use tairix_geometry::{saturate_i32, Point};
use tairix_raster::{
    Affine, Gradient, Group, Layer, Mask, Node, Paint, Pattern, MAX_DRAWING_EXTENT, MAX_GROUP_DEPTH,
};
use tairix_util::mathf::round_i32;

use crate::vector::VectorCursor;

/// Fitted units per pixel.
///
/// Fine enough that placing a point to the nearest unit moves an edge by
/// under half an alpha level, and a divisor of the scan converter's own
/// resolution, so a fitted coordinate is never rounded a second time.
pub(crate) const FIT_UNITS: u32 = 256;

/// The most points fitted artwork may hold, the edges' crossings included.
///
/// A fixed containment bound, not a capacity: splitting every edge where it
/// crosses a fitted edge's line costs the edges times the lines they cross,
/// which artwork built to cross them all would make quadratic. A pointer
/// needs a few thousand; artwork past this draws no cursor at all.
const MAX_FITTED_POINTS: usize = 1 << 18;

/// A cursor's design grid mapped onto the pixel grid of one side.
pub(crate) struct Fit {
    x: Axis,
    y: Axis,
    /// Fitted units per design unit: the plain stretch, which is how the
    /// parts of a drawing placed by a linear map rather than by its edges — a
    /// gradient's frame, a pattern's tile — are carried across.
    stretch: f64,
    side: u32,
}

impl Fit {
    /// The fit of `cursor` at `side` pixels, or `None` for a zero side or
    /// design grid, or a side no surface could be drawn at.
    pub(crate) fn new(cursor: &VectorCursor, side: u32) -> Option<Self> {
        let design = cursor.design_size();
        if side == 0 || design == 0 || side > MAX_DRAWING_EXTENT {
            return None;
        }
        let span = Span::new(design, side);
        let mut edges = Edges::default();
        edges.collect(cursor.nodes(), span, 0);
        Some(Self {
            x: Axis::new(cursor.hotspot_x(), edges.upright, span),
            y: Axis::new(cursor.hotspot_y(), edges.level, span),
            stretch: f64::from(side) * f64::from(FIT_UNITS) / f64::from(design),
            side,
        })
    }

    /// The fitted grid's side in fitted units, which is what the fitted
    /// artwork is drawn on.
    pub(crate) const fn design(&self) -> u32 {
        self.side.saturating_mul(FIT_UNITS)
    }

    /// The hotspot's pixel, held inside the image even where the artwork's
    /// own hotspot lies outside it.
    pub(crate) fn hotspot(&self) -> Point {
        let last = i32::try_from(self.side.saturating_sub(1)).unwrap_or(i32::MAX);
        Point::new(
            self.x.anchor_pixel().clamp(0, last),
            self.y.anchor_pixel().clamp(0, last),
        )
    }

    /// The hotspot's pixel corner in fitted units, before any clamp: the
    /// point the artwork is laid out from.
    pub(crate) const fn anchor(&self) -> (i64, i64) {
        (self.x.anchor, self.y.anchor)
    }

    /// `point` on the fitted grid.
    pub(crate) fn point(&self, point: (i32, i32)) -> (i32, i32) {
        (self.x.map(point.0), self.y.map(point.1))
    }

    /// `nodes` with every contour fitted and every paint carried along, or
    /// `None` past [`MAX_FITTED_POINTS`].
    pub(crate) fn nodes(&self, nodes: &[Node]) -> Option<Vec<Node>> {
        let mut budget = MAX_FITTED_POINTS;
        self.within(nodes, &mut budget, 0)
    }

    fn within(&self, nodes: &[Node], budget: &mut usize, depth: usize) -> Option<Vec<Node>> {
        nodes
            .iter()
            .map(|node| self.node(node, budget, depth))
            .collect()
    }

    /// `contour` on the fitted grid, each edge split wherever it crosses a
    /// fitted edge's line.
    ///
    /// The fit is linear only between those lines. An edge mapped by its ends
    /// alone would leave the fit's path, and pieces overlapping by a sliver —
    /// as every stroke's do — would part there.
    fn contour(&self, contour: &[(i32, i32)], budget: &mut usize) -> Option<Vec<(i32, i32)>> {
        let mut fitted = Vec::with_capacity(contour.len());
        let mut crossings = Vec::new();
        let next = contour.iter().cycle().skip(1);
        for (&from, &to) in contour.iter().zip(next) {
            self.crossings(from, to, &mut crossings);
            *budget = budget.checked_sub(1 + crossings.len())?;
            fitted.push(self.point(from));
            fitted.extend(crossings.iter().map(|&(_, point)| point));
        }
        Some(fitted)
    }

    /// Where the open edge from `from` to `to` crosses a line the fit bends
    /// at, each fitted exactly and in order from `from`, left in `crossings`.
    fn crossings(
        &self,
        from: (i32, i32),
        to: (i32, i32),
        crossings: &mut Vec<(Share, (i32, i32))>,
    ) {
        crossings.clear();
        crossings.extend(self.x.crossed(from.0, to.0).iter().map(|&line| {
            let (share, y) = crossing(line, (from.0, to.0), (from.1, to.1));
            (share, (self.x.map(line), self.y.place(y)))
        }));
        crossings.extend(self.y.crossed(from.1, to.1).iter().map(|&line| {
            let (share, x) = crossing(line, (from.1, to.1), (from.0, to.0));
            (share, (self.x.place(x), self.y.map(line)))
        }));
        crossings.sort_unstable_by_key(|&(share, _)| share);
        crossings.dedup_by(|a, b| a.0 == b.0);
    }

    /// `node`, `depth` groups deep, on the fitted grid.
    ///
    /// A group nested past the depth the renderer draws keeps its place but
    /// none of its content, so it is refused just as it would have been.
    fn node(&self, node: &Node, budget: &mut usize, depth: usize) -> Option<Node> {
        Some(match node {
            Node::Fill(layer) => Node::Fill(Layer {
                paint: self.paint(&layer.paint),
                rule: layer.rule,
                contours: layer
                    .contours
                    .iter()
                    .map(|contour| self.contour(contour, budget))
                    .collect::<Option<_>>()?,
            }),
            Node::Group(group) if depth >= MAX_GROUP_DEPTH => Node::Group(Group {
                opacity: group.opacity,
                mask: None,
                children: Vec::new(),
            }),
            Node::Group(group) => Node::Group(Group {
                opacity: group.opacity,
                mask: match &group.mask {
                    Some(mask) => Some(Mask {
                        kind: mask.kind,
                        content: self.within(&mask.content, budget, depth + 1)?,
                    }),
                    None => None,
                },
                children: self.within(&group.children, budget, depth + 1)?,
            }),
        })
    }

    /// `paint` re-expressed for fitted geometry.
    ///
    /// A gradient or a pattern is positioned in the geometry's coordinates,
    /// so it is carried across by the plain stretch: it lands at most the half
    /// pixel an edge moved from where the edges went, which neither a smooth
    /// ramp nor a repeat can show.
    fn paint(&self, paint: &Paint) -> Paint {
        let unfit = Affine {
            a: 1.0 / self.stretch,
            b: 0.0,
            c: 0.0,
            d: 1.0 / self.stretch,
            e: self.x.unfit_offset(self.stretch),
            f: self.y.unfit_offset(self.stretch),
        };
        carried(paint, unfit, self.stretch, 0)
    }
}

/// `paint` for geometry now reached through `unfit` from its old
/// coordinates, with any pattern tile's own artwork restated on the grid
/// `stretch` times finer that the fitted drawing is drawn on.
fn carried(paint: &Paint, unfit: Affine, stretch: f64, depth: usize) -> Paint {
    match paint {
        Paint::Solid(color) => Paint::Solid(*color),
        Paint::Gradient(gradient) => Paint::Gradient(Gradient {
            to_gradient: unfit.then(gradient.to_gradient),
            ..gradient.clone()
        }),
        Paint::Pattern(pattern) => Paint::Pattern(Pattern {
            content: restated(&pattern.content, stretch, depth + 1),
            to_tile: unfit.then(pattern.to_tile),
            fold: pattern.fold,
            opacity: pattern.opacity,
        }),
    }
}

/// A tile's artwork restated on a grid `stretch` times finer.
///
/// A tile is drawn on the drawing's design grid, so fitting the drawing
/// changes the grid its tiles are drawn on too. Only the scale changes, since
/// a tile has no hotspot and no pixel grid of its own. Nesting past the depth
/// the renderer draws is dropped, as the renderer would drop it.
fn restated(nodes: &[Node], stretch: f64, depth: usize) -> Vec<Node> {
    if depth >= MAX_GROUP_DEPTH {
        return Vec::new();
    }
    let scale = |value: i32| round_i32(f64::from(value) * stretch);
    let unscale = Affine::scale(1.0 / stretch, 1.0 / stretch);
    nodes
        .iter()
        .map(|node| match node {
            Node::Fill(layer) => Node::Fill(Layer {
                paint: carried(&layer.paint, unscale, stretch, depth),
                rule: layer.rule,
                contours: layer
                    .contours
                    .iter()
                    .map(|contour| contour.iter().map(|&(x, y)| (scale(x), scale(y))).collect())
                    .collect(),
            }),
            Node::Group(group) => Node::Group(Group {
                opacity: group.opacity,
                mask: group.mask.as_ref().map(|mask| Mask {
                    kind: mask.kind,
                    content: restated(&mask.content, stretch, depth + 1),
                }),
                children: restated(&group.children, stretch, depth + 1),
            }),
        })
        .collect()
}

/// The ratio a fit stretches by: `side` pixels across `design` units.
#[derive(Copy, Clone)]
struct Span {
    design: i128,
    side: i128,
}

impl Span {
    fn new(design: u32, side: u32) -> Self {
        Self {
            design: i128::from(design),
            side: i128::from(side),
        }
    }

    /// `units` design units in whole pixels, to the nearest.
    fn pixels(self, units: i64) -> i64 {
        rounded(i128::from(units) * self.side, self.design)
    }

    /// Whether `units` design units reach at least a pixel.
    fn reach_a_pixel(self, units: i64) -> bool {
        i128::from(units) * self.side >= self.design
    }
}

/// The coordinates of a drawing's upright and level edges.
#[derive(Default)]
struct Edges {
    /// The `x` of every upright edge.
    upright: Vec<i32>,
    /// The `y` of every level edge.
    level: Vec<i32>,
}

impl Edges {
    /// Gather the edges of everything `nodes` draws or clips with.
    ///
    /// A clip's edge is a visible edge of what it clips, so a mask's content
    /// counts; a pattern tile's does not, being drawn in the tile's own space.
    /// Only an edge at least a pixel long is kept: a shorter one is no stem
    /// anybody sees, and a curve flattened onto the grid leaves plenty.
    fn collect(&mut self, nodes: &[Node], span: Span, depth: usize) {
        if depth >= MAX_GROUP_DEPTH {
            return;
        }
        for node in nodes {
            match node {
                Node::Fill(layer) => {
                    for contour in layer.contours.iter().filter(|contour| contour.len() >= 3) {
                        self.contour(contour, span);
                    }
                }
                Node::Group(group) => {
                    self.collect(&group.children, span, depth + 1);
                    if let Some(mask) = &group.mask {
                        self.collect(&mask.content, span, depth + 1);
                    }
                }
            }
        }
    }

    fn contour(&mut self, contour: &[(i32, i32)], span: Span) {
        let next = contour.iter().cycle().skip(1);
        for (&(x0, y0), &(x1, y1)) in contour.iter().zip(next) {
            if x0 == x1 && span.reach_a_pixel((i64::from(y1) - i64::from(y0)).abs()) {
                self.upright.push(x0);
            }
            if y0 == y1 && span.reach_a_pixel((i64::from(x1) - i64::from(x0)).abs()) {
                self.level.push(y0);
            }
        }
    }
}

/// One edge's place on an axis: how far from the hotspot it was authored, in
/// design units, and how far it lands, in fitted units.
#[derive(Copy, Clone, Debug)]
struct Knot {
    design: i64,
    fitted: i64,
}

/// One axis of a [`Fit`].
struct Axis {
    /// The hotspot, in design units.
    origin: i32,
    /// The hotspot's pixel corner, in fitted units.
    anchor: i64,
    /// The edges past the hotspot, ascending by distance from it, led by the
    /// hotspot itself.
    after: Vec<Knot>,
    /// The edges before it, the same way.
    before: Vec<Knot>,
    /// Every coordinate the fit bends at, the hotspot's included, ascending.
    breaks: Vec<i32>,
    span: Span,
}

impl Axis {
    fn new(origin: i32, mut edges: Vec<i32>, span: Span) -> Self {
        edges.push(origin);
        edges.sort_unstable();
        edges.dedup();
        let from_origin = |edge: i32| i64::from(edge) - i64::from(origin);
        let after = knots(
            edges
                .iter()
                .map(|&edge| from_origin(edge))
                .filter(|&distance| distance > 0),
            span,
        );
        let before = knots(
            edges
                .iter()
                .rev()
                .map(|&edge| -from_origin(edge))
                .filter(|&distance| distance > 0),
            span,
        );
        Self {
            origin,
            anchor: units(span.pixels(i64::from(origin))),
            after,
            before,
            breaks: edges,
            span,
        }
    }

    /// `coordinate` on the fitted grid.
    fn map(&self, coordinate: i32) -> i32 {
        self.place(Ratio::whole(coordinate))
    }

    /// `coordinate` on the fitted grid, where it may lie between two design
    /// units.
    ///
    /// Measured out from the hotspot on either side and rounded the same way
    /// on both, so a coordinate and its mirror image about the hotspot land
    /// mirrored.
    fn place(&self, coordinate: Ratio) -> i32 {
        let distance = coordinate.less(self.origin);
        let offset = if distance.numerator >= 0 {
            along(&self.after, distance, self.span)
        } else {
            along(&self.before, distance.negated(), self.span).saturating_neg()
        };
        saturate_i32(self.anchor.saturating_add(offset))
    }

    /// The coordinates the fit bends at strictly between `from` and `to`.
    fn crossed(&self, from: i32, to: i32) -> &[i32] {
        let (low, high) = (from.min(to), from.max(to));
        let first = self.breaks.partition_point(|&line| line <= low);
        let last = self.breaks.partition_point(|&line| line < high);
        self.breaks.get(first..last).unwrap_or_default()
    }

    /// The hotspot's pixel, before any clamp.
    fn anchor_pixel(&self) -> i32 {
        saturate_i32(self.anchor / i64::from(FIT_UNITS))
    }

    /// The translation that undoes the plain stretch on this axis.
    fn unfit_offset(&self, stretch: f64) -> f64 {
        let anchor = f64::from(self.anchor_pixel()) * f64::from(FIT_UNITS);
        f64::from(self.origin) - anchor / stretch
    }
}

/// A design coordinate held exactly: `numerator / denominator`, the
/// denominator positive.
#[derive(Copy, Clone, Debug)]
struct Ratio {
    numerator: i128,
    denominator: i128,
}

impl Ratio {
    fn whole(value: i32) -> Self {
        Self {
            numerator: i128::from(value),
            denominator: 1,
        }
    }

    /// This coordinate less `origin`.
    fn less(self, origin: i32) -> Self {
        Self {
            numerator: self.numerator - i128::from(origin) * self.denominator,
            ..self
        }
    }

    const fn negated(self) -> Self {
        Self {
            numerator: -self.numerator,
            denominator: self.denominator,
        }
    }
}

/// How far along an edge a crossing lies: `reached / run` of the way, the
/// run positive, compared as the fraction it is.
#[derive(Copy, Clone, Debug)]
struct Share {
    reached: i128,
    run: i128,
}

impl Ord for Share {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.reached * other.run).cmp(&(other.reached * self.run))
    }
}

impl PartialOrd for Share {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Share {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Share {}

/// Where the edge running `on` along one axis and `across` along the other
/// crosses `line` on the first: how far along it, and where it is on the
/// second axis there.
fn crossing(line: i32, on: (i32, i32), across: (i32, i32)) -> (Share, Ratio) {
    let run = i128::from(on.1) - i128::from(on.0);
    let rise = i128::from(across.1) - i128::from(across.0);
    let reached = i128::from(line) - i128::from(on.0);
    let sign = if run < 0 { -1 } else { 1 };
    (
        Share {
            reached: reached * sign,
            run: run * sign,
        },
        Ratio {
            numerator: (i128::from(across.0) * run + reached * rise) * sign,
            denominator: run * sign,
        },
    )
}

/// The knots for `distances`, which ascend from the hotspot.
///
/// Each edge moves to the pixel boundary nearest its own distance, so how
/// far it moves never depends on its neighbours. The one exception keeps a
/// stem from vanishing: an edge at least half a pixel past the one before it
/// never lands on that one.
fn knots(distances: impl Iterator<Item = i64>, span: Span) -> Vec<Knot> {
    let mut knots = alloc::vec![Knot {
        design: 0,
        fitted: 0,
    }];
    let (mut last_design, mut last_pixel) = (0_i64, 0_i64);
    for distance in distances {
        let nearest = span.pixels(distance);
        let pixel = if nearest > last_pixel {
            nearest
        } else if span.reach_a_pixel(distance.saturating_sub(last_design).saturating_mul(2)) {
            last_pixel.saturating_add(1)
        } else {
            last_pixel
        };
        knots.push(Knot {
            design: distance,
            fitted: units(pixel),
        });
        (last_design, last_pixel) = (distance, pixel);
    }
    knots
}

/// How far a point `distance` design units from the hotspot lands, along
/// `knots`: between two edges, in proportion to its place between them, and
/// past the outermost, at the plain stretch.
fn along(knots: &[Knot], distance: Ratio, span: Span) -> i64 {
    let Ratio {
        numerator,
        denominator,
    } = distance;
    let at = knots
        .partition_point(|knot| i128::from(knot.design) * denominator <= numerator)
        .saturating_sub(1);
    let Some(from) = knots.get(at) else {
        return 0;
    };
    let past = numerator.saturating_sub(i128::from(from.design).saturating_mul(denominator));
    let step = match knots.get(at + 1) {
        Some(to) => rounded(
            past.saturating_mul(i128::from(to.fitted) - i128::from(from.fitted)),
            denominator.saturating_mul(i128::from(to.design) - i128::from(from.design)),
        ),
        None => rounded(
            past.saturating_mul(span.side * i128::from(FIT_UNITS)),
            denominator.saturating_mul(span.design),
        ),
    };
    from.fitted.saturating_add(step)
}

/// `pixels` in fitted units.
fn units(pixels: i64) -> i64 {
    pixels.saturating_mul(i64::from(FIT_UNITS))
}

/// `numerator / denominator` to the nearest whole number, a half rounding up;
/// `denominator` is positive.
fn rounded(numerator: i128, denominator: i128) -> i64 {
    let quotient = numerator
        .saturating_mul(2)
        .saturating_add(denominator)
        .div_euclid(denominator.saturating_mul(2));
    i64::try_from(quotient).unwrap_or(if quotient < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
#[path = "fit_tests.rs"]
mod tests;

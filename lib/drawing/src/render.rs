//! Rendering a drawing through `lib/raster`'s one renderer.
//!
//! A square of device pixels — a tile of the window, or of an export — is
//! turned into raster artwork on the design grid. Each node is flattened and
//! stroked in its own space, so a stroke scales and shears with its object as
//! SVG draws it, and only what can reach the square is worked on: a sub-path
//! whose bounds miss it is dropped, a curve whose control hull misses it is
//! its chord (which leaves the winding inside unchanged), a stroke is cut
//! into the runs that can reach it, and what remains is clipped to the
//! square before it is placed, so no zoom can push a vertex off the grid.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::flatten::flatten_cubic;
use tairix_bezier::{Affine, Bounds, FillRule, Path, Point, Segment, Subpath};
use tairix_colour::Rgba;
use tairix_raster::{
    Color, GradientStop, Group, Layer, Mask, MaskKind, Node as Art, Paint as Painted, SpreadMethod,
};
use tairix_svg::geom::{LineCap, LineJoin, StrokeStyle, SubPath};
use tairix_svg::paint::{placed_gradient, GradientGeometry};
use tairix_svg::stroke::stroke_outline;
use tairix_util::fallible;
use tairix_util::mathf::round_i32;

use crate::drawing::Drawing;
use crate::node::{Kind, Node};
use crate::style::{Cap, GradientKind, Join, Paint, PaintOrder, Spread, Stroke};
use crate::text::Glyphs;
use crate::DrawingError;

/// How far a flattened curve may depart from the true one, in device pixels.
const DEVICE_TOLERANCE: f64 = 0.2;

/// How far past the square geometry is kept, in device pixels, so the
/// anti-aliased edge of what crosses its border is drawn whole.
const MARGIN: f64 = 2.0;

/// The most points one node may come to in one square: a containment bound
/// on what a dash pattern or a path of many nodes asks of a render.
const MAX_POINTS: usize = 1 << 21;

/// How objects are drawn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// As they are painted.
    Painted,
    /// Every outline as a hairline of one device pixel in this colour,
    /// paints, opacities and clips set aside.
    Outline(Rgba),
}

/// A square of device pixels to draw.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct View {
    /// The map from the drawing onto device pixels.
    pub to_device: Affine,
    /// The square's top-left corner, in device pixels.
    pub origin: Point,
    /// Its side, in device pixels.
    pub side: f64,
    /// How many design-grid units span its side: the precision vertices are
    /// placed at.
    pub design: u32,
    /// How objects are drawn.
    pub mode: Mode,
}

/// What a square of a drawing renders as.
#[derive(Clone, Debug, Default)]
pub struct Artwork {
    /// The artwork, bottom first, on a design grid spanning the square.
    pub nodes: Vec<Art>,
    /// Whether every glyph its text needs was known.
    pub complete: bool,
}

/// What rendering derives from a node's own definition alone — the path a
/// shape traces and the dashes its stroke cuts it into — which a caller may
/// keep between renders, since no zoom or square changes it. The provided
/// methods derive afresh.
pub trait Derived {
    /// What `node`, a shape, traces in its own space.
    ///
    /// # Errors
    ///
    /// As [`traced`].
    fn path(&self, node: &Arc<Node>) -> Result<Arc<Path>, DrawingError> {
        Ok(Arc::new(traced(node)?))
    }

    /// The dashes `node`'s stroke cuts `path`, what it traces, into.
    ///
    /// # Errors
    ///
    /// As [`dashes`].
    fn dashes(
        &self,
        node: &Arc<Node>,
        path: &Path,
        stroke: &Stroke,
    ) -> Result<Arc<Path>, DrawingError> {
        let _ = node;
        Ok(Arc::new(dashes(path, stroke)?))
    }
}

/// Derives everything afresh and keeps nothing.
#[derive(Copy, Clone, Debug, Default)]
pub struct Afresh;

impl Derived for Afresh {}

/// What `node` traces in its own space: a shape's outline, nothing for text or
/// a group.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the path cannot be held.
pub fn traced(node: &Node) -> Result<Path, DrawingError> {
    match node.kind() {
        Kind::Shape { shape, .. } => shape.to_path(),
        Kind::Text { .. } | Kind::Group { .. } => Ok(Path::new()),
    }
}

/// `path` cut into the dashes of `stroke`, each an open sub-path, measured
/// along the curves themselves so every square and zoom cuts them alike; the
/// path itself for a solid stroke.
///
/// # Errors
///
/// [`DrawingError::TooComplex`] for more dashes than a path may be cut into,
/// and [`DrawingError::OutOfMemory`] when they cannot be held.
pub fn dashes(path: &Path, stroke: &Stroke) -> Result<Path, DrawingError> {
    if stroke.dashes.is_empty() {
        return Ok(path.clone());
    }
    tairix_bezier::offset::dashed(path, &stroke.dashes, stroke.dash_offset).map_err(|error| {
        match error {
            tairix_bezier::boolean::BooleanError::TooComplex => DrawingError::TooComplex,
            tairix_bezier::boolean::BooleanError::OutOfMemory => DrawingError::OutOfMemory,
        }
    })
}

/// `view`'s square of `drawing`, its text set from `glyphs` and what nodes
/// derive taken from `derived`.
///
/// # Errors
///
/// [`DrawingError::TooComplex`] for an object that comes to more than a
/// render may hold, and [`DrawingError::OutOfMemory`] when the artwork cannot
/// be held.
pub fn artwork(
    drawing: &Drawing,
    view: &View,
    glyphs: &dyn Glyphs,
    derived: &dyn Derived,
) -> Result<Artwork, DrawingError> {
    let window = Bounds::new(
        Point::new(view.origin.x - MARGIN, view.origin.y - MARGIN),
        Point::new(
            view.origin.x + view.side + MARGIN,
            view.origin.y + view.side + MARGIN,
        ),
    );
    let mut renderer = Renderer {
        view,
        glyphs,
        derived,
        window,
        to_design: Affine::translate(-view.origin.x, -view.origin.y).then(Affine::scale(
            f64::from(view.design) / view.side,
            f64::from(view.design) / view.side,
        )),
        complete: true,
        scratch: Vec::new(),
    };
    let mut nodes = Vec::new();
    if let (Mode::Painted, Some(background)) = (view.mode, drawing.page().background) {
        let page = drawing.page().bounds();
        let ring = [
            page.min,
            Point::new(page.max.x, page.min.y),
            page.max,
            Point::new(page.min.x, page.max.y),
        ];
        let contour = renderer.placed(ring.iter().copied(), view.to_device)?;
        if let Some(contour) = contour {
            let layer = Layer::filled(
                Painted::Solid(Color::from(background)),
                FillRule::NonZero,
                alloc::vec![contour],
            );
            push(&mut nodes, Art::Fill(layer))?;
        }
    }
    if let Some(area) = renderer.area_in(view.to_device) {
        for (_, layer) in drawing.layers().meeting(area) {
            renderer.node(layer, view.to_device, &mut nodes)?;
        }
    }
    Ok(Artwork {
        nodes,
        complete: renderer.complete,
    })
}

struct Renderer<'v> {
    view: &'v View,
    glyphs: &'v dyn Glyphs,
    derived: &'v dyn Derived,
    /// The square and its margin, in device pixels.
    window: Bounds,
    /// Device pixels onto the design grid.
    to_design: Affine,
    complete: bool,
    /// The clipper's second buffer, kept between contours.
    scratch: Vec<Point>,
}

impl Renderer<'_> {
    /// Where the window lies in a space `to_device` maps onto the device, or
    /// `None` when the map collapses that space to nothing.
    fn area_in(&self, to_device: Affine) -> Option<Bounds> {
        let back = to_device.invert()?;
        let area = self.window.transformed(back);
        area.is_finite().then_some(area)
    }

    fn node(
        &mut self,
        node: &Arc<Node>,
        parent: Affine,
        out: &mut Vec<Art>,
    ) -> Result<(), DrawingError> {
        if !node.visible() || !node.bounds().transformed(parent).intersects(self.window) {
            return Ok(());
        }
        let to_device = node.transform().then(parent);
        let Some(area) = self.area_in(to_device) else {
            return Ok(());
        };
        let painted = matches!(self.view.mode, Mode::Painted);
        let opacity = if painted { node.opacity() } else { 1.0 };
        match node.kind() {
            Kind::Group { children, clip } => {
                let mut inner = Vec::new();
                for (_, child) in children.meeting(area) {
                    self.node(child, to_device, &mut inner)?;
                }
                let mask = match clip {
                    Some(clip) if painted => {
                        let mut content = Vec::new();
                        self.clip(clip, to_device, &mut content)?;
                        Some(Mask {
                            kind: MaskKind::Alpha,
                            content,
                        })
                    }
                    _ => None,
                };
                isolated(opacity, mask, inner, out)
            }
            Kind::Shape { .. } => {
                let path = self.derived.path(node)?;
                self.shape(node, &path, to_device, area, opacity, out)
            }
            Kind::Text { text, style } => {
                let setting = text.set(self.glyphs)?;
                self.complete &= setting.complete;
                let mut path = Path::new();
                for placed in &setting.glyphs {
                    let reach = style.stroke.reach();
                    if placed
                        .glyph
                        .ink()
                        .transformed(placed.at)
                        .inflated(reach)
                        .intersects(area)
                    {
                        let set = placed.glyph.outline().transformed(placed.at);
                        if path.subpaths.try_reserve(set.subpaths.len()).is_err() {
                            return Err(DrawingError::OutOfMemory);
                        }
                        path.subpaths.extend(set.subpaths);
                    }
                }
                self.shape(node, &path, to_device, area, opacity, out)
            }
        }
    }

    /// The layers one painted path comes to: its fill and its stroke in
    /// their order, then its arrowheads, through its opacity.
    fn shape(
        &mut self,
        node: &Arc<Node>,
        path: &Path,
        to_device: Affine,
        area: Bounds,
        opacity: f64,
        out: &mut Vec<Art>,
    ) -> Result<(), DrawingError> {
        let Some(style) = node.style() else {
            return Ok(());
        };
        let tolerance = DEVICE_TOLERANCE / to_device.max_scale();
        if let Mode::Outline(colour) = self.view.mode {
            let hairline = StrokeStyle {
                width: 1.0,
                ..StrokeStyle::default()
            };
            let mut polylines = flattened_runs(path, area, tolerance)?;
            for polyline in &mut polylines {
                for point in &mut polyline.points {
                    *point = to_device.apply(*point);
                }
            }
            let outline = stroked(&polylines, &hairline, DEVICE_TOLERANCE)?;
            let layer = self.layer(
                &outline,
                &Paint::Colour(colour),
                FillRule::NonZero,
                Affine::IDENTITY,
                1.0,
            )?;
            return push_layer(out, layer);
        }
        let mut layers: Vec<Layer> = Vec::new();
        let fill = |renderer: &mut Self, layers: &mut Vec<Layer>| -> Result<(), DrawingError> {
            if !style.fill.paints() {
                return Ok(());
            }
            let contours = flattened_fill(path, area, tolerance)?;
            let layer = renderer.layer(&contours, &style.fill, style.fill_rule, to_device, 1.0)?;
            push_optional(layers, layer)
        };
        let stroke = |renderer: &mut Self, layers: &mut Vec<Layer>| -> Result<(), DrawingError> {
            if !style.stroke.paints() {
                return Ok(());
            }
            let reached = area.inflated(style.stroke.reach());
            let dashed = if style.stroke.dashes.is_empty() {
                None
            } else if matches!(node.kind(), Kind::Shape { .. }) {
                Some(renderer.derived.dashes(node, path, &style.stroke)?)
            } else {
                Some(Arc::new(dashes(path, &style.stroke)?))
            };
            let runs = flattened_runs(dashed.as_deref().unwrap_or(path), reached, tolerance)?;
            let outline = stroked(&runs, &stroke_style(&style.stroke), tolerance)?;
            let layer = renderer.layer(
                &outline,
                &style.stroke.paint,
                FillRule::NonZero,
                to_device,
                1.0,
            )?;
            push_optional(layers, layer)
        };
        match style.paint_order {
            PaintOrder::FillFirst => {
                fill(self, &mut layers)?;
                stroke(self, &mut layers)?;
            }
            PaintOrder::StrokeFirst => {
                stroke(self, &mut layers)?;
                fill(self, &mut layers)?;
            }
        }
        for head in arrowheads(path, &style.stroke) {
            if head.bounds().intersects(area) {
                let ring = flattened_ring(&head, tolerance)?;
                let colour = Paint::Colour(style.stroke.head_colour());
                let layer = self.layer(&[ring], &colour, FillRule::NonZero, to_device, 1.0)?;
                push_optional(&mut layers, layer)?;
            }
        }
        if layers.len() == 1 {
            if let Some(mut only) = layers.pop() {
                // One layer alone composites at its alpha times the opacity,
                // which is what isolating it would come to.
                only.paint = weakened(only.paint, opacity);
                return push(out, Art::Fill(only));
            }
        }
        let mut arts = Vec::new();
        if !fallible::reserve(&mut arts, layers.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        arts.extend(layers.into_iter().map(Art::Fill));
        isolated(opacity, None, arts, out)
    }

    /// What a clip's shapes cover, filled opaque, for an alpha mask.
    fn clip(
        &mut self,
        node: &Arc<Node>,
        parent: Affine,
        out: &mut Vec<Art>,
    ) -> Result<(), DrawingError> {
        if !node.visible() {
            return Ok(());
        }
        let to_device = node.transform().then(parent);
        let Some(area) = self.area_in(to_device) else {
            return Ok(());
        };
        let opaque = Paint::Colour(Rgba::new(255, 255, 255, 255));
        match node.kind() {
            Kind::Group { children, clip } => {
                let mut inner = Vec::new();
                for child in children.iter() {
                    self.clip(child, to_device, &mut inner)?;
                }
                let mask = match clip {
                    Some(clip) => {
                        let mut content = Vec::new();
                        self.clip(clip, to_device, &mut content)?;
                        Some(Mask {
                            kind: MaskKind::Alpha,
                            content,
                        })
                    }
                    None => None,
                };
                isolated(1.0, mask, inner, out)
            }
            Kind::Shape { shape, style } => {
                let path = shape.to_path()?;
                let contours =
                    flattened_fill(&path, area, DEVICE_TOLERANCE / to_device.max_scale())?;
                let layer = self.layer(&contours, &opaque, style.fill_rule, to_device, 1.0)?;
                push_layer(out, layer)
            }
            Kind::Text { text, style } => {
                let setting = text.set(self.glyphs)?;
                self.complete &= setting.complete;
                let mut path = Path::new();
                for placed in &setting.glyphs {
                    let set = placed.glyph.outline().transformed(placed.at);
                    if path.subpaths.try_reserve(set.subpaths.len()).is_err() {
                        return Err(DrawingError::OutOfMemory);
                    }
                    path.subpaths.extend(set.subpaths);
                }
                let contours =
                    flattened_fill(&path, area, DEVICE_TOLERANCE / to_device.max_scale())?;
                let layer = self.layer(&contours, &opaque, style.fill_rule, to_device, 1.0)?;
                push_layer(out, layer)
            }
        }
    }

    /// `contours`, in a space `to_device` maps onto the device, filled with
    /// `paint` weakened by `opacity`; `None` when nothing of them reaches the
    /// square or the paint paints nothing.
    fn layer(
        &mut self,
        contours: &[SubPath],
        paint: &Paint,
        rule: FillRule,
        to_device: Affine,
        opacity: f64,
    ) -> Result<Option<Layer>, DrawingError> {
        let Some(painted) = raster_paint(paint, opacity, to_device.then(self.to_design))? else {
            return Ok(None);
        };
        let mut placed = Vec::new();
        for contour in contours {
            let points = contour.points.iter().map(|&(x, y)| Point::new(x, y));
            if let Some(ring) = self.placed(points, to_device)? {
                if !fallible::push(&mut placed, ring) {
                    return Err(DrawingError::OutOfMemory);
                }
            }
        }
        Ok((!placed.is_empty()).then(|| Layer::filled(painted, rule, placed)))
    }

    /// The ring through `points`, mapped onto the device, clipped to the
    /// window and placed on the design grid; `None` when nothing of it is
    /// left.
    fn placed(
        &mut self,
        points: impl Iterator<Item = Point>,
        to_device: Affine,
    ) -> Result<Option<Vec<(i32, i32)>>, DrawingError> {
        let mut ring = Vec::new();
        for point in points {
            if !fallible::push(&mut ring, to_device.map(point)) {
                return Err(DrawingError::OutOfMemory);
            }
        }
        clip_to(&mut ring, &mut self.scratch, self.window)?;
        if ring.len() < 3 {
            return Ok(None);
        }
        let mut placed = Vec::new();
        if !fallible::reserve(&mut placed, ring.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        placed.extend(ring.iter().map(|&point| {
            let at = self.to_design.map(point);
            (round_i32(at.x), round_i32(at.y))
        }));
        Ok(Some(placed))
    }
}

/// `children` composited through `opacity` and `mask`, spliced in place where
/// neither changes the picture.
fn isolated(
    opacity: f64,
    mask: Option<Mask>,
    mut children: Vec<Art>,
    out: &mut Vec<Art>,
) -> Result<(), DrawingError> {
    let alpha = alpha_of(opacity);
    if alpha == 0 || children.is_empty() {
        return Ok(());
    }
    if alpha == u8::MAX && mask.is_none() {
        if out.try_reserve(children.len()).is_err() {
            return Err(DrawingError::OutOfMemory);
        }
        out.append(&mut children);
        return Ok(());
    }
    push(
        out,
        Art::Group(Group {
            opacity: alpha,
            mask,
            children,
        }),
    )
}

fn push(out: &mut Vec<Art>, art: Art) -> Result<(), DrawingError> {
    if fallible::push(out, art) {
        Ok(())
    } else {
        Err(DrawingError::OutOfMemory)
    }
}

fn push_layer(out: &mut Vec<Art>, layer: Option<Layer>) -> Result<(), DrawingError> {
    layer.map_or(Ok(()), |layer| push(out, Art::Fill(layer)))
}

fn push_optional(layers: &mut Vec<Layer>, layer: Option<Layer>) -> Result<(), DrawingError> {
    match layer {
        Some(layer) => {
            if fallible::push(layers, layer) {
                Ok(())
            } else {
                Err(DrawingError::OutOfMemory)
            }
        }
        None => Ok(()),
    }
}

/// The area `polylines` stroked with `pen` cover; past the render's point
/// bound the object is too complex to draw.
fn stroked(
    polylines: &[SubPath],
    pen: &StrokeStyle,
    tolerance: f64,
) -> Result<Vec<SubPath>, DrawingError> {
    stroke_outline(polylines, pen, tolerance, MAX_POINTS).map_err(|_| DrawingError::TooComplex)
}

/// An opacity as the 8-bit alpha a raster group composites at.
fn alpha_of(opacity: f64) -> u8 {
    tairix_svg::number::opacity_to_alpha(opacity)
}

/// `colour` weakened by `opacity`.
fn scaled(colour: Rgba, opacity: f64) -> Color {
    let alpha =
        u8::try_from(round_i32(f64::from(colour.a) * opacity).clamp(0, 255)).unwrap_or(u8::MAX);
    Color::from(Rgba { a: alpha, ..colour })
}

/// `paint` weakened by `opacity`.
fn weakened(paint: Painted, opacity: f64) -> Painted {
    if opacity >= 1.0 {
        return paint;
    }
    let weaken = |colour: Color| {
        let alpha = round_i32(f64::from(colour.a) * opacity).clamp(0, 255);
        Color::rgba(
            colour.r,
            colour.g,
            colour.b,
            u8::try_from(alpha).unwrap_or(u8::MAX),
        )
    };
    match paint {
        Painted::Solid(colour) => Painted::Solid(weaken(colour)),
        Painted::Gradient(mut gradient) => {
            for stop in &mut gradient.stops {
                stop.color = weaken(stop.color);
            }
            Painted::Gradient(gradient)
        }
        Painted::Pattern(mut pattern) => {
            pattern.opacity =
                u8::try_from(round_i32(f64::from(pattern.opacity) * opacity).clamp(0, 255))
                    .unwrap_or(u8::MAX);
            Painted::Pattern(pattern)
        }
    }
}

/// `paint` as the rasteriser paints it, the paint's own space mapped onto
/// the design grid by `to_design`; `None` for a paint that paints nothing.
fn raster_paint(
    paint: &Paint,
    opacity: f64,
    to_design: Affine,
) -> Result<Option<Painted>, DrawingError> {
    match paint {
        Paint::None => Ok(None),
        Paint::Colour(colour) => {
            Ok((colour.a > 0).then(|| Painted::Solid(scaled(*colour, opacity))))
        }
        Paint::Gradient(gradient) => {
            if !paint.paints() {
                return Ok(None);
            }
            let geometry = match gradient.kind {
                GradientKind::Linear { start, end } => GradientGeometry::Linear {
                    start: start.tuple(),
                    end: end.tuple(),
                },
                GradientKind::Radial {
                    centre,
                    radius,
                    focus,
                } => GradientGeometry::Radial {
                    centre: centre.tuple(),
                    radius,
                    focus: focus.tuple(),
                },
            };
            let mut stops = Vec::new();
            if !fallible::reserve(&mut stops, gradient.stops.len()) {
                return Err(DrawingError::OutOfMemory);
            }
            stops.extend(gradient.stops.iter().map(|stop| GradientStop {
                offset: stop.offset,
                color: scaled(stop.colour, opacity),
            }));
            let spread = match gradient.spread {
                Spread::Pad => SpreadMethod::Pad,
                Spread::Reflect => SpreadMethod::Reflect,
                Spread::Repeat => SpreadMethod::Repeat,
            };
            Ok(placed_gradient(
                geometry,
                stops,
                spread,
                gradient.transform.then(to_design),
            ))
        }
    }
}

/// The pen `stroke` draws with, as the stroker takes it: solid, its dashes
/// already cut along the curves.
fn stroke_style(stroke: &Stroke) -> StrokeStyle {
    StrokeStyle {
        width: stroke.width,
        cap: match stroke.cap {
            Cap::Butt => LineCap::Butt,
            Cap::Round => LineCap::Round,
            Cap::Square => LineCap::Square,
        },
        join: match stroke.join {
            Join::Miter => LineJoin::Miter,
            Join::Round => LineJoin::Round,
            Join::Bevel => LineJoin::Bevel,
        },
        miter_limit: stroke.miter_limit,
        dashes: Vec::new(),
        dash_offset: 0.0,
    }
}

/// The arrowheads `stroke` draws at the ends of `path`.
pub(crate) fn arrowheads(path: &Path, stroke: &Stroke) -> impl Iterator<Item = Subpath> {
    let first = path
        .subpaths
        .first()
        .and_then(|subpath| subpath.segments().next());
    let last = path
        .subpaths
        .last()
        .and_then(|subpath| subpath.segments().last());
    let start = first
        .filter(|_| stroke.draws_head(&stroke.start))
        .and_then(|segment| {
            stroke
                .start
                .outline(segment.eval(0.0), -segment.tangents().0, stroke.width)
        });
    let end = last
        .filter(|_| stroke.draws_head(&stroke.end))
        .and_then(|segment| {
            stroke
                .end
                .outline(segment.eval(1.0), segment.tangents().1, stroke.width)
        });
    start.into_iter().chain(end)
}

/// Counts the points a render takes from one node.
struct Budget(usize);

impl Budget {
    fn take(&mut self, points: usize) -> Result<(), DrawingError> {
        self.0 = self.0.checked_sub(points).ok_or(DrawingError::TooComplex)?;
        Ok(())
    }
}

/// `path`'s fill contours as they can affect `area`: a sub-path whose bounds
/// miss it is dropped, since it winds nothing there, and a curve whose
/// control hull misses it is its chord, since bending the outline within a
/// convex hull that holds none of the area changes no winding inside it.
fn flattened_fill(path: &Path, area: Bounds, tolerance: f64) -> Result<Vec<SubPath>, DrawingError> {
    let mut budget = Budget(MAX_POINTS);
    let mut out = Vec::new();
    for subpath in &path.subpaths {
        if !subpath_hull(subpath).intersects(area) {
            continue;
        }
        let mut points = Vec::new();
        push_point(&mut points, subpath.start, &mut budget)?;
        for segment in subpath.segments() {
            flatten_near(segment, area, tolerance, &mut points, &mut budget)?;
        }
        if !fallible::push(&mut out, SubPath::closed(points)) {
            return Err(DrawingError::OutOfMemory);
        }
    }
    Ok(out)
}

/// `path`'s stroke as it can reach `area`: each sub-path cut into the runs of
/// segments whose control hulls meet it. Every cut falls at a vertex beyond
/// the stroke's reach, so the caps and joins a cut changes are drawn nowhere
/// near the area; a closed sub-path is walked from just past a segment that
/// misses, so no run is cut where it wraps round.
fn flattened_runs(path: &Path, area: Bounds, tolerance: f64) -> Result<Vec<SubPath>, DrawingError> {
    let mut budget = Budget(MAX_POINTS);
    let mut out = Vec::new();
    let near = |segment: Segment| segment_hull(segment).intersects(area);
    for subpath in &path.subpaths {
        if !subpath_hull(subpath).intersects(area) {
            continue;
        }
        let Some(first_far) = subpath.segments().position(|segment| !near(segment)) else {
            let mut points = Vec::new();
            push_point(&mut points, subpath.start, &mut budget)?;
            for segment in subpath.segments() {
                flatten_near(segment, area, tolerance, &mut points, &mut budget)?;
            }
            if subpath.closed && points.len() > 1 && points.first() == points.last() {
                points.pop();
            }
            let run = SubPath {
                points,
                closed: subpath.closed,
            };
            if !fallible::push(&mut out, run) {
                return Err(DrawingError::OutOfMemory);
            }
            continue;
        };
        let turn = if subpath.closed { first_far + 1 } else { 0 };
        let walk = subpath
            .segments()
            .skip(turn)
            .chain(subpath.segments().take(turn));
        let mut run: Vec<(f64, f64)> = Vec::new();
        for segment in walk {
            if near(segment) {
                if run.is_empty() {
                    push_point(&mut run, segment.eval(0.0), &mut budget)?;
                }
                flatten_near(segment, area, tolerance, &mut run, &mut budget)?;
            } else if !run.is_empty()
                && !fallible::push(&mut out, SubPath::open(core::mem::take(&mut run)))
            {
                return Err(DrawingError::OutOfMemory);
            }
        }
        if !run.is_empty() && !fallible::push(&mut out, SubPath::open(run)) {
            return Err(DrawingError::OutOfMemory);
        }
    }
    Ok(out)
}

/// A closed ring through `subpath`, flattened whole.
fn flattened_ring(subpath: &Subpath, tolerance: f64) -> Result<SubPath, DrawingError> {
    let mut budget = Budget(MAX_POINTS);
    let mut points = Vec::new();
    push_point(&mut points, subpath.start, &mut budget)?;
    for segment in subpath.segments() {
        flatten_near(
            segment,
            segment_hull(segment),
            tolerance,
            &mut points,
            &mut budget,
        )?;
    }
    Ok(SubPath::closed(points))
}

/// Append `segment`'s points after its start: flattened where its hull meets
/// `area`, its end alone where it does not.
fn flatten_near(
    segment: Segment,
    area: Bounds,
    tolerance: f64,
    points: &mut Vec<(f64, f64)>,
    budget: &mut Budget,
) -> Result<(), DrawingError> {
    match segment {
        Segment::Line(line) => push_point(points, line.p1, budget),
        Segment::Cubic(cubic) if cubic.control_bounds().intersects(area) => {
            let mut failed = None;
            flatten_cubic(cubic, tolerance, |point| {
                if failed.is_none() {
                    failed = push_point(points, point, budget).err();
                }
            });
            failed.map_or(Ok(()), Err)
        }
        Segment::Cubic(cubic) => push_point(points, cubic.p3, budget),
    }
}

fn push_point(
    points: &mut Vec<(f64, f64)>,
    point: Point,
    budget: &mut Budget,
) -> Result<(), DrawingError> {
    budget.take(1)?;
    if fallible::push(points, point.tuple()) {
        Ok(())
    } else {
        Err(DrawingError::OutOfMemory)
    }
}

/// Bounds holding a segment and its control points.
fn segment_hull(segment: Segment) -> Bounds {
    match segment {
        Segment::Line(line) => Bounds::new(line.p0, line.p1),
        Segment::Cubic(cubic) => cubic.control_bounds(),
    }
}

/// Bounds holding a sub-path and every control point it has.
fn subpath_hull(subpath: &Subpath) -> Bounds {
    subpath
        .segments()
        .fold(Bounds::of_point(subpath.start), |bounds, segment| {
            bounds.union(segment_hull(segment))
        })
}

/// One edge of the clipping window, and the coordinate it stands at.
#[derive(Copy, Clone)]
enum Edge {
    Left(f64),
    Right(f64),
    Top(f64),
    Bottom(f64),
}

impl Edge {
    /// The coordinate this edge cuts across, and the edge's own.
    const fn coordinates(self, p: Point) -> (f64, f64) {
        match self {
            Self::Left(x) | Self::Right(x) => (p.x, x),
            Self::Top(y) | Self::Bottom(y) => (p.y, y),
        }
    }

    fn inside(self, p: Point) -> bool {
        let (at, edge) = self.coordinates(p);
        match self {
            Self::Left(_) | Self::Top(_) => at >= edge,
            Self::Right(_) | Self::Bottom(_) => at <= edge,
        }
    }

    /// Where the step from `a` to `b`, which crosses this edge, meets it.
    fn crossing(self, a: Point, b: Point) -> Point {
        let ((from, edge), (to, _)) = (self.coordinates(a), self.coordinates(b));
        a.lerp(b, (edge - from) / (to - from))
    }
}

/// Clip the ring `points` to `window` in place, Sutherland and Hodgman's way:
/// against each edge in turn, keeping what lies inside and the crossings.
/// Clipping to a convex window keeps the winding of every point inside it.
fn clip_to(
    points: &mut Vec<Point>,
    scratch: &mut Vec<Point>,
    window: Bounds,
) -> Result<(), DrawingError> {
    let edges = [
        Edge::Left(window.min.x),
        Edge::Right(window.max.x),
        Edge::Top(window.min.y),
        Edge::Bottom(window.max.y),
    ];
    for edge in edges {
        if points.iter().all(|&p| edge.inside(p)) {
            continue;
        }
        let Some(&last) = points.last() else {
            return Ok(());
        };
        scratch.clear();
        if !fallible::reserve(scratch, points.len() + points.len() / 2 + 2) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut previous = last;
        for &current in points.iter() {
            let (was, is) = (edge.inside(previous), edge.inside(current));
            if was != is && !fallible::push(scratch, edge.crossing(previous, current)) {
                return Err(DrawingError::OutOfMemory);
            }
            if is && !fallible::push(scratch, current) {
                return Err(DrawingError::OutOfMemory);
            }
            previous = current;
        }
        core::mem::swap(points, scratch);
        if points.len() < 3 {
            points.clear();
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;

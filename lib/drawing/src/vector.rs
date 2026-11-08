//! The walk the vector formats share: a drawing as paths painted in order,
//! each object in its own space, its text as its glyphs' outlines, a clip of
//! several shapes as their exact union, a gradient as a shading over the
//! range its shape needs and arrowheads after their stroke. PDF and
//! PostScript spell the same operators differently and paint transparency
//! differently, so each format is a [`Sink`] the walk writes through.

use alloc::vec::Vec;
use core::fmt::{self, Write};

use tairix_bezier::boolean::{boolean, Operation};
use tairix_bezier::{Affine, Bounds, FillRule, Path, PathEl};
use tairix_colour::Rgba;
use tairix_compress::deflate::Flush;
use tairix_compress::zlib::Encoder;
use tairix_image::JpegFrame;
use tairix_raster::{Pixel, Surface};
use tairix_util::fallible::{self, Text};
use tairix_util::mathf::{ceil, floor, fmax, fmin, round_i32};

use crate::drawing::Page;
use crate::image::{Encoding, Image, Source};
use crate::node::{Kind, Node};
use crate::render::{arrowheads, stroke_area};
use crate::style::{Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Stroke, Style};
use crate::text::Glyphs;
use crate::DrawingError;

/// Points per pixel: both formats measure in points, the drawing in CSS
/// pixels.
pub(crate) const POINTS_PER_PIXEL: f64 = 0.75;

/// The most periods a reflected or repeated gradient is stitched over; a
/// shape needing more is painted with its ramp's ends carried on beyond.
const MAX_PERIODS: f64 = 256.0;

pub(crate) fn written(result: fmt::Result) -> Result<(), DrawingError> {
    result.map_err(|_| DrawingError::OutOfMemory)
}

/// A number as both formats read one: decimals only, never an exponent, to
/// four places.
pub(crate) struct Real(pub(crate) f64);

impl fmt::Display for Real {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = if self.0.is_finite() { self.0 } else { 0.0 };
        let mut text = tairix_inline::ArrayString::<48>::new();
        if fmt::write(&mut text, format_args!("{value:.4}")).is_err() {
            return Err(fmt::Error);
        }
        let mut spelled = text.as_str();
        if spelled.contains('.') {
            spelled = spelled.trim_end_matches('0').trim_end_matches('.');
        }
        if spelled == "-0" || spelled.is_empty() {
            spelled = "0";
        }
        f.write_str(spelled)
    }
}

/// An affine map's six coefficients, as both formats list them.
pub(crate) struct Matrix(pub(crate) Affine);

impl fmt::Display for Matrix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(
            f,
            "{} {} {} {} {} {}",
            Real(m.a),
            Real(m.b),
            Real(m.c),
            Real(m.d),
            Real(m.e),
            Real(m.f)
        )
    }
}

/// How a format spells the operators the walk writes. A matrix and a
/// rectangle are written with their operands: PostScript takes a matrix as
/// an array, PDF as six numbers.
pub(crate) struct Dialect {
    pub(crate) move_to: &'static str,
    pub(crate) line_to: &'static str,
    pub(crate) curve_to: &'static str,
    pub(crate) close: &'static str,
    pub(crate) fill: &'static str,
    pub(crate) even_odd_fill: &'static str,
    pub(crate) stroke: &'static str,
    /// Clip to the path and drop it.
    pub(crate) clip: &'static str,
    pub(crate) even_odd_clip: &'static str,
    /// Clip to no area at all.
    pub(crate) clip_nothing: &'static str,
    pub(crate) save: &'static str,
    pub(crate) restore: &'static str,
    pub(crate) line_width: &'static str,
    pub(crate) line_cap: &'static str,
    pub(crate) line_join: &'static str,
    pub(crate) miter_limit: &'static str,
    pub(crate) dash: &'static str,
    pub(crate) concat: fn(&mut Text, Affine) -> fmt::Result,
    pub(crate) fill_rect: fn(&mut Text, Bounds) -> fmt::Result,
}

/// What a format paints with: its colours, its shadings, and how it
/// composites an object as a whole.
pub(crate) trait Sink {
    /// The format's spelling of the walk's operators.
    const DIALECT: Dialect;

    /// Set `colour`, weakened by `opacity`, as what strokes paint, or fills
    /// where not `stroking`; on the line being written.
    fn colour(
        &mut self,
        out: &mut Text,
        colour: Rgba,
        opacity: f64,
        stroking: bool,
    ) -> Result<(), DrawingError>;

    /// Paint the current clip with `gradient` shaded over `geometry`,
    /// weakened by `opacity`: `reach` bounds the clip in the gradient's own
    /// space, which the walk has already mapped onto.
    fn shade(
        &mut self,
        out: &mut Text,
        gradient: &Gradient,
        geometry: &Geometry,
        painted: (f64, Bounds),
    ) -> Result<(), DrawingError>;

    /// Paint `inner`, an object's content bounded by `bounds`, composited as
    /// a whole at `opacity`.
    fn isolated(
        &mut self,
        out: &mut Text,
        inner: &str,
        bounds: Bounds,
        opacity: f64,
    ) -> Result<(), DrawingError>;

    /// Paint `image` over its rectangle in the current space, weakened by
    /// `opacity`.
    fn image(&mut self, out: &mut Text, image: &Image, opacity: f64) -> Result<(), DrawingError>;
}

/// The walk over a drawing, writing through `sink`.
pub(crate) struct Walk<'g, S> {
    pub(crate) sink: S,
    glyphs: &'g dyn Glyphs,
    /// Whether every glyph its text needs was known.
    pub(crate) complete: bool,
}

impl<'g, S: Sink> Walk<'g, S> {
    pub(crate) fn new(sink: S, glyphs: &'g dyn Glyphs) -> Self {
        Self {
            sink,
            glyphs,
            complete: true,
        }
    }

    /// The page's ground, where it has one, beneath everything.
    pub(crate) fn ground(&mut self, out: &mut Text, page: &Page) -> Result<(), DrawingError> {
        let Some(background) = page.background else {
            return Ok(());
        };
        self.sink.colour(out, background, 1.0, false)?;
        written(out.write_char('\n'))?;
        written((S::DIALECT.fill_rect)(out, page.bounds()))
    }

    pub(crate) fn node(&mut self, out: &mut Text, node: &Node) -> Result<(), DrawingError> {
        if !node.visible() {
            return Ok(());
        }
        let opacity = node.opacity();
        if opacity <= 0.0 {
            return Ok(());
        }
        if opacity < 1.0 && isolates(node) {
            let mut inner = Text::new();
            self.content(&mut inner, node, 1.0)?;
            return self
                .sink
                .isolated(out, inner.as_str(), node.bounds(), opacity);
        }
        self.content(out, node, opacity)
    }

    /// `node` in its own space, its opacity `opacity` folded into its one
    /// paint.
    fn content(&mut self, out: &mut Text, node: &Node, opacity: f64) -> Result<(), DrawingError> {
        let dialect = &S::DIALECT;
        written(writeln!(out, "{}", dialect.save))?;
        if node.transform() != Affine::IDENTITY {
            written((dialect.concat)(out, node.transform()))?;
        }
        match node.kind() {
            Kind::Group { children, clip } => {
                if let Some(clip) = clip {
                    match self.clip_path(clip)? {
                        Some(covered) => {
                            path_ops(out, &covered, dialect)?;
                            written(writeln!(out, "{}", dialect.clip))?;
                        }
                        None => written(writeln!(out, "{}", dialect.clip_nothing))?,
                    }
                }
                for child in children.iter() {
                    self.node(out, child)?;
                }
            }
            Kind::Shape { shape, style } => {
                let path = shape.to_path()?;
                self.painted(out, &path, style, opacity)?;
            }
            Kind::Text { text, style } => {
                let setting = text.set(self.glyphs)?;
                self.complete &= setting.complete;
                for part in setting.painted(text) {
                    let path = setting.outline_of(part.glyphs.clone(), |_| true)?;
                    self.painted(out, &path, &part.style(style), opacity)?;
                }
            }
            Kind::Image { image } => self.sink.image(out, image, opacity)?,
        }
        written(writeln!(out, "{}", dialect.restore))
    }

    /// The outlines a text node's glyphs set, in its own space.
    fn text_path(&mut self, node: &Node) -> Result<Path, DrawingError> {
        let Kind::Text { text, .. } = node.kind() else {
            return Ok(Path::new());
        };
        let setting = text.set(self.glyphs)?;
        self.complete &= setting.complete;
        setting.outline(|_| true)
    }

    /// The area a clip node covers in its parent's space, as one path filled
    /// non-zero: several shapes are their exact union. `None` covers
    /// nothing.
    fn clip_path(&mut self, node: &Node) -> Result<Option<Path>, DrawingError> {
        if !node.visible() {
            return Ok(None);
        }
        let union = |a: &Path, a_rule, b: &Path| {
            boolean(a, a_rule, b, FillRule::NonZero, Operation::Union).map_err(DrawingError::from)
        };
        let covered = match node.kind() {
            Kind::Shape { shape, style } => {
                union(&shape.to_path()?, style.fill_rule, &Path::new())?
            }
            Kind::Text { .. } => union(&self.text_path(node)?, FillRule::NonZero, &Path::new())?,
            Kind::Image { image } => image.outline()?,
            Kind::Group { children, clip } => {
                let mut covered = Path::new();
                for child in children.iter() {
                    if let Some(part) = self.clip_path(child)? {
                        covered = union(&covered, FillRule::NonZero, &part)?;
                    }
                }
                if let Some(clip) = clip {
                    let Some(window) = self.clip_path(clip)? else {
                        return Ok(None);
                    };
                    covered = boolean(
                        &covered,
                        FillRule::NonZero,
                        &window,
                        FillRule::NonZero,
                        Operation::Intersection,
                    )
                    .map_err(DrawingError::from)?;
                }
                covered
            }
        };
        let placed = covered.transformed(node.transform())?;
        Ok((!placed.is_empty()).then_some(placed))
    }

    /// `path` painted as `style` says, weakened by `opacity`.
    fn painted(
        &mut self,
        out: &mut Text,
        path: &Path,
        style: &Style,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        let order = match style.paint_order {
            PaintOrder::FillFirst => [true, false],
            PaintOrder::StrokeFirst => [false, true],
        };
        for fill in order {
            if fill {
                self.fill(out, path, &style.fill, style.fill_rule, opacity)?;
            } else {
                self.stroke(out, path, &style.stroke, opacity)?;
            }
        }
        for head in arrowheads(path, &style.stroke)?.into_iter().flatten() {
            let head = Path::single(head)?;
            self.fill(
                out,
                &head,
                &Paint::Colour(style.stroke.head_colour()),
                FillRule::NonZero,
                opacity,
            )?;
        }
        Ok(())
    }

    fn fill(
        &mut self,
        out: &mut Text,
        path: &Path,
        paint: &Paint,
        rule: FillRule,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        let dialect = &S::DIALECT;
        let even_odd = rule == FillRule::EvenOdd;
        match paint {
            Paint::None => Ok(()),
            Paint::Colour(colour) if colour.a == 0 => Ok(()),
            Paint::Colour(colour) => {
                self.sink.colour(out, *colour, opacity, false)?;
                written(out.write_char('\n'))?;
                path_ops(out, path, dialect)?;
                let operator = if even_odd {
                    dialect.even_odd_fill
                } else {
                    dialect.fill
                };
                written(writeln!(out, "{operator}"))
            }
            Paint::Gradient(gradient) => {
                written(writeln!(out, "{}", dialect.save))?;
                path_ops(out, path, dialect)?;
                let clip = if even_odd {
                    dialect.even_odd_clip
                } else {
                    dialect.clip
                };
                written(writeln!(out, "{clip}"))?;
                self.gradient(out, gradient, opacity, path.bounds())?;
                written(writeln!(out, "{}", dialect.restore))
            }
        }
    }

    fn stroke(
        &mut self,
        out: &mut Text,
        path: &Path,
        stroke: &Stroke,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        if !stroke.paints() {
            return Ok(());
        }
        if let Paint::Gradient(_) = stroke.paint {
            // Both formats stroke with a colour alone: a gradient fills the
            // stroke's outline instead.
            let area = stroke_area(path, stroke)?;
            return self.fill(out, &area, &stroke.paint, FillRule::NonZero, opacity);
        }
        let Paint::Colour(colour) = stroke.paint else {
            return Ok(());
        };
        let dialect = &S::DIALECT;
        self.sink.colour(out, colour, opacity, true)?;
        written(write!(
            out,
            " {} {} {} {} {} {} {} {}",
            Real(stroke.width),
            dialect.line_width,
            match stroke.cap {
                Cap::Butt => 0,
                Cap::Round => 1,
                Cap::Square => 2,
            },
            dialect.line_cap,
            match stroke.join {
                Join::Miter => 0,
                Join::Round => 1,
                Join::Bevel => 2,
            },
            dialect.line_join,
            Real(stroke.miter_limit),
            dialect.miter_limit
        ))?;
        if !stroke.dashes.is_empty() {
            written(out.write_str(" ["))?;
            for dash in stroke.dashes.iter() {
                written(write!(out, " {}", Real(*dash)))?;
            }
            written(write!(
                out,
                " ] {} {}",
                Real(stroke.dash_offset),
                dialect.dash
            ))?;
        }
        written(out.write_char('\n'))?;
        path_ops(out, path, dialect)?;
        written(writeln!(out, "{}", dialect.stroke))
    }

    /// Paint the current clip with `gradient` weakened by `opacity`, `area`
    /// bounding what it fills in the object's space.
    fn gradient(
        &mut self,
        out: &mut Text,
        gradient: &Gradient,
        opacity: f64,
        area: Bounds,
    ) -> Result<(), DrawingError> {
        let dialect = &S::DIALECT;
        let last = gradient
            .stops
            .last()
            .map_or(Rgba::new(0, 0, 0, 0), |stop| stop.colour);
        // A gradient its map collapses, or one with no extent to run along,
        // paints its last colour, as SVG paints it.
        let placed = gradient.transform.invert().and_then(|back| {
            let reach = area.transformed(back);
            Geometry::of(gradient, reach).map(|geometry| (geometry, reach))
        });
        let Some((geometry, reach)) = placed else {
            self.sink.colour(out, last, opacity, false)?;
            written(out.write_char('\n'))?;
            return written((dialect.fill_rect)(out, area));
        };
        written((dialect.concat)(out, gradient.transform))?;
        self.sink.shade(out, gradient, &geometry, (opacity, reach))
    }
}

/// Whether `node`'s opacity, when it has one, must composite it as a whole:
/// anything that paints more than once over itself.
fn isolates(node: &Node) -> bool {
    match node.kind() {
        Kind::Group { .. } => true,
        Kind::Image { .. } => false,
        Kind::Text { text, .. } if text.repaints() => true,
        Kind::Shape { style, .. } | Kind::Text { style, .. } => {
            let heads = [style.stroke.start, style.stroke.end]
                .iter()
                .any(|head| style.stroke.draws_head(head));
            (style.fill.paints() && style.stroke.paints()) || heads
        }
    }
}

/// `path` as path construction operators, a segment to a line.
fn path_ops(out: &mut Text, path: &Path, dialect: &Dialect) -> Result<(), DrawingError> {
    for subpath in &path.subpaths {
        written(writeln!(
            out,
            "{} {} {}",
            Real(subpath.start.x),
            Real(subpath.start.y),
            dialect.move_to
        ))?;
        for element in &subpath.elements {
            match *element {
                PathEl::Line(to) => written(writeln!(
                    out,
                    "{} {} {}",
                    Real(to.x),
                    Real(to.y),
                    dialect.line_to
                ))?,
                PathEl::Cubic(c1, c2, to) => written(writeln!(
                    out,
                    "{} {} {} {} {} {} {}",
                    Real(c1.x),
                    Real(c1.y),
                    Real(c2.x),
                    Real(c2.y),
                    Real(to.x),
                    Real(to.y),
                    dialect.curve_to
                ))?,
            }
        }
        if subpath.closed {
            written(writeln!(out, "{}", dialect.close))?;
        }
    }
    Ok(())
}

/// What a format passes a picture on as: the JPEG it was read from, as it
/// came, or its own pixels.
pub(crate) enum Raster<'i> {
    /// JPEG bytes and what their frame states.
    Jpeg(&'i [u8], JpegFrame),
    /// The picture's pixels, premultiplied.
    Pixels(&'i Surface),
}

impl<'i> Raster<'i> {
    /// `image` as a format passes it on: its JPEG where it was read from one
    /// the format can take — any, or a baseline one where not `progressive`
    /// — and its pixels otherwise.
    pub(crate) fn of(image: &'i Image, progressive: bool) -> Self {
        match image.source() {
            Some(Source {
                encoding: Encoding::Jpeg(frame),
                bytes,
            }) if progressive || !frame.progressive => Self::Jpeg(bytes, *frame),
            _ => Self::Pixels(image.pixels()),
        }
    }

    /// Its width and height in pixels, as stored.
    pub(crate) fn size(&self) -> (u32, u32) {
        match self {
            Self::Jpeg(_, frame) => frame.stored,
            Self::Pixels(pixels) => (pixels.width(), pixels.height()),
        }
    }

    /// The map from its pixels, a pixel to the unit and the first row at the
    /// top, onto `image`'s rectangle: a stored JPEG is turned as its frame
    /// says, since neither format reads that from its bytes.
    pub(crate) fn to_place(&self, image: &Image) -> Affine {
        match self {
            Self::Jpeg(_, frame) => frame
                .orientation
                .to_picture(frame.stored)
                .then(image.to_place()),
            Self::Pixels(_) => image.to_place(),
        }
    }
}

/// How a picture's pixels are written, sample by sample, straight.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Samples {
    Rgb,
    Grey,
    Alpha,
    /// Each pixel's mask, shown or not, before its colour: a PostScript
    /// masked image interleaved by sample, where a mask sample of nought
    /// paints.
    MaskedRgb,
    MaskedGrey,
}

impl Samples {
    /// Bytes a pixel takes.
    pub(crate) const fn bytes(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Grey | Self::Alpha => 1,
            Self::MaskedRgb => 4,
            Self::MaskedGrey => 2,
        }
    }

    /// The row `rows[1]`, between the rows above and below it, onto `out`.
    fn write(self, rows: [Option<&[Pixel]>; 3], out: &mut [u8]) {
        let row = rows[1].unwrap_or_default();
        for (x, (pixel, sample)) in row
            .iter()
            .zip(out.chunks_exact_mut(self.bytes()))
            .enumerate()
        {
            let colour = if pixel.a == 0 {
                bled(rows, x)
            } else {
                pixel.unpremultiply()
            };
            let mask = if pixel.a >= 0x80 { 0 } else { u8::MAX };
            match self {
                Self::Rgb => sample.copy_from_slice(&[colour.r, colour.g, colour.b]),
                Self::Grey => sample[0] = colour.r,
                Self::Alpha => sample[0] = pixel.a,
                Self::MaskedRgb => sample.copy_from_slice(&[mask, colour.r, colour.g, colour.b]),
                Self::MaskedGrey => sample.copy_from_slice(&[mask, colour.r]),
            }
        }
    }
}

/// The colour a wholly clear pixel at `x` of the middle of `rows` is written
/// in: its neighbours' own, weighted by how much each shows, so a viewer that
/// smooths a picture blends its edge with its colours rather than with black.
fn bled(rows: [Option<&[Pixel]>; 3], x: usize) -> tairix_raster::Color {
    let (mut sum, mut alpha) = ([0_u32; 3], 0_u32);
    for row in rows.into_iter().flatten() {
        let around = row
            .get(x.saturating_sub(1)..(x + 2).min(row.len()))
            .unwrap_or_default();
        for pixel in around {
            sum[0] += u32::from(pixel.r);
            sum[1] += u32::from(pixel.g);
            sum[2] += u32::from(pixel.b);
            alpha += u32::from(pixel.a);
        }
    }
    if alpha == 0 {
        return tairix_raster::Color::TRANSPARENT;
    }
    let [r, g, b] =
        sum.map(|channel| u8::try_from((channel * 255 + alpha / 2) / alpha).unwrap_or(u8::MAX));
    tairix_raster::Color::rgba(r, g, b, 0)
}

/// Pieces deflated into one zlib stream as they come, so what they make is
/// never held whole before it is compressed.
pub(crate) struct Deflating<'e> {
    encoder: &'e mut Encoder,
    packed: Vec<u8>,
    filled: usize,
}

impl<'e> Deflating<'e> {
    pub(crate) fn new(encoder: &'e mut Encoder) -> Self {
        encoder.reset();
        Self {
            encoder,
            packed: Vec::new(),
            filled: 0,
        }
    }

    pub(crate) fn push(&mut self, piece: &[u8]) -> Result<(), DrawingError> {
        self.compress(piece, Flush::None)
    }

    fn compress(&mut self, piece: &[u8], flush: Flush) -> Result<(), DrawingError> {
        let end = self
            .filled
            .checked_add(self.encoder.bound(piece.len()))
            .ok_or(DrawingError::OutOfMemory)?;
        if !fallible::grow_to(&mut self.packed, end, 0) {
            return Err(DrawingError::OutOfMemory);
        }
        self.filled += self
            .encoder
            .compress(piece, &mut self.packed[self.filled..end], flush)
            .map_err(|_| DrawingError::OutOfMemory)?;
        Ok(())
    }

    /// The stream, ended.
    pub(crate) fn finish(mut self) -> Result<Vec<u8>, DrawingError> {
        self.compress(&[], Flush::Finish)?;
        self.packed.truncate(self.filled);
        Ok(self.packed)
    }

    /// `pixels` deflated a row at a time as `samples` writes them.
    pub(crate) fn pixels(
        mut self,
        pixels: &Surface,
        samples: Samples,
    ) -> Result<Vec<u8>, DrawingError> {
        let width = pixels.width() as usize;
        let mut row = fallible::zeroed(width * samples.bytes()).ok_or(DrawingError::OutOfMemory)?;
        let mut rows = pixels.pixels().chunks_exact(width.max(1)).peekable();
        let mut above = None;
        while let Some(line) = rows.next() {
            samples.write([above, Some(line), rows.peek().copied()], &mut row);
            self.push(&row)?;
            above = Some(line);
        }
        self.finish()
    }
}

/// Which part of a gradient's stops a shading carries.
#[derive(Copy, Clone)]
pub(crate) enum Channel {
    Colour,
    /// Their alpha, weakened by the opacity, as grey.
    Alpha(f64),
}

/// Where a shading's ramp runs, in the gradient's own space: its
/// coordinates and the parameter range it covers.
pub(crate) struct Geometry {
    coords: [f64; 6],
    radial: bool,
    /// The whole periods the shading spans, `0..1` for a padded ramp.
    first: f64,
    last: f64,
}

impl Geometry {
    /// The geometry `gradient` paints across `reach`, the area it fills in
    /// its own space; `None` for one with no extent to run along.
    fn of(gradient: &Gradient, reach: Bounds) -> Option<Self> {
        let corners = reach.corners();
        match gradient.kind {
            GradientKind::Linear { start, end } => {
                let along = end - start;
                let length = along.dot(along);
                if length <= 0.0 {
                    return None;
                }
                let (mut first, mut last) = (0.0, 1.0);
                if gradient.spread != Spread::Pad {
                    let ts = corners.map(|corner| (corner - start).dot(along) / length);
                    first = floor(ts.iter().copied().fold(f64::INFINITY, fmin));
                    last = ceil(ts.iter().copied().fold(f64::NEG_INFINITY, fmax));
                    if last - first > MAX_PERIODS || !(first.is_finite() && last.is_finite()) {
                        first = 0.0;
                        last = 1.0;
                    }
                }
                let from = start + along * first;
                let to = start + along * last;
                Some(Self {
                    coords: [from.x, from.y, to.x, to.y, 0.0, 0.0],
                    radial: false,
                    first,
                    last,
                })
            }
            GradientKind::Radial {
                centre,
                radius,
                focus,
            } => {
                if radius <= 0.0 {
                    return None;
                }
                let mut last = 1.0;
                if gradient.spread != Spread::Pad {
                    let offset = (centre - focus).length();
                    let room = radius - offset;
                    let far = corners
                        .iter()
                        .map(|corner| corner.distance(focus))
                        .fold(0.0, fmax);
                    last = if room > 0.0 { ceil(far / room) } else { 1.0 };
                    if last > MAX_PERIODS || !last.is_finite() {
                        last = 1.0;
                    }
                }
                let to = focus + (centre - focus) * last;
                Some(Self {
                    coords: [focus.x, focus.y, 0.0, to.x, to.y, radius * last],
                    radial: true,
                    first: 0.0,
                    last,
                })
            }
        }
    }

    /// The shading dictionary painting `gradient`'s `channel` over this
    /// geometry, as PDF and PostScript both read it.
    pub(crate) fn shading(
        &self,
        gradient: &Gradient,
        channel: Channel,
    ) -> Result<Text, DrawingError> {
        let space = match channel {
            Channel::Colour => "/DeviceRGB",
            Channel::Alpha(_) => "/DeviceGray",
        };
        let mut ramped = Text::new();
        ramp(&mut ramped, gradient, channel)?;
        let ramp = ramped.as_str();
        let mut out = Text::new();
        written(write!(
            out,
            "<< /ShadingType {} /ColorSpace {space} /Coords [",
            if self.radial { 3 } else { 2 }
        ))?;
        let used = if self.radial { 6 } else { 4 };
        for value in &self.coords[..used] {
            written(write!(out, " {}", Real(*value)))?;
        }
        written(write!(
            out,
            " ] /Domain [{} {}] /Extend [true true] /Function ",
            Real(self.first),
            Real(self.last)
        ))?;
        // Whole periods both, at most `MAX_PERIODS` apart.
        let first = round_i32(self.first);
        let periods = round_i32(self.last) - first;
        if periods <= 1 {
            written(out.write_str(ramp))?;
        } else {
            // A stitching function of one ramp per period, each run forwards
            // or, reflecting, backwards on alternate periods.
            written(write!(
                out,
                "<< /FunctionType 3 /Domain [{} {}] /Functions [",
                Real(self.first),
                Real(self.last)
            ))?;
            for _ in 0..periods {
                written(write!(out, " {ramp}"))?;
            }
            written(out.write_str(" ] /Bounds ["))?;
            for period in 1..periods {
                written(write!(out, " {}", first + period))?;
            }
            written(out.write_str(" ] /Encode ["))?;
            for period in 0..periods {
                let backwards =
                    gradient.spread == Spread::Reflect && (first + period).rem_euclid(2) == 1;
                written(out.write_str(if backwards { " 1 0" } else { " 0 1" }))?;
            }
            written(out.write_str(" ] >>"))?;
        }
        written(out.write_str(" >>"))?;
        Ok(out)
    }
}

/// What a stop paints in `channel`, as both formats spell a function's
/// values.
struct Values(Rgba, Channel);

impl fmt::Display for Values {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(colour, channel) = *self;
        match channel {
            Channel::Colour => write!(
                f,
                "{} {} {}",
                Real(f64::from(colour.r) / 255.0),
                Real(f64::from(colour.g) / 255.0),
                Real(f64::from(colour.b) / 255.0)
            ),
            Channel::Alpha(opacity) => write!(f, "{}", Real(f64::from(colour.a) / 255.0 * opacity)),
        }
    }
}

/// One linear piece of a ramp, from one colour to another.
struct Piece(Rgba, Rgba, Channel);

impl fmt::Display for Piece {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(from, to, channel) = *self;
        write!(
            f,
            "<< /FunctionType 2 /Domain [0 1] /C0 [{}] /C1 [{}] /N 1 >>",
            Values(from, channel),
            Values(to, channel)
        )
    }
}

/// The function a gradient's stops ramp through over `0..=1`, one linear
/// piece between each two stops that stand apart, onto `out`.
fn ramp(out: &mut Text, gradient: &Gradient, channel: Channel) -> Result<(), DrawingError> {
    let stops = &gradient.stops;
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        let clear = Rgba::new(0, 0, 0, 0);
        return written(write!(out, "{}", Piece(clear, clear, channel)));
    };
    // Every boundary where the ramp changes piece, with the colours either
    // side of it; the ends carry the end colours to 0 and 1.
    let mut points: Vec<(f64, Rgba)> = Vec::new();
    if !fallible::reserve(&mut points, stops.len() + 2) {
        return Err(DrawingError::OutOfMemory);
    }
    points.push((0.0, first.colour));
    points.extend(stops.iter().map(|stop| (stop.offset, stop.colour)));
    points.push((1.0, last.colour));
    let apart = || {
        points
            .windows(2)
            .filter(|pair| pair[1].0 > pair[0].0)
            .map(|pair| (pair[0], pair[1]))
    };
    let mut pieces = apart();
    let (Some(((_, from), (_, to))), None) = (pieces.next(), pieces.next()) else {
        let count = apart().count();
        if count == 0 {
            return written(write!(out, "{}", Piece(last.colour, last.colour, channel)));
        }
        written(out.write_str("<< /FunctionType 3 /Domain [0 1] /Functions ["))?;
        for ((_, from), (_, to)) in apart() {
            written(write!(out, " {}", Piece(from, to, channel)))?;
        }
        written(out.write_str(" ] /Bounds ["))?;
        for ((from_at, _), _) in apart().skip(1) {
            written(write!(out, " {}", Real(from_at)))?;
        }
        written(out.write_str(" ] /Encode ["))?;
        for _ in 0..count {
            written(out.write_str(" 0 1"))?;
        }
        return written(out.write_str(" ] >>"));
    };
    written(write!(out, "{}", Piece(from, to, channel)))
}

#[cfg(test)]
#[path = "vector_tests.rs"]
pub(crate) mod tests;

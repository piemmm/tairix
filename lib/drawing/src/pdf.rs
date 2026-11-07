//! Writing a drawing as PDF.
//!
//! One page the size of the drawing's, painted by one content stream: every
//! path kept as curves; colours with their alpha through graphics states;
//! gradients as shadings, a stop's alpha through a soft mask, a reflected or
//! repeated spread stitched over the range its shape needs; an opacity that
//! changes the picture as a transparency group; a clip of several shapes as
//! their exact union; text as its glyphs' outlines; arrowheads after their
//! stroke, as a viewer draws an SVG marker. Every stream is deflated.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use tairix_bezier::boolean::{boolean, BooleanError, Operation};
use tairix_bezier::offset::{outline, StrokeStyle};
use tairix_bezier::{Affine, Bounds, FillRule, Path, PathEl};
use tairix_colour::Rgba;
use tairix_compress::deflate::Flush;
use tairix_compress::zlib::Encoder;
use tairix_util::fallible;
use tairix_util::mathf::{ceil, floor, fmax, fmin, round_i32};

use crate::drawing::Drawing;
use crate::node::{Kind, Node};
use crate::render::{arrowheads, dashes};
use crate::style::{Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Stroke, Style};
use crate::text::Glyphs;
use crate::DrawingError;

/// Points per pixel: PDF's unit is the point, the drawing's the CSS pixel.
const POINTS_PER_PIXEL: f64 = 0.75;

/// The most periods a reflected or repeated gradient is stitched over; a
/// shape needing more is painted with its ramp's ends carried on beyond.
const MAX_PERIODS: f64 = 256.0;

/// Object numbers fixed before anything is written.
const CATALOG: usize = 1;
const PAGES: usize = 2;
const PAGE: usize = 3;
const CONTENT: usize = 4;
const RESOURCES: usize = 5;
const INFO: usize = 6;
const FIRST_FREE: usize = 7;

/// A drawing as PDF.
#[derive(Clone, Debug)]
pub struct Pdf {
    /// The file.
    pub bytes: Vec<u8>,
    /// Whether every glyph its text needs was known.
    pub complete: bool,
}

/// `drawing` as a PDF file, its text set from `glyphs`.
///
/// # Errors
///
/// [`DrawingError::TooComplex`] for geometry past what a clip's union or a
/// stroke's outline may hold, and [`DrawingError::OutOfMemory`] when the
/// file cannot be held.
pub fn pdf(drawing: &Drawing, glyphs: &dyn Glyphs) -> Result<Pdf, DrawingError> {
    let mut writer = PdfWriter {
        glyphs,
        objects: Vec::new(),
        states: Vec::new(),
        masks: Vec::new(),
        shadings: Vec::new(),
        forms: Vec::new(),
        complete: true,
        encoder: Box::new(Encoder::new()),
    };
    let page = drawing.page();
    let size = page.size();
    let mut content = String::new();
    written(writeln!(
        content,
        "{} 0 0 {} 0 {} cm",
        Real(POINTS_PER_PIXEL),
        Real(-POINTS_PER_PIXEL),
        Real(size.y * POINTS_PER_PIXEL)
    ))?;
    if let Some(background) = page.background {
        writer.colour_fill(&mut content, background, 1.0)?;
        written(writeln!(
            content,
            "0 0 {} {} re f",
            Real(size.x),
            Real(size.y)
        ))?;
    }
    for layer in drawing.layers().iter() {
        writer.node(&mut content, layer)?;
    }
    let content = writer.stream("", content.as_bytes())?;
    let mut info = String::from("<< /Producer (TAIRiX Draw)");
    if let Some(title) = &drawing.info().title {
        written(write!(info, " /Title {}", TextString(title)))?;
    }
    if let Some(description) = &drawing.info().description {
        written(write!(info, " /Subject {}", TextString(description)))?;
    }
    info.push_str(" >>");
    let fixed = [
        (CATALOG, alloc::format!("<< /Type /Catalog /Pages {PAGES} 0 R >>").into_bytes()),
        (PAGES, alloc::format!("<< /Type /Pages /Kids [{PAGE} 0 R] /Count 1 >>").into_bytes()),
        (
            PAGE,
            alloc::format!(
                "<< /Type /Page /Parent {PAGES} 0 R /MediaBox [0 0 {} {}] /Contents {CONTENT} 0 R /Resources {RESOURCES} 0 R /Group << /S /Transparency /CS /DeviceRGB >> >>",
                Real(size.x * POINTS_PER_PIXEL),
                Real(size.y * POINTS_PER_PIXEL)
            )
            .into_bytes(),
        ),
        (CONTENT, content),
        (RESOURCES, writer.resources()?.into_bytes()),
        (INFO, info.into_bytes()),
    ];
    let bytes = writer.file(&fixed)?;
    Ok(Pdf {
        bytes,
        complete: writer.complete,
    })
}

fn written(result: fmt::Result) -> Result<(), DrawingError> {
    result.map_err(|_| DrawingError::OutOfMemory)
}

fn of_boolean(error: BooleanError) -> DrawingError {
    match error {
        BooleanError::TooComplex => DrawingError::TooComplex,
        BooleanError::OutOfMemory => DrawingError::OutOfMemory,
    }
}

/// A number as PDF spells one: decimals only, never an exponent, to four
/// places.
struct Real(f64);

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

/// A string as a PDF text string: UTF-16BE behind its byte order mark, in
/// hexadecimal, which no character can break out of.
struct TextString<'a>(&'a str);

impl fmt::Display for TextString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<FEFF")?;
        for unit in self.0.encode_utf16() {
            write!(f, "{unit:04X}")?;
        }
        f.write_char('>')
    }
}

/// An affine map as a `cm` operand list.
struct Matrix(Affine);

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

struct PdfWriter<'g> {
    glyphs: &'g dyn Glyphs,
    /// Objects numbered from [`FIRST_FREE`], each its whole body.
    objects: Vec<Vec<u8>>,
    /// The fill and stroke alphas of each plain graphics state, named `/G`.
    states: Vec<(u8, u8)>,
    /// The soft mask form of each masking graphics state, named `/M`.
    masks: Vec<usize>,
    /// Each shading's dictionary, named `/S`.
    shadings: Vec<String>,
    /// Each form's object number, named `/F`.
    forms: Vec<usize>,
    complete: bool,
    encoder: Box<Encoder>,
}

impl PdfWriter<'_> {
    /// `content` as a deflated stream object of the extra entries `entries`.
    fn stream(&mut self, entries: &str, content: &[u8]) -> Result<Vec<u8>, DrawingError> {
        self.encoder.reset();
        let mut packed =
            fallible::zeroed(self.encoder.bound(content.len())).ok_or(DrawingError::OutOfMemory)?;
        let length = self
            .encoder
            .compress(content, &mut packed, Flush::Finish)
            .map_err(|_| DrawingError::OutOfMemory)?;
        packed.truncate(length);
        let head =
            alloc::format!("<< {entries} /Length {length} /Filter /FlateDecode >>\nstream\n");
        let mut object = Vec::new();
        if !fallible::reserve(&mut object, head.len() + length + 11) {
            return Err(DrawingError::OutOfMemory);
        }
        object.extend_from_slice(head.as_bytes());
        object.extend_from_slice(&packed);
        object.extend_from_slice(b"\nendstream");
        Ok(object)
    }

    /// Add `body` as the next free object, answering its number.
    fn object(&mut self, body: Vec<u8>) -> Result<usize, DrawingError> {
        if !fallible::push(&mut self.objects, body) {
            return Err(DrawingError::OutOfMemory);
        }
        Ok(FIRST_FREE + self.objects.len() - 1)
    }

    /// A transparency group form of `content`, bounded by `bounds` in the
    /// space it is painted in.
    fn form(&mut self, content: &str, bounds: Bounds, grey: bool) -> Result<usize, DrawingError> {
        let space = if grey { "/DeviceGray" } else { "/DeviceRGB" };
        let entries = alloc::format!(
            "/Type /XObject /Subtype /Form /BBox [{} {} {} {}] /Group << /S /Transparency /CS {space} >> /Resources {RESOURCES} 0 R",
            Real(bounds.min.x),
            Real(bounds.min.y),
            Real(bounds.max.x),
            Real(bounds.max.y)
        );
        let body = self.stream(&entries, content.as_bytes())?;
        self.object(body)
    }

    /// The graphics state painting fills at `fill` and strokes at `stroke`
    /// alpha.
    fn state(&mut self, fill: u8, stroke: u8) -> Result<usize, DrawingError> {
        if let Some(at) = self
            .states
            .iter()
            .position(|&known| known == (fill, stroke))
        {
            return Ok(at);
        }
        if !fallible::push(&mut self.states, (fill, stroke)) {
            return Err(DrawingError::OutOfMemory);
        }
        Ok(self.states.len() - 1)
    }

    fn shading(&mut self, dictionary: String) -> Result<usize, DrawingError> {
        if !fallible::push(&mut self.shadings, dictionary) {
            return Err(DrawingError::OutOfMemory);
        }
        Ok(self.shadings.len() - 1)
    }

    fn resources(&self) -> Result<String, DrawingError> {
        let mut out = String::from("<< /ExtGState <<");
        for (index, (fill, stroke)) in self.states.iter().enumerate() {
            written(write!(
                out,
                " /G{index} << /ca {} /CA {} >>",
                Real(f64::from(*fill) / 255.0),
                Real(f64::from(*stroke) / 255.0)
            ))?;
        }
        for (index, form) in self.masks.iter().enumerate() {
            written(write!(
                out,
                " /M{index} << /SMask << /Type /Mask /S /Luminosity /G {form} 0 R >> >>"
            ))?;
        }
        out.push_str(" >> /Shading <<");
        for (index, shading) in self.shadings.iter().enumerate() {
            written(write!(out, " /S{index} {shading}"))?;
        }
        out.push_str(" >> /XObject <<");
        for (index, form) in self.forms.iter().enumerate() {
            written(write!(out, " /F{index} {form} 0 R"))?;
        }
        out.push_str(" >> >>");
        Ok(out)
    }

    /// The file: its header, every object, the cross-reference table and
    /// the trailer.
    fn file(&self, fixed: &[(usize, Vec<u8>); 6]) -> Result<Vec<u8>, DrawingError> {
        let count = FIRST_FREE + self.objects.len();
        let total: usize = fixed.iter().map(|(_, body)| body.len() + 32).sum::<usize>()
            + self
                .objects
                .iter()
                .map(|body| body.len() + 32)
                .sum::<usize>()
            + count * 20
            + 256;
        let mut out = Vec::new();
        if !fallible::reserve(&mut out, total) {
            return Err(DrawingError::OutOfMemory);
        }
        out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets = Vec::new();
        if !fallible::reserve(&mut offsets, count) {
            return Err(DrawingError::OutOfMemory);
        }
        offsets.resize(count, 0);
        let mut emit = |out: &mut Vec<u8>, number: usize, body: &[u8]| {
            offsets[number] = out.len();
            out.extend_from_slice(alloc::format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };
        for (number, body) in fixed {
            emit(&mut out, *number, body);
        }
        for (index, body) in self.objects.iter().enumerate() {
            emit(&mut out, FIRST_FREE + index, body);
        }
        let table = out.len();
        out.extend_from_slice(alloc::format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for offset in &offsets[1..] {
            out.extend_from_slice(alloc::format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            alloc::format!(
                "trailer\n<< /Size {count} /Root {CATALOG} 0 R /Info {INFO} 0 R >>\nstartxref\n{table}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Ok(out)
    }

    fn node(&mut self, out: &mut String, node: &Node) -> Result<(), DrawingError> {
        if !node.visible() {
            return Ok(());
        }
        let opacity = node.opacity();
        if opacity <= 0.0 {
            return Ok(());
        }
        if opacity < 1.0 && isolates(node) {
            let mut inner = String::new();
            self.content(&mut inner, node, 1.0)?;
            let form = self.form(&inner, node.bounds(), false)?;
            if !fallible::push(&mut self.forms, form) {
                return Err(DrawingError::OutOfMemory);
            }
            let alpha = tairix_svg::number::opacity_to_alpha(opacity);
            let state = self.state(alpha, alpha)?;
            return written(writeln!(
                out,
                "q /G{state} gs /F{} Do Q",
                self.forms.len() - 1
            ));
        }
        self.content(out, node, opacity)
    }

    /// `node` in its own space, its opacity `opacity` folded into its one
    /// paint.
    fn content(&mut self, out: &mut String, node: &Node, opacity: f64) -> Result<(), DrawingError> {
        out.push_str("q\n");
        if node.transform() != Affine::IDENTITY {
            written(writeln!(out, "{} cm", Matrix(node.transform())))?;
        }
        match node.kind() {
            Kind::Group { children, clip } => {
                if let Some(clip) = clip {
                    match self.clip_path(clip)? {
                        Some(path) => {
                            path_ops(out, &path)?;
                            out.push_str("W n\n");
                        }
                        None => out.push_str("0 0 0 0 re W n\n"),
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
            Kind::Text { .. } => {
                let path = self.text_path(node)?;
                if let Some(style) = node.style() {
                    self.painted(out, &path, style, opacity)?;
                }
            }
        }
        out.push_str("Q\n");
        Ok(())
    }

    /// The outlines a text node's glyphs set, in its own space.
    fn text_path(&mut self, node: &Node) -> Result<Path, DrawingError> {
        let Kind::Text { text, .. } = node.kind() else {
            return Ok(Path::new());
        };
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
        Ok(path)
    }

    /// The area a clip node covers in its parent's space, as one path filled
    /// non-zero: several shapes are their exact union. `None` covers
    /// nothing.
    fn clip_path(&mut self, node: &Node) -> Result<Option<Path>, DrawingError> {
        if !node.visible() {
            return Ok(None);
        }
        let covered = match node.kind() {
            Kind::Shape { shape, style } => {
                let path = shape.to_path()?;
                boolean(
                    &path,
                    style.fill_rule,
                    &Path::new(),
                    FillRule::NonZero,
                    Operation::Union,
                )
                .map_err(of_boolean)?
            }
            Kind::Text { .. } => {
                let path = self.text_path(node)?;
                boolean(
                    &path,
                    FillRule::NonZero,
                    &Path::new(),
                    FillRule::NonZero,
                    Operation::Union,
                )
                .map_err(of_boolean)?
            }
            Kind::Group { children, clip } => {
                let mut union = Path::new();
                for child in children.iter() {
                    if let Some(part) = self.clip_path(child)? {
                        union = boolean(
                            &union,
                            FillRule::NonZero,
                            &part,
                            FillRule::NonZero,
                            Operation::Union,
                        )
                        .map_err(of_boolean)?;
                    }
                }
                if let Some(clip) = clip {
                    let Some(window) = self.clip_path(clip)? else {
                        return Ok(None);
                    };
                    union = boolean(
                        &union,
                        FillRule::NonZero,
                        &window,
                        FillRule::NonZero,
                        Operation::Intersection,
                    )
                    .map_err(of_boolean)?;
                }
                union
            }
        };
        let placed = covered.transformed(node.transform());
        Ok((!placed.is_empty()).then_some(placed))
    }

    /// `path` painted as `style` says, weakened by `opacity`.
    fn painted(
        &mut self,
        out: &mut String,
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
        for head in arrowheads(path, &style.stroke) {
            let head = Path {
                subpaths: alloc::vec![head],
            };
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
        out: &mut String,
        path: &Path,
        paint: &Paint,
        rule: FillRule,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        let operator = if rule == FillRule::EvenOdd { "f*" } else { "f" };
        match paint {
            Paint::None => Ok(()),
            Paint::Colour(colour) if colour.a == 0 => Ok(()),
            Paint::Colour(colour) => {
                self.colour_fill(out, *colour, opacity)?;
                path_ops(out, path)?;
                written(writeln!(out, "{operator}"))
            }
            Paint::Gradient(gradient) => {
                out.push_str("q\n");
                path_ops(out, path)?;
                out.push_str(if rule == FillRule::EvenOdd {
                    "W* n\n"
                } else {
                    "W n\n"
                });
                self.gradient(out, gradient, opacity, path.bounds())?;
                out.push_str("Q\n");
                Ok(())
            }
        }
    }

    fn stroke(
        &mut self,
        out: &mut String,
        path: &Path,
        stroke: &Stroke,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        if !stroke.paints() {
            return Ok(());
        }
        if let Paint::Gradient(_) = stroke.paint {
            // PDF strokes with a colour alone: a gradient fills the stroke's
            // outline instead.
            let dashed = dashes(path, stroke)?;
            let pen = StrokeStyle {
                width: stroke.width,
                join: stroke.join,
                miter_limit: stroke.miter_limit,
                start_cap: stroke.cap,
                end_cap: stroke.cap,
            };
            let area = outline(&dashed, &pen, 0.01).map_err(of_boolean)?;
            return self.fill(out, &area, &stroke.paint, FillRule::NonZero, opacity);
        }
        let Paint::Colour(colour) = stroke.paint else {
            return Ok(());
        };
        let alpha = tairix_svg::number::opacity_to_alpha(f64::from(colour.a) / 255.0 * opacity);
        let state = self.state(255, alpha)?;
        written(write!(
            out,
            "/G{state} gs {} {} {} RG {} w {} J {} j {} M",
            Real(f64::from(colour.r) / 255.0),
            Real(f64::from(colour.g) / 255.0),
            Real(f64::from(colour.b) / 255.0),
            Real(stroke.width),
            match stroke.cap {
                Cap::Butt => 0,
                Cap::Round => 1,
                Cap::Square => 2,
            },
            match stroke.join {
                Join::Miter => 0,
                Join::Round => 1,
                Join::Bevel => 2,
            },
            Real(stroke.miter_limit)
        ))?;
        if !stroke.dashes.is_empty() {
            out.push_str(" [");
            for dash in stroke.dashes.iter() {
                written(write!(out, " {}", Real(*dash)))?;
            }
            written(write!(out, " ] {} d", Real(stroke.dash_offset)))?;
        }
        out.push('\n');
        path_ops(out, path)?;
        out.push_str("S\n");
        Ok(())
    }

    fn colour_fill(
        &mut self,
        out: &mut String,
        colour: Rgba,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        let alpha = tairix_svg::number::opacity_to_alpha(f64::from(colour.a) / 255.0 * opacity);
        let state = self.state(alpha, 255)?;
        written(writeln!(
            out,
            "/G{state} gs {} {} {} rg",
            Real(f64::from(colour.r) / 255.0),
            Real(f64::from(colour.g) / 255.0),
            Real(f64::from(colour.b) / 255.0)
        ))
    }

    /// Paint the current clip with `gradient` weakened by `opacity`, `area`
    /// bounding what it fills in the object's space.
    fn gradient(
        &mut self,
        out: &mut String,
        gradient: &Gradient,
        opacity: f64,
        area: Bounds,
    ) -> Result<(), DrawingError> {
        let last = gradient
            .stops
            .last()
            .map_or(Rgba::new(0, 0, 0, 0), |stop| stop.colour);
        let Some(back) = gradient.transform.invert() else {
            // A gradient its map collapses paints its last colour, as SVG
            // paints it.
            self.colour_fill(out, last, opacity)?;
            written(writeln!(
                out,
                "{} {} {} {} re f",
                Real(area.min.x),
                Real(area.min.y),
                Real(area.width()),
                Real(area.height())
            ))?;
            return Ok(());
        };
        let reach = area.transformed(back);
        let Some(geometry) = Geometry::of(gradient, reach) else {
            self.colour_fill(out, last, opacity)?;
            written(writeln!(
                out,
                "{} {} {} {} re f",
                Real(area.min.x),
                Real(area.min.y),
                Real(area.width()),
                Real(area.height())
            ))?;
            return Ok(());
        };
        written(writeln!(out, "{} cm", Matrix(gradient.transform)))?;
        let colours = geometry.shading(gradient, Channel::Colour)?;
        let colours = self.shading(colours)?;
        let translucent =
            opacity < 1.0 || gradient.stops.iter().any(|stop| stop.colour.a < u8::MAX);
        if translucent {
            let alphas = geometry.shading(gradient, Channel::Alpha(opacity))?;
            let alphas = self.shading(alphas)?;
            let form = self.form(&alloc::format!("/S{alphas} sh\n"), reach, true)?;
            if !fallible::push(&mut self.masks, form) {
                return Err(DrawingError::OutOfMemory);
            }
            written(write!(out, "/M{} gs ", self.masks.len() - 1))?;
        }
        written(writeln!(out, "/S{colours} sh"))
    }
}

/// Whether `node`'s opacity, when it has one, must composite it as a whole:
/// anything that paints more than once over itself.
fn isolates(node: &Node) -> bool {
    match node.kind() {
        Kind::Group { .. } => true,
        Kind::Shape { style, .. } | Kind::Text { style, .. } => {
            let heads = [style.stroke.start, style.stroke.end]
                .iter()
                .any(|head| style.stroke.draws_head(head));
            (style.fill.paints() && style.stroke.paints()) || heads
        }
    }
}

/// `path` as PDF path construction operators.
fn path_ops(out: &mut String, path: &Path) -> Result<(), DrawingError> {
    for subpath in &path.subpaths {
        written(write!(
            out,
            "{} {} m",
            Real(subpath.start.x),
            Real(subpath.start.y)
        ))?;
        for element in &subpath.elements {
            match *element {
                PathEl::Line(to) => written(write!(out, " {} {} l", Real(to.x), Real(to.y)))?,
                PathEl::Cubic(c1, c2, to) => written(write!(
                    out,
                    " {} {} {} {} {} {} c",
                    Real(c1.x),
                    Real(c1.y),
                    Real(c2.x),
                    Real(c2.y),
                    Real(to.x),
                    Real(to.y)
                ))?,
            }
        }
        if subpath.closed {
            out.push_str(" h");
        }
        out.push('\n');
    }
    Ok(())
}

/// Which part of a gradient's stops a shading carries.
#[derive(Copy, Clone)]
enum Channel {
    Colour,
    /// Their alpha, weakened by the opacity, as grey.
    Alpha(f64),
}

/// Where a shading's ramp runs, in the gradient's own space: its
/// coordinates and the parameter range it covers.
struct Geometry {
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
    /// geometry.
    fn shading(&self, gradient: &Gradient, channel: Channel) -> Result<String, DrawingError> {
        let space = match channel {
            Channel::Colour => "/DeviceRGB",
            Channel::Alpha(_) => "/DeviceGray",
        };
        let ramp = ramp(gradient, channel)?;
        let mut out = String::new();
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
            out.push_str(&ramp);
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
                out.push(' ');
                out.push_str(&ramp);
            }
            out.push_str(" ] /Bounds [");
            for period in 1..periods {
                written(write!(out, " {}", first + period))?;
            }
            out.push_str(" ] /Encode [");
            for period in 0..periods {
                let backwards =
                    gradient.spread == Spread::Reflect && (first + period).rem_euclid(2) == 1;
                out.push_str(if backwards { " 1 0" } else { " 0 1" });
            }
            out.push_str(" ] >>");
        }
        out.push_str(" >>");
        Ok(out)
    }
}

/// The function a gradient's stops ramp through over `0..=1`, one linear
/// piece between each two stops that stand apart.
fn ramp(gradient: &Gradient, channel: Channel) -> Result<String, DrawingError> {
    let values = |colour: Rgba| -> String {
        match channel {
            Channel::Colour => alloc::format!(
                "{} {} {}",
                Real(f64::from(colour.r) / 255.0),
                Real(f64::from(colour.g) / 255.0),
                Real(f64::from(colour.b) / 255.0)
            ),
            Channel::Alpha(opacity) => {
                alloc::format!("{}", Real(f64::from(colour.a) / 255.0 * opacity))
            }
        }
    };
    let stops = &gradient.stops;
    let piece = |from: Rgba, to: Rgba| {
        alloc::format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{}] /C1 [{}] /N 1 >>",
            values(from),
            values(to)
        )
    };
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return Ok(piece(Rgba::new(0, 0, 0, 0), Rgba::new(0, 0, 0, 0)));
    };
    // Every boundary where the ramp changes piece, with the colours either
    // side of it; the ends carry the end colours to 0 and 1.
    let mut points: Vec<(f64, Rgba)> = Vec::new();
    if !fallible::reserve(&mut points, stops.len() + 2) {
        return Err(DrawingError::OutOfMemory);
    }
    points.push((0.0, first.colour));
    for stop in stops {
        points.push((stop.offset, stop.colour));
    }
    points.push((1.0, last.colour));
    let mut pieces = String::new();
    let mut bounds = String::new();
    let mut count = 0;
    for pair in points.windows(2) {
        let [(from_at, from), (to_at, to)] = [pair[0], pair[1]];
        if to_at <= from_at {
            continue;
        }
        if count > 0 {
            written(write!(bounds, " {}", Real(from_at)))?;
        }
        pieces.push(' ');
        pieces.push_str(&piece(from, to));
        count += 1;
    }
    if count <= 1 {
        return Ok(if count == 1 {
            String::from(pieces.trim_start())
        } else {
            piece(last.colour, last.colour)
        });
    }
    let mut encode = String::new();
    for _ in 0..count {
        encode.push_str(" 0 1");
    }
    Ok(alloc::format!(
        "<< /FunctionType 3 /Domain [0 1] /Functions [{pieces} ] /Bounds [{bounds} ] /Encode [{encode} ] >>"
    ))
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod tests;

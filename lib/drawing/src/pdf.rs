//! Writing a drawing as PDF.
//!
//! One page the size of the drawing's, painted by one content stream through
//! the walk every vector format shares (`vector`): colours with their alpha
//! through graphics states; gradients as shadings, a stop's alpha through a
//! soft mask; an opacity that changes the picture as a transparency group.
//! Every stream is deflated.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use tairix_bezier::{Affine, Bounds};
use tairix_collections::{try_box, OrderedMap};
use tairix_colour::Rgba;
use tairix_compress::zlib::Encoder;
use tairix_util::fallible::{self, Text};

use crate::drawing::Drawing;
use crate::image::{survey, Image};
use crate::style::Gradient;
use crate::text::Glyphs;
use crate::vector::{
    written, Channel, Deflating, Dialect, Geometry, Matrix, Raster, Real, Samples, Sink, Walk,
    POINTS_PER_PIXEL,
};
use crate::DrawingError;

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
    let sink = PdfSink {
        objects: Vec::new(),
        states: Vec::new(),
        masks: Vec::new(),
        shadings: Vec::new(),
        forms: Vec::new(),
        images: OrderedMap::new(),
        pictures: Vec::new(),
        encoder: try_box(Encoder::new())?,
    };
    let mut walk = Walk::new(sink, glyphs);
    let page = drawing.page();
    let size = page.size();
    let mut content = Text::new();
    written(writeln!(
        content,
        "{} 0 0 {} 0 {} cm",
        Real(POINTS_PER_PIXEL),
        Real(-POINTS_PER_PIXEL),
        Real(size.y * POINTS_PER_PIXEL)
    ))?;
    walk.ground(&mut content, page)?;
    for layer in drawing.layers().iter() {
        walk.node(&mut content, layer)?;
    }
    let complete = walk.complete;
    let mut writer = walk.sink;
    let content = writer.stream("", content.as_str().as_bytes())?;
    let mut info = Text::new();
    written(info.write_str("<< /Producer (TAIRiX Draw)"))?;
    if let Some(title) = &drawing.info().title {
        written(write!(info, " /Title {}", TextString(title)))?;
    }
    if let Some(description) = &drawing.info().description {
        written(write!(info, " /Subject {}", TextString(description)))?;
    }
    written(info.write_str(" >>"))?;
    let fixed = [
        (
            CATALOG,
            bytes_of(format_args!("<< /Type /Catalog /Pages {PAGES} 0 R >>"))?,
        ),
        (
            PAGES,
            bytes_of(format_args!(
                "<< /Type /Pages /Kids [{PAGE} 0 R] /Count 1 >>"
            ))?,
        ),
        (
            PAGE,
            bytes_of(format_args!(
                "<< /Type /Page /Parent {PAGES} 0 R /MediaBox [0 0 {} {}] /Contents {CONTENT} 0 R /Resources {RESOURCES} 0 R /Group << /S /Transparency /CS /DeviceRGB >> >>",
                Real(size.x * POINTS_PER_PIXEL),
                Real(size.y * POINTS_PER_PIXEL)
            ))?,
        ),
        (CONTENT, content),
        (RESOURCES, writer.resources()?.into_string().into_bytes()),
        (INFO, info.into_string().into_bytes()),
    ];
    let bytes = writer.file(&fixed)?;
    Ok(Pdf { bytes, complete })
}

/// `arguments` written, as an object's bytes.
fn bytes_of(arguments: fmt::Arguments<'_>) -> Result<Vec<u8>, DrawingError> {
    let mut text = Text::new();
    written(text.write_fmt(arguments))?;
    Ok(text.into_string().into_bytes())
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

/// A byte file written to as text is, the room for each piece taken
/// fallibly.
struct Bytes<'v>(&'v mut Vec<u8>);

impl fmt::Write for Bytes<'_> {
    fn write_str(&mut self, piece: &str) -> fmt::Result {
        self.0.try_reserve(piece.len()).map_err(|_| fmt::Error)?;
        self.0.extend_from_slice(piece.as_bytes());
        Ok(())
    }
}

/// What PDF paints with: the file's objects, and the graphics states,
/// shadings and forms its content names.
struct PdfSink {
    /// Objects numbered from [`FIRST_FREE`], each its whole body.
    objects: Vec<Vec<u8>>,
    /// The fill and stroke alphas of each plain graphics state, named `/G`.
    states: Vec<(u8, u8)>,
    /// The soft mask form of each masking graphics state, named `/M`.
    masks: Vec<usize>,
    /// Each shading's dictionary, named `/S`.
    shadings: Vec<Text>,
    /// Each form's object number, named `/F`.
    forms: Vec<usize>,
    /// Each picture's place among `pictures`, by its pixels' address, so a
    /// picture shown many times is written once.
    images: OrderedMap<usize, usize>,
    /// Each picture's image object number, named `/I`.
    pictures: Vec<usize>,
    encoder: Box<Encoder>,
}

impl PdfSink {
    /// `content` as a deflated stream object of the extra entries `entries`.
    fn stream(&mut self, entries: &str, content: &[u8]) -> Result<Vec<u8>, DrawingError> {
        let mut deflating = Deflating::new(&mut self.encoder);
        deflating.push(content)?;
        let packed = deflating.finish()?;
        let mut framing = Text::new();
        written(write!(framing, "{entries} /Filter /FlateDecode"))?;
        Self::framed(framing.as_str(), &packed)
    }

    /// `content`, as it is, as a stream object of the entries `entries`.
    fn framed(entries: &str, content: &[u8]) -> Result<Vec<u8>, DrawingError> {
        let mut object = Vec::new();
        written(write!(
            Bytes(&mut object),
            "<< {entries} /Length {} >>\nstream\n",
            content.len()
        ))?;
        if !fallible::reserve(&mut object, content.len() + 11) {
            return Err(DrawingError::OutOfMemory);
        }
        object.extend_from_slice(content);
        object.extend_from_slice(b"\nendstream");
        Ok(object)
    }

    /// The image object `raster` is written as, with a soft mask of its
    /// alpha where any of it lets something through: a JPEG passed on as it
    /// came, and other pixels deflated a row at a time.
    fn picture(&mut self, raster: &Raster<'_>) -> Result<usize, DrawingError> {
        let (width, height) = raster.size();
        let entries = |space: &str, filter: &str, mask: Option<usize>| {
            let mut text = Text::new();
            written(write!(
                text,
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /{space} /BitsPerComponent 8 /Interpolate true /Filter /{filter}"
            ))?;
            if let Some(mask) = mask {
                written(write!(text, " /SMask {mask} 0 R"))?;
            }
            Ok::<Text, DrawingError>(text)
        };
        let body = match raster {
            Raster::Jpeg(bytes, frame) => {
                let space = if frame.components == 1 {
                    "DeviceGray"
                } else {
                    "DeviceRGB"
                };
                Self::framed(entries(space, "DCTDecode", None)?.as_str(), bytes)?
            }
            Raster::Pixels(pixels) => {
                let survey = survey(pixels);
                let mask = if survey.opaque {
                    None
                } else {
                    let alpha = Deflating::new(&mut self.encoder).pixels(pixels, Samples::Alpha)?;
                    let framed =
                        Self::framed(entries("DeviceGray", "FlateDecode", None)?.as_str(), &alpha)?;
                    Some(self.object(framed)?)
                };
                let (space, samples) = if survey.grey {
                    ("DeviceGray", Samples::Grey)
                } else {
                    ("DeviceRGB", Samples::Rgb)
                };
                let colour = Deflating::new(&mut self.encoder).pixels(pixels, samples)?;
                Self::framed(entries(space, "FlateDecode", mask)?.as_str(), &colour)?
            }
        };
        self.object(body)
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
        let mut entries = Text::new();
        written(write!(
            entries,
            "/Type /XObject /Subtype /Form /BBox [{} {} {} {}] /Group << /S /Transparency /CS {space} >> /Resources {RESOURCES} 0 R",
            Real(bounds.min.x),
            Real(bounds.min.y),
            Real(bounds.max.x),
            Real(bounds.max.y)
        ))?;
        let body = self.stream(entries.as_str(), content.as_bytes())?;
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

    fn shading(&mut self, dictionary: Text) -> Result<usize, DrawingError> {
        if !fallible::push(&mut self.shadings, dictionary) {
            return Err(DrawingError::OutOfMemory);
        }
        Ok(self.shadings.len() - 1)
    }

    fn resources(&self) -> Result<Text, DrawingError> {
        let mut out = Text::new();
        written(out.write_str("<< /ExtGState <<"))?;
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
        written(out.write_str(" >> /Shading <<"))?;
        for (index, shading) in self.shadings.iter().enumerate() {
            written(write!(out, " /S{index} {}", shading.as_str()))?;
        }
        written(out.write_str(" >> /XObject <<"))?;
        for (index, form) in self.forms.iter().enumerate() {
            written(write!(out, " /F{index} {form} 0 R"))?;
        }
        for (index, picture) in self.pictures.iter().enumerate() {
            written(write!(out, " /I{index} {picture} 0 R"))?;
        }
        written(out.write_str(" >> >>"))?;
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
            written(writeln!(Bytes(out), "{number} 0 obj"))?;
            if !fallible::reserve(out, body.len() + 8) {
                return Err(DrawingError::OutOfMemory);
            }
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
            Ok(())
        };
        for (number, body) in fixed {
            emit(&mut out, *number, body)?;
        }
        for (index, body) in self.objects.iter().enumerate() {
            emit(&mut out, FIRST_FREE + index, body)?;
        }
        let table = out.len();
        written(write!(
            Bytes(&mut out),
            "xref\n0 {count}\n0000000000 65535 f \n"
        ))?;
        for offset in &offsets[1..] {
            written(writeln!(Bytes(&mut out), "{offset:010} 00000 n "))?;
        }
        written(write!(
            Bytes(&mut out),
            "trailer\n<< /Size {count} /Root {CATALOG} 0 R /Info {INFO} 0 R >>\nstartxref\n{table}\n%%EOF\n"
        ))?;
        Ok(out)
    }
}

impl Sink for PdfSink {
    const DIALECT: Dialect = Dialect {
        move_to: "m",
        line_to: "l",
        curve_to: "c",
        close: "h",
        fill: "f",
        even_odd_fill: "f*",
        stroke: "S",
        clip: "W n",
        even_odd_clip: "W* n",
        clip_nothing: "0 0 0 0 re W n",
        save: "q",
        restore: "Q",
        line_width: "w",
        line_cap: "J",
        line_join: "j",
        miter_limit: "M",
        dash: "d",
        concat,
        fill_rect,
    };

    fn colour(
        &mut self,
        out: &mut Text,
        colour: Rgba,
        opacity: f64,
        stroking: bool,
    ) -> Result<(), DrawingError> {
        let alpha = tairix_svg::number::opacity_to_alpha(f64::from(colour.a) / 255.0 * opacity);
        let (state, operator) = if stroking {
            (self.state(255, alpha)?, "RG")
        } else {
            (self.state(alpha, 255)?, "rg")
        };
        written(write!(
            out,
            "/G{state} gs {} {} {} {operator}",
            Real(f64::from(colour.r) / 255.0),
            Real(f64::from(colour.g) / 255.0),
            Real(f64::from(colour.b) / 255.0)
        ))
    }

    fn shade(
        &mut self,
        out: &mut Text,
        gradient: &Gradient,
        geometry: &Geometry,
        (opacity, reach): (f64, Bounds),
    ) -> Result<(), DrawingError> {
        let colours = geometry.shading(gradient, Channel::Colour)?;
        let colours = self.shading(colours)?;
        let translucent =
            opacity < 1.0 || gradient.stops.iter().any(|stop| stop.colour.a < u8::MAX);
        if translucent {
            let alphas = geometry.shading(gradient, Channel::Alpha(opacity))?;
            let alphas = self.shading(alphas)?;
            let mut masked = Text::new();
            written(writeln!(masked, "/S{alphas} sh"))?;
            let form = self.form(masked.as_str(), reach, true)?;
            if !fallible::push(&mut self.masks, form) {
                return Err(DrawingError::OutOfMemory);
            }
            written(write!(out, "/M{} gs ", self.masks.len() - 1))?;
        }
        written(writeln!(out, "/S{colours} sh"))
    }

    fn isolated(
        &mut self,
        out: &mut Text,
        inner: &str,
        bounds: Bounds,
        opacity: f64,
    ) -> Result<(), DrawingError> {
        let form = self.form(inner, bounds, false)?;
        if !fallible::push(&mut self.forms, form) {
            return Err(DrawingError::OutOfMemory);
        }
        let alpha = tairix_svg::number::opacity_to_alpha(opacity);
        let state = self.state(alpha, alpha)?;
        written(writeln!(
            out,
            "q /G{state} gs /F{} Do Q",
            self.forms.len() - 1
        ))
    }

    fn image(&mut self, out: &mut Text, image: &Image, opacity: f64) -> Result<(), DrawingError> {
        let raster = Raster::of(image, true);
        let key = Arc::as_ptr(image.pyramid()).addr();
        let named = if let Some(&named) = self.images.get(&key) {
            named
        } else {
            let object = self.picture(&raster)?;
            if !fallible::push(&mut self.pictures, object) {
                return Err(DrawingError::OutOfMemory);
            }
            let named = self.pictures.len() - 1;
            self.images
                .try_insert(key, named)
                .map_err(|_| DrawingError::OutOfMemory)?;
            named
        };
        // An image is painted over the unit square, its first row at the top.
        let (width, height) = raster.size();
        let square = Affine {
            a: f64::from(width),
            b: 0.0,
            c: 0.0,
            d: -f64::from(height),
            e: 0.0,
            f: f64::from(height),
        };
        let alpha = tairix_svg::number::opacity_to_alpha(opacity);
        let state = self.state(alpha, u8::MAX)?;
        written(writeln!(
            out,
            "q /G{state} gs {} cm /I{named} Do Q",
            Matrix(square.then(raster.to_place(image)))
        ))
    }
}

/// `map` concatenated onto the current one, as PDF writes it.
fn concat(out: &mut Text, map: Affine) -> fmt::Result {
    writeln!(out, "{} cm", Matrix(map))
}

/// `area` filled, as PDF writes it.
fn fill_rect(out: &mut Text, area: Bounds) -> fmt::Result {
    writeln!(
        out,
        "{} {} {} {} re f",
        Real(area.min.x),
        Real(area.min.y),
        Real(area.width()),
        Real(area.height())
    )
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod tests;

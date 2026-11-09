//! Writing a drawing as Encapsulated PostScript.
//!
//! One page the size of the drawing's, in PostScript Level 3 and 7-bit clean,
//! through the walk every vector format shares (`vector`), gradients as
//! `shfill`, pictures as images in ASCII base-85: a baseline JPEG as it came,
//! other pixels deflated, masked where they let anything through. PostScript
//! paints no transparency, so a translucent colour, gradient, opacity or
//! pixel is written opaque — a pixel shown or not by whether it is more than
//! half shown — and the file says it was.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use tairix_bezier::{Affine, Bounds};
use tairix_collections::{try_box, OrderedMap};
use tairix_colour::Rgba;
use tairix_compress::zlib::Encoder;
use tairix_inline::ArrayString;
use tairix_util::fallible::Text;
use tairix_util::mathf::ceil;

use crate::drawing::Drawing;
use crate::image::{survey, Image, Survey};
use crate::style::Gradient;
use crate::text::Glyphs;
use crate::vector::{
    written, Channel, Deflating, Dialect, Geometry, Matrix, Raster, Real, Samples, Sink, Walk,
    POINTS_PER_PIXEL,
};
use crate::DrawingError;

/// The characters of base-85 data a line carries after the space it opens
/// with.
const DATA_LINE: usize = 75;

/// The longest line the document structuring conventions allow.
const MAX_LINE: usize = 255;

/// A drawing as EPS.
#[derive(Clone, Debug)]
pub struct Eps {
    /// The file.
    pub bytes: Vec<u8>,
    /// Whether every glyph its text needs was known.
    pub complete: bool,
    /// Whether translucency the drawing holds was written opaque.
    pub flattened: bool,
}

/// `drawing` as an EPS file, its text set from `glyphs`.
///
/// # Errors
///
/// [`DrawingError::TooComplex`] for geometry past what a clip's union or a
/// stroke's outline may hold, and [`DrawingError::OutOfMemory`] when the
/// file cannot be held.
pub fn eps(drawing: &Drawing, glyphs: &dyn Glyphs) -> Result<Eps, DrawingError> {
    let sink = EpsSink {
        flattened: false,
        encoder: None,
        packed: OrderedMap::new(),
    };
    let mut walk = Walk::new(sink, glyphs);
    let page = drawing.page();
    let size = page.size();
    let (width, height) = (size.x * POINTS_PER_PIXEL, size.y * POINTS_PER_PIXEL);
    let mut content = Text::new();
    written(writeln!(
        content,
        "[{} 0 0 {} 0 {}] concat",
        Real(POINTS_PER_PIXEL),
        Real(-POINTS_PER_PIXEL),
        Real(height)
    ))?;
    walk.ground(&mut content, page)?;
    for layer in drawing.layers().iter() {
        walk.node(&mut content, layer)?;
    }
    let mut out = Text::new();
    written(out.reserve(content.len() + 512))?;
    written(out.write_str("%!PS-Adobe-3.0 EPSF-3.0\n%%Creator: TAIRiX Draw\n"))?;
    if let Some(title) = &drawing.info().title {
        written(out.write_str("%%Title: "))?;
        string_literal(&mut out, title, MAX_LINE - "%%Title: ".len())?;
        written(out.write_char('\n'))?;
    }
    written(write!(
        out,
        "%%BoundingBox: 0 0 {} {}\n%%HiResBoundingBox: 0 0 {} {}\n",
        Real(ceil(width)),
        Real(ceil(height)),
        Real(width),
        Real(height)
    ))?;
    written(out.write_str(
        "%%LanguageLevel: 3\n%%DocumentData: Clean7Bit\n%%Pages: 1\n%%EndComments\n\
         %%BeginProlog\n%%EndProlog\n%%Page: 1 1\ngsave\n",
    ))?;
    within_lines(&mut out, content.as_str())?;
    written(out.write_str("grestore\nshowpage\n%%Trailer\n%%EOF\n"))?;
    Ok(Eps {
        bytes: out.into_string().into_bytes(),
        complete: walk.complete,
        flattened: walk.sink.flattened,
    })
}

/// `text` onto `out` as a PostScript string literal of no more than `room`
/// characters, 7-bit clean: a parenthesis or backslash escaped, and every
/// other byte outside printable ASCII in octal. Characters past the room are
/// left out whole.
fn string_literal(out: &mut Text, text: &str, room: usize) -> Result<(), DrawingError> {
    written(out.write_char('('))?;
    let mut length = 1;
    for character in text.chars() {
        // A character's four bytes at four characters of octal each.
        let mut spelled = ArrayString::<16>::new();
        let mut bytes = [0; 4];
        for &byte in character.encode_utf8(&mut bytes).as_bytes() {
            match byte {
                b'(' | b')' | b'\\' => written(write!(spelled, "\\{}", char::from(byte)))?,
                b' '..=b'~' => written(spelled.write_char(char::from(byte)))?,
                _ => written(write!(spelled, "\\{byte:03o}"))?,
            }
        }
        if length + spelled.len() + 1 > room {
            break;
        }
        written(out.write_str(&spelled))?;
        length += spelled.len();
    }
    written(out.write_char(')'))
}

/// `content` onto `out` with every line within the conventions, one past them
/// broken between its tokens: the walk writes no string or comment a break
/// could land inside, and no token longer than a line.
fn within_lines(out: &mut Text, content: &str) -> Result<(), DrawingError> {
    for line in content.split_inclusive('\n') {
        let (body, ended) = line
            .strip_suffix('\n')
            .map_or((line, false), |body| (body, true));
        if body.len() <= MAX_LINE {
            written(out.write_str(line))?;
            continue;
        }
        wrapped(out, body)?;
        if ended {
            written(out.write_char('\n'))?;
        }
    }
    Ok(())
}

/// `text`, a run of tokens apart by spaces, written onto `out` in lines no
/// longer than the conventions allow, broken where a space stands.
fn wrapped(out: &mut Text, text: &str) -> Result<(), DrawingError> {
    let mut line = 0;
    for token in text.split(' ').filter(|token| !token.is_empty()) {
        if line > 0 && line + 1 + token.len() > MAX_LINE {
            written(out.write_char('\n'))?;
            line = 0;
        } else if line > 0 {
            written(out.write_char(' '))?;
            line += 1;
        }
        written(out.write_str(token))?;
        line += token.len();
    }
    Ok(())
}

/// `bytes` onto `out` in ASCII base-85, ended by `~>`, each line opening
/// with a space so none reads as a comment the conventions give a meaning to.
fn ascii85(out: &mut Text, bytes: &[u8]) -> Result<(), DrawingError> {
    written(out.reserve(bytes.len() / 4 * 5 + bytes.len() / (DATA_LINE * 4 / 5) + 16))?;
    let mut line = DATA_LINE;
    let mut put = |out: &mut Text, chars: &[u8]| {
        if line + chars.len() > DATA_LINE {
            written(out.write_str("\n "))?;
            line = 0;
        }
        line += chars.len();
        // Every base-85 digit is ASCII, so no group fails to spell.
        let spelled = core::str::from_utf8(chars).map_err(|_| DrawingError::Invalid)?;
        written(out.write_str(spelled))
    };
    for group in bytes.chunks(4) {
        let mut word = [0; 4];
        word[..group.len()].copy_from_slice(group);
        let mut value = u32::from_be_bytes(word);
        if value == 0 && group.len() == 4 {
            put(out, b"z")?;
            continue;
        }
        let mut digits = [0; 5];
        for digit in digits.iter_mut().rev() {
            *digit = b'!' + u8::try_from(value % 85).unwrap_or(0);
            value /= 85;
        }
        put(out, &digits[..=group.len()])?;
    }
    put(out, b"~>")?;
    written(out.write_char('\n'))
}

/// The dictionary of an image `size` pixels, grey or colour, its samples
/// read from the data that follows through `filter`, its first row at the
/// top of the space it is painted in.
fn image_data(
    out: &mut Text,
    (width, height): (u32, u32),
    grey: bool,
    filter: &str,
) -> fmt::Result {
    let decode = if grey { "[0 1]" } else { "[0 1 0 1 0 1]" };
    write!(
        out,
        "<< /ImageType 1 /Width {width} /Height {height} /BitsPerComponent 8\n\
         /Decode {decode} /ImageMatrix [1 0 0 1 0 0] /Interpolate true\n\
         /DataSource currentfile /ASCII85Decode filter {filter} >>"
    )
}

/// What PostScript paints with: opaque colours, shadings and images, noting
/// where the drawing asked for translucency it cannot paint.
struct EpsSink {
    flattened: bool,
    /// The encoder pictures are deflated through, made for the first.
    encoder: Option<Box<Encoder>>,
    /// Each picture's pixels as an image writes them, by where they are
    /// held: PostScript draws a picture where it is shown, so one shown twice
    /// is written twice, but deflated once.
    packed: OrderedMap<usize, Packed>,
}

/// A picture's pixels deflated as an image reads them, and what they need.
struct Packed {
    survey: Survey,
    bytes: Vec<u8>,
}

impl Packed {
    /// `pixels` deflated through `encoder`, masked where any lets something
    /// through.
    fn of(encoder: &mut Encoder, pixels: &tairix_raster::Surface) -> Result<Self, DrawingError> {
        let survey = survey(pixels);
        let samples = match (survey.opaque, survey.grey) {
            (true, true) => Samples::Grey,
            (true, false) => Samples::Rgb,
            (false, true) => Samples::MaskedGrey,
            (false, false) => Samples::MaskedRgb,
        };
        let bytes = Deflating::new(encoder).pixels(pixels, samples)?;
        Ok(Self { survey, bytes })
    }
}

impl EpsSink {
    fn encoder(&mut self) -> Result<&mut Encoder, DrawingError> {
        if self.encoder.is_none() {
            self.encoder = Some(try_box(Encoder::new())?);
        }
        self.encoder.as_deref_mut().ok_or(DrawingError::OutOfMemory)
    }
}

impl Sink for EpsSink {
    const DIALECT: Dialect = Dialect {
        move_to: "moveto",
        line_to: "lineto",
        curve_to: "curveto",
        close: "closepath",
        fill: "fill",
        even_odd_fill: "eofill",
        stroke: "stroke",
        clip: "clip newpath",
        even_odd_clip: "eoclip newpath",
        clip_nothing: "0 0 0 0 rectclip",
        save: "gsave",
        restore: "grestore",
        line_width: "setlinewidth",
        line_cap: "setlinecap",
        line_join: "setlinejoin",
        miter_limit: "setmiterlimit",
        dash: "setdash",
        concat,
        fill_rect,
    };

    fn colour(
        &mut self,
        out: &mut Text,
        colour: Rgba,
        opacity: f64,
        _stroking: bool,
    ) -> Result<(), DrawingError> {
        self.flattened |= colour.a < u8::MAX || opacity < 1.0;
        written(write!(
            out,
            "{} {} {} setrgbcolor",
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
        (opacity, _): (f64, Bounds),
    ) -> Result<(), DrawingError> {
        self.flattened |=
            opacity < 1.0 || gradient.stops.iter().any(|stop| stop.colour.a < u8::MAX);
        let shading = geometry.shading(gradient, Channel::Colour)?;
        written(out.write_str(shading.as_str()))?;
        written(out.write_str(" shfill\n"))
    }

    fn isolated(
        &mut self,
        out: &mut Text,
        inner: &str,
        _bounds: Bounds,
        _opacity: f64,
    ) -> Result<(), DrawingError> {
        self.flattened = true;
        written(out.write_str(inner))
    }

    fn image(&mut self, out: &mut Text, image: &Image, opacity: f64) -> Result<(), DrawingError> {
        self.flattened |= opacity < 1.0;
        // Level 3 is not held to read a progressive JPEG.
        let raster = Raster::of(image, false);
        let size = raster.size();
        written(out.write_str("gsave\n"))?;
        written(concat(out, raster.to_place(image)))?;
        match raster {
            Raster::Jpeg(bytes, frame) => {
                let grey = frame.components == 1;
                written(out.write_str(space(grey)))?;
                written(image_data(out, size, grey, "/DCTDecode filter"))?;
                written(out.write_str(" image"))?;
                ascii85(out, bytes)?;
            }
            Raster::Pixels(pixels) => {
                let key = Arc::as_ptr(image.pyramid()).addr();
                if !self.packed.contains_key(&key) {
                    let packed = Packed::of(self.encoder()?, pixels)?;
                    self.packed
                        .try_insert(key, packed)
                        .map_err(|_| DrawingError::OutOfMemory)?;
                }
                let Packed { survey, bytes } =
                    self.packed.get(&key).ok_or(DrawingError::OutOfMemory)?;
                self.flattened |= !survey.binary;
                written(out.write_str(space(survey.grey)))?;
                if survey.opaque {
                    written(image_data(out, size, survey.grey, "/FlateDecode filter"))?;
                } else {
                    let (width, height) = size;
                    written(out.write_str("<< /ImageType 3 /InterleaveType 1 /DataDict "))?;
                    written(image_data(out, size, survey.grey, "/FlateDecode filter"))?;
                    written(write!(
                        out,
                        "\n/MaskDict << /ImageType 1 /Width {width} /Height {height} /BitsPerComponent 8\n\
                         /Decode [0 1] /ImageMatrix [1 0 0 1 0 0] >> >>"
                    ))?;
                }
                written(out.write_str(" image"))?;
                ascii85(out, bytes)?;
            }
        }
        written(out.write_str("grestore\n"))
    }
}

/// Setting the colour space an image's samples are read in.
const fn space(grey: bool) -> &'static str {
    if grey {
        "/DeviceGray setcolorspace\n"
    } else {
        "/DeviceRGB setcolorspace\n"
    }
}

/// `map` concatenated onto the current one, as PostScript writes it.
fn concat(out: &mut Text, map: Affine) -> fmt::Result {
    writeln!(out, "[{}] concat", Matrix(map))
}

/// `area` filled, as PostScript writes it.
fn fill_rect(out: &mut Text, area: Bounds) -> fmt::Result {
    writeln!(
        out,
        "{} {} {} {} rectfill",
        Real(area.min.x),
        Real(area.min.y),
        Real(area.width()),
        Real(area.height())
    )
}

#[cfg(test)]
#[path = "eps_tests.rs"]
mod tests;

//! OpenRaster: a ZIP holding `mimetype`, the layer stack in `stack.xml`,
//! each layer as a PNG, the layers as composed in `mergedimage.png`, and a
//! thumbnail (the OpenRaster specification, baseline 0.0.5).
//!
//! The stack lists its topmost layer first; a document here holds its
//! bottom layer first. Reading follows nested stacks by folding each into
//! its layers — hidden with it, as opaque as both — and states what that
//! cannot keep exactly, along with any blending other than plain
//! compositing, as held beside.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_util::cnum;
use tairix_xml::Element;

use crate::encode::EncodeError;
use crate::picture::{over, Picture, PictureSource};
use crate::zip::{Archive, Writer, ZipError};
use crate::{png, rgba_picture, DecodeError, DecodeLimits, RasterImage, Unkept};

/// What the first entry holds, which names the archive OpenRaster.
const MIMETYPE: &str = "image/openraster";

/// The most layers one document is read with: a fixed defence against a
/// stack of millions, not a capacity.
pub const MOST_LAYERS: usize = 256;

/// The longest `stack.xml` is read at.
const MOST_STACK_BYTES: usize = 1 << 20;

/// The longest a layer's PNG is read at.
const MOST_LAYER_BYTES: usize = 1 << 28;

/// The plain compositing a layer is laid with, by default and as written.
const SRC_OVER: &str = "svg:src-over";

/// One layer of an OpenRaster document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraLayer {
    /// What it is called.
    pub name: String,
    /// Its pixels, as colour.
    pub picture: Picture,
    /// Where its top left lies on the canvas.
    pub at: (i32, i32),
    /// How much of it shows, out of 255.
    pub opacity: u8,
    /// Whether it shows at all.
    pub visible: bool,
}

/// One layer to write as OpenRaster.
#[derive(Copy, Clone)]
pub struct OraLayerSource<'a> {
    /// What it is called.
    pub name: &'a str,
    /// Its pixels, written as a PNG.
    pub picture: &'a dyn PictureSource,
    /// Where its top left lies on the canvas.
    pub at: (i32, i32),
    /// How much of it shows, out of 255.
    pub opacity: u8,
    /// Whether it shows at all.
    pub visible: bool,
}

/// An OpenRaster document: its canvas and its layers, the bottom first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraDocument {
    /// The canvas's width.
    pub width: u32,
    /// The canvas's height.
    pub height: u32,
    /// The layers, the bottom first.
    pub layers: Vec<OraLayer>,
}

/// Whether `bytes` begin as OpenRaster must: a stored `mimetype` entry first,
/// holding the OpenRaster media type.
#[must_use]
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
        && bytes.get(8..10) == Some(&[0, 0])
        && bytes.get(26..28) == Some(&[8, 0])
        && bytes.get(30..38) == Some(b"mimetype")
        && bytes.get(38..54) == Some(MIMETYPE.as_bytes())
}

/// Write `layers`, the bottom first, as an OpenRaster document of a
/// `canvas` that size, with `merged` — the layers as composed — and
/// `thumbnail`, the same no larger than 256 pixels on either side.
///
/// # Errors
///
/// [`EncodeError`] where a picture cannot be encoded, the canvas has a side
/// of zero ([`EncodeError::TooLarge`], as every encoder here answers one),
/// the stack is empty or past [`MOST_LAYERS`], or the archive would pass what
/// a ZIP without its 64-bit extension holds.
pub fn encode_ora(
    canvas: (u32, u32),
    layers: &[OraLayerSource<'_>],
    merged: &dyn PictureSource,
    thumbnail: &dyn PictureSource,
) -> Result<Vec<u8>, EncodeError> {
    if layers.is_empty() || layers.len() > MOST_LAYERS {
        return Err(EncodeError::LayerCount);
    }
    if canvas.0 == 0 || canvas.1 == 0 {
        return Err(EncodeError::TooLarge);
    }
    let mut zip = Writer::new();
    let stored = |zip: &mut Writer, name: &str, data: &[u8]| {
        zip.store(name, data).map_err(|refusal| match refusal {
            ZipError::OutOfMemory => EncodeError::OutOfMemory,
            _ => EncodeError::TooLarge,
        })
    };
    stored(&mut zip, "mimetype", MIMETYPE.as_bytes())?;
    stored(&mut zip, "stack.xml", stack(canvas, layers)?.as_bytes())?;
    for (index, layer) in layers.iter().enumerate() {
        let png = crate::encode_png(layer.picture)?;
        stored(&mut zip, &layer_path(index), &png)?;
    }
    stored(&mut zip, "mergedimage.png", &crate::encode_png(merged)?)?;
    stored(
        &mut zip,
        "Thumbnails/thumbnail.png",
        &crate::encode_png(thumbnail)?,
    )?;
    zip.finish().map_err(|_| EncodeError::TooLarge)
}

/// Where layer `index`'s PNG is stored.
fn layer_path(index: usize) -> String {
    alloc::format!("data/layer{index}.png")
}

/// The stack of `layers` on a `canvas` that size, the topmost first.
fn stack(
    (width, height): (u32, u32),
    layers: &[OraLayerSource<'_>],
) -> Result<String, EncodeError> {
    let mut xml = String::new();
    let mut put = |text: &str| {
        xml.try_reserve(text.len())
            .map_err(|_| EncodeError::OutOfMemory)?;
        xml.push_str(text);
        Ok::<(), EncodeError>(())
    };
    put("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    put(&alloc::format!(
        "<image version=\"0.0.5\" w=\"{width}\" h=\"{height}\">\n<stack>\n"
    ))?;
    for (index, layer) in layers.iter().enumerate().rev() {
        let thousandths = (u32::from(layer.opacity) * 1000 + 127) / 255;
        let mut line = String::new();
        let _ = write!(
            line,
            "<layer name=\"{}\" src=\"{}\" x=\"{}\" y=\"{}\" opacity=\"{}.{:03}\" visibility=\"{}\" composite-op=\"{SRC_OVER}\"/>",
            Escaped(layer.name),
            layer_path(index),
            layer.at.0,
            layer.at.1,
            thousandths / 1000,
            thousandths % 1000,
            if layer.visible { "visible" } else { "hidden" },
        );
        put(&line)?;
        put("\n")?;
    }
    put("</stack>\n</image>\n")?;
    Ok(xml)
}

/// Text escaped for an XML attribute value.
struct Escaped<'a>(&'a str);

impl core::fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for ch in self.0.chars() {
            match ch {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                '\'' => f.write_str("&apos;")?,
                ch => f.write_char(ch)?,
            }
        }
        Ok(())
    }
}

impl From<ZipError> for DecodeError {
    fn from(refusal: ZipError) -> Self {
        match refusal {
            ZipError::Malformed => Self::OraBadArchive,
            ZipError::Unsupported => Self::OraUnsupportedArchive,
            ZipError::TooLarge => Self::PixelCountExceedsLimit,
            ZipError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

/// A stack as read: its canvas, its layers topmost first, and what folding
/// it left out.
type Stack = ((u32, u32), Vec<Placed>, Unkept);

/// A layer as the stack places it, before its pixels are read.
struct Placed {
    name: String,
    src: String,
    at: (i32, i32),
    opacity: u8,
    visible: bool,
}

/// The canvas `root`, a stack's `image` element, declares.
fn canvas(root: &Element<'_>) -> Result<(u32, u32), DecodeError> {
    if root.name != "image" {
        return Err(DecodeError::OraBadStack);
    }
    let side = |name: &str| {
        root.attr(name)
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|&side| side > 0)
            .ok_or(DecodeError::OraBadStack)
    };
    Ok((side("w")?, side("h")?))
}

/// The canvas OpenRaster `bytes` declare, reading no layer.
pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), DecodeError> {
    canvas_of(&open(bytes)?)
}

/// The canvas `archive`'s stack declares.
fn canvas_of(archive: &Archive<'_>) -> Result<(u32, u32), DecodeError> {
    let xml = stack_xml(archive)?;
    canvas(&tairix_xml::parse(&xml, "").map_err(|_| DecodeError::OraBadStack)?)
}

/// Read the stack in `xml`: its canvas and layers, topmost first, with
/// what folding it left out.
fn read_stack(xml: &str, limits: &DecodeLimits) -> Result<Stack, DecodeError> {
    let root = tairix_xml::parse(xml, "").map_err(|_| DecodeError::OraBadStack)?;
    let (width, height) = canvas(&root)?;
    if width > limits.max_width() {
        return Err(DecodeError::WidthExceedsLimit);
    }
    if height > limits.max_height() {
        return Err(DecodeError::HeightExceedsLimit);
    }
    if u64::from(width) * u64::from(height) > limits.max_pixels() {
        return Err(DecodeError::PixelCountExceedsLimit);
    }
    let top = root
        .children()
        .find(|child| child.name == "stack")
        .ok_or(DecodeError::OraBadStack)?;
    let mut placed = Vec::new();
    let mut unkept = Unkept::default();
    fold(top, (u8::MAX, true), &mut placed, &mut unkept)?;
    Ok(((width, height), placed, unkept))
}

/// Fold `stack`'s layers, topmost first, into `placed`, each as hidden and
/// as faint as the stacks holding it make it.
fn fold(
    stack: &Element<'_>,
    (opacity, visible): (u8, bool),
    placed: &mut Vec<Placed>,
    unkept: &mut Unkept,
) -> Result<(), DecodeError> {
    for child in stack.children() {
        let own = (attr_opacity(child)?, attr_visible(child));
        let composite = child.attr("composite-op").unwrap_or(SRC_OVER);
        if composite != SRC_OVER {
            unkept.extras = true;
        }
        let laid = (scale(opacity, own.0), visible && own.1);
        match child.name {
            "layer" => {
                if placed.len() >= MOST_LAYERS {
                    return Err(DecodeError::OraTooManyLayers);
                }
                let coordinate = |name: &str| match child.attr(name) {
                    None => Ok(0),
                    Some(value) => value
                        .trim()
                        .parse::<i32>()
                        .map_err(|_| DecodeError::OraBadStack),
                };
                let src = String::from(child.attr("src").ok_or(DecodeError::OraBadStack)?);
                let name = String::from(child.attr("name").unwrap_or(""));
                placed
                    .try_reserve(1)
                    .map_err(|_| DecodeError::OutOfMemory)?;
                placed.push(Placed {
                    name,
                    src,
                    at: (coordinate("x")?, coordinate("y")?),
                    opacity: laid.0,
                    visible: laid.1,
                });
            }
            "stack" => {
                // A group composes its layers before it is laid; folded, each
                // is laid alone, which only agrees where the group lets every
                // layer through unchanged.
                if own.0 != u8::MAX || child.attr("isolation").is_some_and(|value| value != "auto")
                {
                    unkept.extras = true;
                }
                fold(child, laid, placed, unkept)?;
            }
            _ => unkept.extras = true,
        }
    }
    Ok(())
}

/// An element's `opacity`, out of 255: whole where it states none.
fn attr_opacity(element: &Element<'_>) -> Result<u8, DecodeError> {
    let Some(value) = element.attr("opacity") else {
        return Ok(u8::MAX);
    };
    let (number, used) = cnum::scan_double(value.trim()).ok_or(DecodeError::OraBadStack)?;
    if used != value.trim().len() || !number.is_finite() {
        return Err(DecodeError::OraBadStack);
    }
    let clamped = number.clamp(0.0, 1.0) * 255.0;
    u8::try_from(tairix_util::mathf::round_i32(clamped)).map_err(|_| DecodeError::OraBadStack)
}

/// Whether an element shows: unless it says `hidden`.
fn attr_visible(element: &Element<'_>) -> bool {
    element.attr("visibility") != Some("hidden")
}

/// `a` scaled by `b`, both out of 255.
fn scale(a: u8, b: u8) -> u8 {
    u8::try_from((u32::from(a) * u32::from(b) + 127) / 255).unwrap_or(u8::MAX)
}

/// Open `bytes` as an OpenRaster archive, refusing one that does not name
/// itself so.
fn open(bytes: &[u8]) -> Result<Archive<'_>, DecodeError> {
    let archive = Archive::open(bytes)?;
    if archive.names().next() != Some(b"mimetype".as_slice())
        || archive.read("mimetype", MIMETYPE.len())?.as_deref() != Some(MIMETYPE.as_bytes())
    {
        return Err(DecodeError::OraBadMimetype);
    }
    Ok(archive)
}

/// The stack's XML, read from the archive.
fn stack_xml(archive: &Archive<'_>) -> Result<String, DecodeError> {
    let xml = archive
        .read("stack.xml", MOST_STACK_BYTES)?
        .ok_or(DecodeError::OraBadStack)?;
    String::from_utf8(xml).map_err(|_| DecodeError::OraBadStack)
}

/// Read OpenRaster `bytes` as its layers, the bottom first, with what the
/// file held that they do not.
pub(crate) fn decode_native(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Result<(OraDocument, Unkept), DecodeError> {
    let archive = open(bytes)?;
    let xml = stack_xml(&archive)?;
    let ((width, height), placed, unkept) = read_stack(&xml, limits)?;
    let mut layers = Vec::new();
    layers
        .try_reserve_exact(placed.len())
        .map_err(|_| DecodeError::OutOfMemory)?;
    for layer in placed.into_iter().rev() {
        let png = archive
            .read(&layer.src, MOST_LAYER_BYTES)?
            .ok_or(DecodeError::OraMissingLayer)?;
        let picture = rgba_picture(png::decode(&png, limits)?)?;
        layers.push(OraLayer {
            name: layer.name,
            picture,
            at: layer.at,
            opacity: layer.opacity,
            visible: layer.visible,
        });
    }
    Ok((
        OraDocument {
            width,
            height,
            layers,
        },
        unkept,
    ))
}

/// The picture OpenRaster `bytes` shows: its merged image where it carries
/// one of the canvas's size, and otherwise its layers composed.
pub(crate) fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
    let archive = open(bytes)?;
    let size = canvas_of(&archive)?;
    if let Some(merged) = archive.read("mergedimage.png", MOST_LAYER_BYTES)? {
        let image = png::decode(&merged, limits)?;
        if (image.width(), image.height()) == size {
            return Ok(image);
        }
    }
    let (document, _) = decode_native(bytes, limits)?;
    composed(&document)
}

/// `document`'s visible layers composed over clear, each as faint as it is.
fn composed(document: &OraDocument) -> Result<RasterImage, DecodeError> {
    let (width, height) = (document.width as usize, document.height as usize);
    let count = width
        .checked_mul(height)
        .and_then(|count| count.checked_mul(4))
        .ok_or(DecodeError::DimensionsOverflow)?;
    let mut out = tairix_util::fallible::filled(count, 0u8).ok_or(DecodeError::OutOfMemory)?;
    for layer in document.layers.iter().filter(|layer| layer.visible) {
        let colours = layer.picture.to_rgba().ok_or(DecodeError::OutOfMemory)?;
        let (lw, lh) = (
            layer.picture.width() as usize,
            layer.picture.height() as usize,
        );
        for row in 0..lh {
            let Some(down) = offset(layer.at.1, row, height) else {
                continue;
            };
            for column in 0..lw {
                let Some(across) = offset(layer.at.0, column, width) else {
                    continue;
                };
                let from = (row * lw + column) * 4;
                let mut above = [0u8; 4];
                above.copy_from_slice(&colours[from..from + 4]);
                above[3] = scale(above[3], layer.opacity);
                let into = (down * width + across) * 4;
                let mut below = [0u8; 4];
                below.copy_from_slice(&out[into..into + 4]);
                out[into..into + 4].copy_from_slice(&over(below, above));
            }
        }
    }
    Ok(RasterImage::from_parts(
        document.width,
        document.height,
        out,
    ))
}

/// Where a layer's pixel `along` from its edge at `start` falls on a canvas
/// `length` long, if it falls on it at all.
fn offset(start: i32, along: usize, length: usize) -> Option<usize> {
    let at = i64::from(start) + i64::try_from(along).ok()?;
    usize::try_from(at).ok().filter(|&at| at < length)
}

#[cfg(test)]
#[path = "ora_tests.rs"]
mod tests;

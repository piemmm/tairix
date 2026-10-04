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
use core::ops::Range;

use tairix_raster::div255;
use tairix_util::{cnum, fallible};
use tairix_xml::Element;

use crate::encode::EncodeError;
use crate::picture::{masked_colour, over, Picture, PictureSource, Pixels};
use crate::zip::{Archive, View, Writer, ZipError};
use crate::{png, DecodeError, DecodeLimits, RasterImage, Unkept, RGBA_BYTES};

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

/// A stack as read: its canvas, its layers topmost first, and what it held
/// that reading it did not keep.
type Stack = ((u32, u32), Vec<Placed>, Unkept);

/// A layer as the stack places it, before its pixels are read.
struct Placed {
    name: String,
    src: String,
    at: (i32, i32),
    opacity: u8,
    visible: bool,
}

impl Placed {
    /// Whether any of it shows.
    const fn shows(&self) -> bool {
        self.visible && self.opacity > 0
    }
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

/// The canvas `root` declares, refused where it passes `limits`.
fn checked_canvas(root: &Element<'_>, limits: &DecodeLimits) -> Result<(u32, u32), DecodeError> {
    let (width, height) = canvas(root)?;
    limits.check(width, height)?;
    Ok((width, height))
}

/// The canvas OpenRaster `bytes` declare, reading no layer.
pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), DecodeError> {
    canvas(&parse(&stack_xml(&open(bytes)?)?)?)
}

/// The stack `xml` spells, as far as its root element.
fn parse(xml: &str) -> Result<Element<'_>, DecodeError> {
    tairix_xml::parse(xml, "").map_err(|_| DecodeError::OraBadStack)
}

/// The stack under `root`: its canvas, refused where it passes `limits`, and
/// its layers topmost first.
fn read_stack(root: &Element<'_>, limits: &DecodeLimits) -> Result<Stack, DecodeError> {
    let size = checked_canvas(root, limits)?;
    let top = root
        .children()
        .find(|child| child.name == "stack")
        .ok_or(DecodeError::OraBadStack)?;
    let mut placed = Vec::new();
    // The writer here states no resolution, so a stated one is not kept.
    let mut unkept = Unkept {
        extras: root.attr("xres").is_some() || root.attr("yres").is_some(),
        ..Unkept::default()
    };
    fold(top, (u8::MAX, true), &mut placed, &mut unkept)?;
    Ok((size, placed, unkept))
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
        let laid = (
            div255(u32::from(opacity) * u32::from(own.0)),
            visible && own.1,
        );
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
    layers(&archive, read_stack(&parse(&xml)?, limits)?, limits)
}

/// The document `stack` lays out of `archive`'s entries: its layers, the
/// bottom first, each as colour, with what the file held that they do not.
/// A picture several layers name is read and decoded once, so naming one
/// entry many times costs one decode.
fn layers(
    archive: &Archive<'_>,
    ((width, height), mut placed, mut unkept): Stack,
    limits: &DecodeLimits,
) -> Result<(OraDocument, Unkept), DecodeError> {
    let mut layers: Vec<OraLayer> = Vec::new();
    layers
        .try_reserve_exact(placed.len())
        .map_err(|_| DecodeError::OutOfMemory)?;
    for index in (0..placed.len()).rev() {
        // The layers already read are those beneath this one, nearest last.
        let shared = placed
            .get(index + 1..)
            .unwrap_or_default()
            .iter()
            .zip(layers.iter().rev())
            .find(|(beneath, _)| beneath.src == placed[index].src)
            .map(|(_, read)| read.picture.try_clone().ok_or(DecodeError::OutOfMemory));
        let picture = if let Some(copy) = shared {
            copy?
        } else {
            let png = archive
                .read(&placed[index].src, MOST_LAYER_BYTES)?
                .ok_or(DecodeError::OraMissingLayer)?;
            let (picture, held) = png::decode_native(&png, limits)?;
            let indexed = matches!(picture.pixels(), Pixels::Indexed { .. });
            unkept.precision |= held.precision;
            unkept.extras |= held.extras;
            // A layer is colour, so a palette is restated rather than kept.
            unkept.converted |= held.converted || indexed;
            colour(picture)?
        };
        let layer = &mut placed[index];
        layers.push(OraLayer {
            name: core::mem::take(&mut layer.name),
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

/// `picture` as colour, a palette's indices looked up.
fn colour(picture: Picture) -> Result<Picture, DecodeError> {
    if matches!(picture.pixels(), Pixels::Rgba(_)) {
        return Ok(picture);
    }
    let rgba = picture.to_rgba().ok_or(DecodeError::OutOfMemory)?;
    Picture::rgba(picture.width(), picture.height(), rgba)
        .map(|colour| colour.with_density(picture.density()))
        .map_err(|_| DecodeError::DimensionsOverflow)
}

/// The picture OpenRaster `bytes` shows: its merged image where it carries
/// one of the canvas's size, and otherwise the layers that show composed, so
/// a layer nothing of shows is never read.
pub(crate) fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
    let archive = open(bytes)?;
    let xml = stack_xml(&archive)?;
    let root = parse(&xml)?;
    let size = checked_canvas(&root, limits)?;
    if let Some(merged) = archive.read("mergedimage.png", MOST_LAYER_BYTES)? {
        let image = png::decode(&merged, limits)?;
        if (image.width(), image.height()) == size {
            return Ok(image);
        }
    }
    let (size, mut placed, unkept) = read_stack(&root, limits)?;
    placed.retain(Placed::shows);
    let (document, _) = layers(&archive, (size, placed, unkept), limits)?;
    composed(&document)
}

/// An upper bound of the bytes a [`decode`] of `bytes` holds at once, read
/// from the directory, the stack and the layers' own headers: the directory
/// twice over, the stack and its parse, the merged image read and decoded,
/// then — where that does not answer — the layers' path. A layer stored in
/// the archive is costed from its header; one compressed there, or whose
/// header will not read, at the most `limits` admit.
///
/// # Errors
///
/// What [`decode`] would refuse before decoding: a malformed archive, or a
/// stack whose canvas will not read or passes `limits`.
pub(crate) fn peak_bytes(bytes: &[u8], limits: &DecodeLimits) -> Result<u64, DecodeError> {
    let archive = open(bytes)?;
    let stack = archive
        .view("stack.xml")?
        .ok_or(DecodeError::OraBadStack)?
        .size;
    let xml = stack_xml(&archive)?;
    let root = parse(&xml)?;
    let size = checked_canvas(&root, limits)?;
    let directory = 2 * (archive.held_bytes() + MIMETYPE.len()) as u64;
    let parsed = (stack as u64).saturating_add(tairix_xml::parse_peak_bytes(stack));
    let merged = archive.view("mergedimage.png")?;
    // A stored merged image the canvas's size is shown, or refuses the
    // decode, before any layer is read.
    let answers = merged
        .as_ref()
        .and_then(|view| view.stored)
        .and_then(|png| png::probe(png).ok())
        == Some(size);
    let merged = merged.map_or(0, |view| entry_peak(&view, limits));
    // A stack the layers' path refuses ends that path before any layer is read.
    let layered = if answers {
        0
    } else {
        read_stack(&root, limits).map_or(0, |(size, placed, _)| {
            layers_peak(&archive, (size, &placed, stack), limits)
        })
    };
    Ok([parsed, merged, layered]
        .into_iter()
        .fold(directory, u64::saturating_add))
}

/// What the layers' path holds over a `canvas` holding `placed` read from a
/// stack of `stack` bytes: the layer records, every layer that shows kept
/// decoded beside the costliest being read, and the canvas they are composed
/// into. A layer the archive does not hold ends the path there, so it costs
/// nothing.
fn layers_peak(
    archive: &Archive<'_>,
    ((width, height), placed, stack): ((u32, u32), &[Placed], usize),
    limits: &DecodeLimits,
) -> u64 {
    use core::mem::size_of;
    // The records grow by doubling; their names are copied out of the stack.
    let records = ((4 + 3 * placed.len()) * size_of::<Placed>() + stack) as u64;
    let mut kept = 0u64;
    let mut reading = 0u64;
    let mut shown = 0usize;
    for view in placed
        .iter()
        .filter(|layer| layer.shows())
        .filter_map(|layer| archive.view(&layer.src).ok().flatten())
    {
        kept = kept.saturating_add(layer_bytes(&view, limits));
        reading = reading.max(entry_peak(&view, limits));
        shown += 1;
    }
    let layers = (shown * size_of::<OraLayer>()) as u64;
    let canvas = u64::from(width) * u64::from(height) * RGBA_BYTES as u64;
    [kept, reading, layers, canvas]
        .into_iter()
        .fold(records, u64::saturating_add)
}

/// What reading `view` out of the archive and decoding it as a PNG holds.
fn entry_peak(view: &View<'_>, limits: &DecodeLimits) -> u64 {
    let decode = view
        .stored
        .and_then(|png| png::peak_bytes(png, limits).ok())
        .unwrap_or_else(|| png::peak_ceiling(view.size, limits));
    (view.size as u64).saturating_add(decode)
}

/// The colours the layer `view` holds decode to, kept beside the others.
fn layer_bytes(view: &View<'_>, limits: &DecodeLimits) -> u64 {
    view.stored
        .and_then(|png| png::probe(png).ok())
        .map_or(limits.max_pixels(), |(width, height)| {
            u64::from(width) * u64::from(height)
        })
        .saturating_mul(RGBA_BYTES as u64)
}

/// `document`'s showing layers composed over clear, each as faint as it is.
fn composed(document: &OraDocument) -> Result<RasterImage, DecodeError> {
    let (width, height) = (document.width as usize, document.height as usize);
    let count = width
        .checked_mul(height)
        .and_then(|count| count.checked_mul(RGBA_BYTES))
        .ok_or(DecodeError::DimensionsOverflow)?;
    let mut out = fallible::filled(count, 0u8).ok_or(DecodeError::OutOfMemory)?;
    for layer in document
        .layers
        .iter()
        .filter(|layer| layer.visible && layer.opacity > 0)
    {
        let picture = &layer.picture;
        // A layer is colour, so its rows are read where they lie.
        let Pixels::Rgba(rgba) = picture.pixels() else {
            return Err(DecodeError::DimensionsOverflow);
        };
        let span = picture.width() as usize;
        let (Some((columns, across)), Some((rows, down))) = (
            landing(layer.at.0, span, width),
            landing(layer.at.1, picture.height() as usize, height),
        ) else {
            continue;
        };
        for (offset, y) in rows.enumerate() {
            let first = y * span;
            let colours = rgba
                .get((first + columns.start) * RGBA_BYTES..(first + columns.end) * RGBA_BYTES)
                .ok_or(DecodeError::DimensionsOverflow)?;
            let start = ((down + offset) * width + across) * RGBA_BYTES;
            let targets = out
                .get_mut(start..start + colours.len())
                .ok_or(DecodeError::DimensionsOverflow)?;
            for (below, above) in targets
                .as_chunks_mut::<RGBA_BYTES>()
                .0
                .iter_mut()
                .zip(colours.as_chunks::<RGBA_BYTES>().0)
            {
                *below = over(*below, masked_colour(*above, layer.opacity));
            }
        }
    }
    Ok(RasterImage::from_parts(
        document.width,
        document.height,
        out,
    ))
}

/// Of `length` pixels along one axis from a layer's edge at `start`, the run
/// that falls on a canvas `canvas` long and where its first falls; `None`
/// where none does.
fn landing(start: i32, length: usize, canvas: usize) -> Option<(Range<usize>, usize)> {
    let start = i64::from(start);
    let first = (-start).max(0);
    let end = i64::try_from(length)
        .ok()?
        .min(i64::try_from(canvas).ok()? - start);
    if first >= end {
        return None;
    }
    Some((
        usize::try_from(first).ok()?..usize::try_from(end).ok()?,
        usize::try_from(start + first).ok()?,
    ))
}

#[cfg(test)]
#[path = "ora_tests.rs"]
mod tests;

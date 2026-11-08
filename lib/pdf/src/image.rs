//! Images (ISO 32000-2 §8.9): image objects and inline images decoded into
//! straight-alpha sRGB pictures, their masks applied.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::SharedSlice;
use tairix_image::{DecodeLimits, ImageFormat, Orientation, PROGRESSIVE_420_BYTES_PER_PIXEL};
use tairix_util::fallible;

use crate::colour::{Space, Srgb};
pub(crate) use crate::display::Picture;
use crate::document::{tolerated, Document};
use crate::filter::{charge, Decoded, ImageCodec};
use crate::function::{numbers, values, Bits, Work, MAX_COMPONENTS};
use crate::object::{Dict, Name, Object};
use crate::shading::level;
use crate::PdfError;

/// The widest and tallest image read: past it an image is refused rather than
/// allocated.
const MAX_SIDE: u32 = 1 << 16;

/// How an image is painted.
pub(crate) enum Kind {
    /// In its own colours, sampled in its colour space.
    Colour(Arc<Space>),
    /// As a stencil the fill colour is painted through.
    Stencil(Srgb),
}

/// The picture image dictionary `dict` describes, its samples `decoded`,
/// painted as `kind`.
pub(crate) fn picture(
    document: &Document<'_>,
    dict: &Dict,
    decoded: Decoded,
    kind: &Kind,
) -> Result<Picture, PdfError> {
    let width = side(document.get(dict, b"Width")?.as_int())?;
    let height = side(document.get(dict, b"Height")?.as_int())?;
    // A codec not read here is refused before anything is spent on it.
    match (kind, decoded.image) {
        (_, None) => {}
        (Kind::Colour(space), Some(ImageCodec::Dct))
            if matches!(**space, Space::Gray | Space::Rgb) => {}
        _ => return Err(PdfError::Unsupported),
    }
    let pixels = pixel_bytes(width, height)?;
    charge(document.budget(), pixels)?;
    let mut rgba = fallible::zeroed(pixels).ok_or(PdfError::OutOfMemory)?;
    let mut jpeg = None;
    match kind {
        Kind::Stencil(colour) => {
            let invert = numbers(document, dict, b"Decode")?
                .is_some_and(|decode| decode.first() == Some(&1.0));
            let bits = mask_bits(document, &decoded, width, height)?;
            let [r, g, b] = colour.map(level);
            for (pixel, &shown) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(&bits) {
                *pixel = [r, g, b, if shown == invert { 0 } else { 255 }];
            }
        }
        Kind::Colour(space) => coloured(
            document,
            dict,
            decoded,
            (space, (width, height)),
            &mut rgba,
            &mut jpeg,
        )?,
    }
    Ok(Picture {
        width,
        height,
        pixels: rgba,
        jpeg,
    })
}

/// Fill `rgba` with the pixels of a colour image `width` by `height`,
/// sampled in `space` and masked; `jpeg` takes its JPEG's bytes where the
/// pixels are the JPEG's own.
fn coloured(
    document: &Document<'_>,
    dict: &Dict,
    decoded: Decoded,
    (space, (width, height)): (&Space, (u32, u32)),
    rgba: &mut [u8],
    jpeg: &mut Option<SharedSlice<u8>>,
) -> Result<(), PdfError> {
    match &decoded.image {
        Some(ImageCodec::Dct) => {
            let plain = dct(document, &decoded.bytes, (width, height), space, rgba)?;
            let unmasked = dict.get(b"SMask").is_none()
                && dict.get(b"Mask").is_none()
                && dict.get(b"Decode").is_none();
            if plain && unmasked {
                *jpeg = Some(SharedSlice::try_from_vec(decoded.bytes)?);
            }
        }
        Some(_) => return Err(PdfError::Unsupported),
        None => {
            let bits = bits_per_component(document, dict)?;
            let decode = match numbers(document, dict, b"Decode")? {
                Some(decode) if decode.len() == 2 * space.components() => decode
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&[low, high]| (low, high))
                    .collect(),
                _ => space.decode_ranges(bits),
            };
            samples(
                space,
                (width, height, bits),
                &decode,
                &decoded.bytes,
                (rgba, document.work()),
            )?;
            if let Some(keys) = document.get(dict, b"Mask")?.as_array() {
                colour_key(
                    document,
                    keys,
                    (width, height, bits),
                    space.components(),
                    &decoded.bytes,
                    rgba,
                )?;
            }
        }
    }
    let mask = document.get(dict, b"SMask")?;
    if let Some(mask) = mask.as_stream() {
        let decoded = document.decode(mask)?;
        let matte = match numbers(document, &mask.dict, b"Matte")? {
            Some(matte) => space.srgb(&values(matte), document.work())?,
            None => None,
        };
        soft_mask(
            document,
            &mask.dict,
            &decoded,
            ((width, height), matte),
            rgba,
        )?;
    } else if let Some(mask) = document.get(dict, b"Mask")?.as_stream() {
        let decoded = document.decode(mask)?;
        stencil_mask(document, &mask.dict, &decoded, (width, height), rgba)?;
    }
    Ok(())
}

/// A side of an image, in pixels.
fn side(value: Option<i64>) -> Result<u32, PdfError> {
    match value {
        Some(side @ 1..) => u32::try_from(side)
            .ok()
            .filter(|&side| side <= MAX_SIDE)
            .ok_or(PdfError::TooComplex),
        _ => Err(PdfError::Malformed),
    }
}

/// The bytes a picture of `width` by `height` takes.
fn pixel_bytes(width: u32, height: u32) -> Result<usize, PdfError> {
    usize::try_from(u64::from(width) * u64::from(height) * 4).map_err(|_| PdfError::TooComplex)
}

fn bits_per_component(document: &Document<'_>, dict: &Dict) -> Result<u32, PdfError> {
    match document
        .get(dict, b"BitsPerComponent")?
        .as_int()
        .map(u32::try_from)
    {
        Some(Ok(bits @ (1 | 2 | 4 | 8 | 16))) => Ok(bits),
        _ => Err(PdfError::Malformed),
    }
}

/// The bytes a row of `width` samples of `components` at `bits` takes.
fn row_bytes(width: u32, components: usize, bits: u32) -> Result<usize, PdfError> {
    let bits = u64::from(width)
        * u64::try_from(components).map_err(|_| PdfError::TooComplex)?
        * u64::from(bits);
    usize::try_from(bits.div_ceil(8)).map_err(|_| PdfError::TooComplex)
}

/// A one-bit mask's samples, one a pixel, each whether it is 0 — where a
/// stencil paints — a short mask read as painting nowhere past its end, what
/// they are held in charged to `document`'s budget.
fn mask_bits(
    document: &Document<'_>,
    decoded: &Decoded,
    width: u32,
    height: u32,
) -> Result<Vec<bool>, PdfError> {
    if decoded.image.is_some() {
        return Err(PdfError::Unsupported);
    }
    let row = row_bytes(width, 1, 1)?;
    let count =
        usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| PdfError::TooComplex)?;
    charge(document.budget(), count)?;
    let mut shown = Vec::new();
    shown.try_reserve_exact(count)?;
    let mut lines = decoded.bytes.chunks_exact(row);
    for _ in 0..height {
        let line = lines.next();
        let mut bits = Bits::new(line.unwrap_or_default());
        for _ in 0..width {
            shown.push(line.is_some() && bits.take(1) == 0);
        }
    }
    Ok(shown)
}

/// The most entries a table of every colour an image's samples can spell
/// holds: past it, its pixels are converted as they come.
const MAX_TABLE_BITS: u32 = 16;

/// Raw samples into `rgba`: directly for plain RGB bytes, through a table of
/// every colour the samples can spell where that is no larger than the image,
/// and otherwise through the colour space a pixel at a time, a run of equal
/// pixels converted once; a tint transform's steps charged to `work`.
fn samples(
    space: &Space,
    (width, height, bits): (u32, u32, u32),
    decode: &[(f64, f64)],
    data: &[u8],
    (rgba, work): (&mut [u8], &Work),
) -> Result<(), PdfError> {
    let components = space.components();
    if decode.len() != components {
        return Err(PdfError::Malformed);
    }
    let row = row_bytes(width, components, bits)?;
    let full = f64::from(u32::MAX >> (32 - bits));
    let colour_of = |raw: &[u32]| -> Result<[u8; 4], PdfError> {
        let read = values(
            raw.iter()
                .zip(decode)
                .map(|(&sample, &(low, high))| low + f64::from(sample) * (high - low) / full),
        );
        Ok(space.srgb(&read, work)?.map_or([0, 0, 0, 0], |rgb| {
            let [r, g, b] = rgb.map(level);
            [r, g, b, 255]
        }))
    };
    let device_bytes = bits == 8 && decode.iter().all(|&range| range == (0.0, 1.0));
    let pixels = u64::from(width) * u64::from(height);
    let spelled = u32::try_from(components)
        .ok()
        .and_then(|components| components.checked_mul(bits))
        .filter(|&spelled| spelled <= MAX_TABLE_BITS && (1u64 << spelled) <= pixels.max(256));
    let table = match spelled {
        Some(spelled) if !(device_bytes && matches!(space, Space::Rgb)) => {
            Some((table(&colour_of, spelled, (components, bits))?, spelled))
        }
        _ => None,
    };
    let mut raw = [0u32; MAX_COMPONENTS];
    let raw = &mut raw[..components];
    let mut last: Option<([u32; MAX_COMPONENTS], [u8; 4])> = None;
    for (line, out) in data
        .chunks_exact(row)
        .zip(rgba.chunks_exact_mut(width as usize * 4))
    {
        let pixels = out.as_chunks_mut::<4>().0;
        let mut reader = Bits::new(line);
        match &table {
            Some((table, 8)) => {
                for (pixel, &index) in pixels.iter_mut().zip(line) {
                    *pixel = table[usize::from(index)];
                }
            }
            Some((table, 16)) => {
                for (pixel, &index) in pixels.iter_mut().zip(line.as_chunks::<2>().0) {
                    *pixel = table[usize::from(u16::from_be_bytes(index))];
                }
            }
            Some((table, spelled)) => {
                for pixel in pixels {
                    *pixel = table[reader.take(*spelled) as usize];
                }
            }
            None if device_bytes && matches!(space, Space::Rgb) => {
                for (pixel, &[r, g, b]) in pixels.iter_mut().zip(line.as_chunks::<3>().0) {
                    *pixel = [r, g, b, 255];
                }
            }
            None => {
                for pixel in pixels {
                    for sample in raw.iter_mut() {
                        *sample = reader.take(bits);
                    }
                    *pixel = match &last {
                        Some((held, colour)) if held[..components] == *raw => *colour,
                        _ => {
                            let colour = colour_of(raw)?;
                            let mut held = [0u32; MAX_COMPONENTS];
                            held[..components].copy_from_slice(raw);
                            last = Some((held, colour));
                            colour
                        }
                    };
                }
            }
        }
    }
    Ok(())
}

/// Every colour samples `spelled` bits wide come to, one an index: a pixel's
/// samples read as one number, its first component highest, as `Bits` reads
/// them together.
fn table(
    colour_of: &impl Fn(&[u32]) -> Result<[u8; 4], PdfError>,
    spelled: u32,
    (components, bits): (usize, u32),
) -> Result<Vec<[u8; 4]>, PdfError> {
    let entries = 1u32 << spelled;
    let mut table = Vec::new();
    table.try_reserve_exact(entries as usize)?;
    let mask = u32::MAX >> (32 - bits);
    let mut raw = [0u32; MAX_COMPONENTS];
    let raw = &mut raw[..components];
    for index in 0..entries {
        let mut rest = index;
        for sample in raw.iter_mut().rev() {
            *sample = rest & mask;
            rest >>= bits;
        }
        table.push(colour_of(raw)?);
    }
    Ok(table)
}

/// A JPEG's pixels into `rgba`, as stored: a JPEG inside a PDF is placed by
/// the page, so an orientation its metadata states is undone. Answers whether
/// its bytes alone are the picture.
fn dct(
    document: &Document<'_>,
    bytes: &[u8],
    (width, height): (u32, u32),
    space: &Space,
    rgba: &mut [u8],
) -> Result<bool, PdfError> {
    let frame = tairix_image::jpeg_frame(bytes).map_err(|error| image_error(&error))?;
    let wanted = match space {
        Space::Gray => 1,
        Space::Rgb => 3,
        _ => return Err(PdfError::Unsupported),
    };
    if frame.components != wanted || frame.stored != (width, height) {
        return Err(PdfError::Unsupported);
    }
    // The decoder answers pixels of its own before they are copied over.
    charge(document.budget(), pixel_bytes(width, height)?)?;
    let room = document.budget().get() / 4;
    let limits = DecodeLimits::new(
        MAX_SIDE,
        MAX_SIDE,
        room,
        room.saturating_mul(PROGRESSIVE_420_BYTES_PER_PIXEL),
    );
    let decoded = tairix_image::decode_as(ImageFormat::Jpeg, bytes, &limits)
        .map_err(|error| image_error(&error))?;
    if frame.orientation == Orientation::from_tag(1).unwrap_or(frame.orientation) {
        rgba.copy_from_slice(decoded.pixels());
        return Ok(true);
    }
    let to_picture = frame.orientation.to_picture((width, height));
    let (across, _) = frame.orientation.picture_size(width, height);
    let source = decoded.pixels();
    for y in 0..height {
        for x in 0..width {
            let at = to_picture.map(tairix_bezier::Point::new(
                f64::from(x) + 0.5,
                f64::from(y) + 0.5,
            ));
            let (px, py) = (whole(at.x), whole(at.y));
            let from = (py * across as usize + px) * 4;
            let to = (y as usize * width as usize + x as usize) * 4;
            if let Some(pixel) = source.get(from..from + 4) {
                rgba[to..to + 4].copy_from_slice(pixel);
            }
        }
    }
    Ok(false)
}

/// A pixel centre's coordinate as its pixel's index.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a pixel centre within the picture, floored and held non-negative, before the cast"
)]
fn whole(value: f64) -> usize {
    value.max(0.0) as usize
}

fn image_error(error: &tairix_image::DecodeError) -> PdfError {
    match error {
        tairix_image::DecodeError::OutOfMemory => PdfError::OutOfMemory,
        _ => PdfError::Unsupported,
    }
}

/// Clear the pixels whose raw samples all fall within colour key mask
/// `keys`' ranges.
fn colour_key(
    document: &Document<'_>,
    keys: &[Object],
    (width, height, bits): (u32, u32, u32),
    components: usize,
    data: &[u8],
    rgba: &mut [u8],
) -> Result<(), PdfError> {
    if keys.len() != 2 * components {
        return Err(PdfError::Malformed);
    }
    let mut ranges = Vec::new();
    ranges.try_reserve_exact(components)?;
    for [low, high] in keys.as_chunks::<2>().0 {
        let bound = |object: &Object| -> Result<i64, PdfError> {
            document
                .resolve(object)?
                .as_int()
                .ok_or(PdfError::Malformed)
        };
        ranges.push((bound(low)?, bound(high)?));
    }
    let row = row_bytes(width, components, bits)?;
    let lines = data
        .chunks_exact(row)
        .zip(rgba.chunks_exact_mut(width as usize * 4))
        .take(height as usize);
    for (line, out) in lines {
        let mut reader = Bits::new(line);
        for pixel in out.as_chunks_mut::<4>().0 {
            let mut keyed = true;
            for &(low, high) in &ranges {
                let sample = i64::from(reader.take(bits));
                keyed &= (low..=high).contains(&sample);
            }
            if keyed {
                pixel[3] = 0;
            }
        }
    }
    Ok(())
}

/// A soft mask image's grey into `rgba`'s alpha, sampled to the image's size,
/// and `matte`, the colour the image's colours were blended with, undone.
fn soft_mask(
    document: &Document<'_>,
    dict: &Dict,
    decoded: &Decoded,
    ((width, height), matte): ((u32, u32), Option<Srgb>),
    rgba: &mut [u8],
) -> Result<(), PdfError> {
    let mask_width = side(document.get(dict, b"Width")?.as_int())?;
    let mask_height = side(document.get(dict, b"Height")?.as_int())?;
    let pixels = pixel_bytes(mask_width, mask_height)?;
    charge(document.budget(), pixels)?;
    let mut grey = fallible::zeroed(pixels).ok_or(PdfError::OutOfMemory)?;
    match &decoded.image {
        Some(ImageCodec::Dct) => {
            dct(
                document,
                &decoded.bytes,
                (mask_width, mask_height),
                &Space::Gray,
                &mut grey,
            )?;
        }
        Some(_) => return Err(PdfError::Unsupported),
        None => {
            let bits = bits_per_component(document, dict)?;
            let decode = match numbers(document, dict, b"Decode")?.as_deref() {
                Some(&[low, high]) => alloc::vec![(low, high)],
                _ => alloc::vec![(0.0, 1.0)],
            };
            samples(
                &Space::Gray,
                (mask_width, mask_height, bits),
                &decode,
                &decoded.bytes,
                (&mut grey, document.work()),
            )?;
        }
    }
    let columns = nearest_columns(width, mask_width)?;
    for y in 0..height {
        let my = nearest(y, height, mask_height);
        for (x, &mx) in columns.iter().enumerate() {
            let alpha = grey[(my * mask_width as usize + mx) * 4];
            let at = (y as usize * width as usize + x) * 4;
            let pixel = &mut rgba[at..at + 4];
            if let (Some(matte), true) = (matte, alpha > 0) {
                for (channel, background) in pixel[..3].iter_mut().zip(matte) {
                    let background = background * 255.0;
                    let unblended =
                        (f64::from(*channel) - background) * 255.0 / f64::from(alpha) + background;
                    *channel = level(unblended / 255.0);
                }
            }
            pixel[3] = level(f64::from(pixel[3]) / 255.0 * f64::from(alpha) / 255.0);
        }
    }
    Ok(())
}

/// An explicit stencil mask into `rgba`'s alpha: clear where the mask's
/// sample is 1, sampled to the image's size.
fn stencil_mask(
    document: &Document<'_>,
    dict: &Dict,
    decoded: &Decoded,
    (width, height): (u32, u32),
    rgba: &mut [u8],
) -> Result<(), PdfError> {
    let mask_width = side(document.get(dict, b"Width")?.as_int())?;
    let mask_height = side(document.get(dict, b"Height")?.as_int())?;
    let invert =
        numbers(document, dict, b"Decode")?.is_some_and(|decode| decode.first() == Some(&1.0));
    let shown = mask_bits(document, decoded, mask_width, mask_height)?;
    let columns = nearest_columns(width, mask_width)?;
    for y in 0..height {
        let my = nearest(y, height, mask_height);
        for (x, &mx) in columns.iter().enumerate() {
            if shown[my * mask_width as usize + mx] == invert {
                rgba[(y as usize * width as usize + x) * 4 + 3] = 0;
            }
        }
    }
    Ok(())
}

/// The row or column of a side `to` long nearest `at` of a side `from` long.
fn nearest(at: u32, from: u32, to: u32) -> usize {
    let scaled = u64::from(at) * u64::from(to) / u64::from(from);
    u32::try_from(scaled).map_or(to as usize - 1, |scaled| {
        (scaled as usize).min(to as usize - 1)
    })
}

/// For each of `width` columns, the column of a mask `mask_width` wide
/// nearest it: worked out once for every row.
fn nearest_columns(width: u32, mask_width: u32) -> Result<Vec<usize>, PdfError> {
    let mut columns = Vec::new();
    columns.try_reserve_exact(width as usize)?;
    columns.extend((0..width).map(|x| nearest(x, width, mask_width)));
    Ok(columns)
}

/// Inline image key `key` spelled out in full, as an image object's
/// dictionary spells it.
pub(crate) fn spelled_out(key: Vec<u8>) -> Result<Name, PdfError> {
    let full: &[u8] = match key.as_slice() {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        b"L" => b"Length",
        _ => return Ok(Name(key)),
    };
    Name::of(full)
}

/// Where an inline image's data ends in `data`, which begins at its first
/// byte: its stated length, its samples' own length where it is not filtered
/// and its colour space, `space`, is known, or else the first `EI` that
/// stands alone and is followed by content rather than more binary data, as
/// every reader finds it.
pub(crate) fn inline_end(
    document: &Document<'_>,
    dict: &Dict,
    space: Option<&Space>,
    data: &[u8],
) -> Result<usize, PdfError> {
    if let Some(length) = document
        .get(dict, b"Length")?
        .as_int()
        .and_then(|length| usize::try_from(length).ok())
    {
        return Ok(length.min(data.len()));
    }
    if dict.get(b"Filter").is_none() {
        if let Some(Some(length)) = tolerated(sample_bytes(document, dict, space))? {
            return Ok(length.min(data.len()));
        }
    }
    let mut at = 0;
    while let Some(found) = crate::parse::find(&data[at..], b"EI") {
        let end = at + found;
        at = end + 2;
        let alone_before = end == 0 || crate::lex::is_white(data[end - 1]);
        let alone_after = data
            .get(end + 2)
            .is_none_or(|&byte| crate::lex::is_white(byte) || crate::lex::is_delimiter(byte));
        let content_after = data[end + 2..].iter().take(INLINE_PROBE).all(|&byte| {
            byte == b'\t'
                || byte == b'\n'
                || byte == b'\r'
                || byte == 0x0c
                || (b' '..=b'~').contains(&byte)
        });
        if alone_before && alone_after && content_after {
            return Ok(end.saturating_sub(1));
        }
    }
    Ok(data.len())
}

/// The bytes an unfiltered inline image's samples take; `None` for a colour
/// image whose space is not known.
fn sample_bytes(
    document: &Document<'_>,
    dict: &Dict,
    space: Option<&Space>,
) -> Result<Option<usize>, PdfError> {
    let width = side(document.get(dict, b"Width")?.as_int())?;
    let height = side(document.get(dict, b"Height")?.as_int())?;
    let (components, bits) = if document.get(dict, b"ImageMask")?.as_bool() == Some(true) {
        (1, 1)
    } else {
        let Some(space) = space else {
            return Ok(None);
        };
        (space.components(), bits_per_component(document, dict)?)
    };
    let length = row_bytes(width, components, bits)?
        .checked_mul(height as usize)
        .ok_or(PdfError::TooComplex)?;
    Ok(Some(length))
}

/// How many bytes after a candidate `EI` must read as content for it to end
/// an inline image.
const INLINE_PROBE: usize = 64;

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;

//! Images (ISO 32000-2 §8.9): image objects and inline images decoded into
//! straight-alpha sRGB pictures, their masks applied.

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::SharedSlice;
use tairix_image::{DecodeLimits, ImageFormat, Orientation, PROGRESSIVE_420_BYTES_PER_PIXEL};
use tairix_util::fallible;

use crate::colour::{Space, Srgb};
pub(crate) use crate::display::Picture;
use crate::document::Document;
use crate::error::tolerated;
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
    Stencil,
}

/// The picture colour image dictionary `dict` describes, its samples
/// `decoded`, sampled in `space`.
pub(crate) fn picture(
    document: &Document,
    dict: &Dict,
    decoded: Decoded,
    space: &Space,
) -> Result<Picture, PdfError> {
    let width = side(document.get(dict, b"Width")?.as_int())?;
    let height = side(document.get(dict, b"Height")?.as_int())?;
    // A codec not read here is refused before anything is spent on it.
    match decoded.image {
        None => {}
        Some(ImageCodec::Dct) if matches!(space, Space::Gray | Space::Rgb) => {}
        Some(_) => return Err(PdfError::Unsupported),
    }
    let pixels = pixel_bytes(width, height)?;
    charge(document.budget(), pixels)?;
    let mut rgba = fallible::zeroed(pixels).ok_or(PdfError::OutOfMemory)?;
    let mut jpeg = None;
    coloured(
        document,
        dict,
        decoded,
        (space, (width, height)),
        &mut rgba,
        &mut jpeg,
    )?;
    Ok(Picture {
        width,
        height,
        pixels: rgba,
        jpeg,
    })
}

/// A stencil's samples, read once however many colours it is painted in:
/// for each pixel, whether it paints.
pub(crate) struct Stencil {
    width: u32,
    height: u32,
    paints: Vec<bool>,
}

/// The stencil image dictionary `dict` describes, its samples `decoded`.
pub(crate) fn stencil(
    document: &Document,
    dict: &Dict,
    decoded: &Decoded,
) -> Result<Stencil, PdfError> {
    let width = side(document.get(dict, b"Width")?.as_int())?;
    let height = side(document.get(dict, b"Height")?.as_int())?;
    let paints = mask_bits(document, dict, decoded, (width, height))?;
    Ok(Stencil {
        width,
        height,
        paints,
    })
}

impl Stencil {
    /// The picture it paints in `colour`, what that is held in charged to
    /// `document`'s budget.
    pub(crate) fn painted(&self, document: &Document, colour: Srgb) -> Result<Picture, PdfError> {
        let pixels = pixel_bytes(self.width, self.height)?;
        charge(document.budget(), pixels)?;
        let mut rgba = fallible::zeroed(pixels).ok_or(PdfError::OutOfMemory)?;
        let [r, g, b] = colour.map(level);
        for (pixel, &paints) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(&self.paints) {
            *pixel = [r, g, b, if paints { 255 } else { 0 }];
        }
        Ok(Picture {
            width: self.width,
            height: self.height,
            pixels: rgba,
            jpeg: None,
        })
    }
}

/// Fill `rgba` with the pixels of a colour image `width` by `height`,
/// sampled in `space` and masked; `jpeg` takes its JPEG's bytes where the
/// pixels are the JPEG's own.
fn coloured(
    document: &Document,
    dict: &Dict,
    decoded: Decoded,
    (space, (width, height)): (&Space, (u32, u32)),
    rgba: &mut [u8],
    jpeg: &mut Option<SharedSlice<u8>>,
) -> Result<(), PdfError> {
    let mask = document.get(dict, b"Mask")?;
    let plain = sampled(
        document,
        dict,
        &decoded,
        (space, (width, height)),
        rgba,
        mask.as_array(),
    )?;
    let soft = document.get(dict, b"SMask")?;
    if let Some(soft) = soft.as_stream() {
        let decoded = document.decode(soft)?;
        let matte = match numbers(document, &soft.dict, b"Matte", MAX_COMPONENTS)? {
            Some(matte) => space.srgb(&values(matte), document.work())?,
            None => None,
        };
        soft_mask(
            document,
            &soft.dict,
            &decoded,
            ((width, height), matte),
            rgba,
        )?;
    } else if let Some(stencil) = mask.as_stream() {
        let decoded = document.decode(stencil)?;
        stencil_mask(document, &stencil.dict, &decoded, (width, height), rgba)?;
    }
    if plain && dict.get(b"SMask").is_none() && dict.get(b"Mask").is_none() {
        *jpeg = Some(SharedSlice::try_from_vec(decoded.bytes)?);
    }
    Ok(())
}

/// Fill `rgba` with the pixels of an image `width` by `height` whose samples
/// `decoded` are read in `space` through its `/Decode`, clearing those its
/// colour key `keys` names. A JPEG's samples are the pixels it decodes to,
/// taken back out of them only where a decode array or a colour key reads
/// them. Answers whether the pixels are the JPEG's own.
fn sampled(
    document: &Document,
    dict: &Dict,
    decoded: &Decoded,
    (space, (width, height)): (&Space, (u32, u32)),
    rgba: &mut [u8],
    keys: Option<&[Object]>,
) -> Result<bool, PdfError> {
    let (raw, bits) = match decoded.image {
        Some(ImageCodec::Dct) => {
            let plain = dct(document, &decoded.bytes, (width, height), space, rgba)?;
            if keys.is_none() && dict.get(b"Decode").is_none() {
                return Ok(plain);
            }
            (
                Cow::Owned(jpeg_samples(document, rgba, space.components())?),
                8,
            )
        }
        Some(_) => return Err(PdfError::Unsupported),
        None => (
            Cow::Borrowed(decoded.bytes.as_slice()),
            bits_per_component(document, dict)?,
        ),
    };
    let decode = decode_array(document, dict, space, bits)?;
    samples(
        space,
        (width, height, bits),
        &decode,
        &raw,
        (rgba, document.work()),
    )?;
    if let Some(keys) = keys {
        colour_key(
            document,
            keys,
            (width, height, bits),
            space.components(),
            &raw,
            rgba,
        )?;
    }
    Ok(false)
}

/// The range each of `space`'s components decodes into from samples `bits`
/// deep: `dict`'s `/Decode` where it states one for every component, and the
/// space's own where not.
fn decode_array(
    document: &Document,
    dict: &Dict,
    space: &Space,
    bits: u32,
) -> Result<Vec<(f64, f64)>, PdfError> {
    let decode = tolerated(numbers(document, dict, b"Decode", 2 * MAX_COMPONENTS))?;
    Ok(match decode.flatten() {
        Some(decode) if decode.len() == 2 * space.components() => decode
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[low, high]| (low, high))
            .collect(),
        _ => space.decode_ranges(bits),
    })
}

/// A JPEG's samples, `components` a pixel, taken back out of the pixels
/// `rgba` it decoded to, a grey one's from each pixel's red; what they are
/// held in is charged to `document`'s budget.
fn jpeg_samples(document: &Document, rgba: &[u8], components: usize) -> Result<Vec<u8>, PdfError> {
    let pixels = rgba.as_chunks::<4>().0;
    let count = pixels
        .len()
        .checked_mul(components)
        .ok_or(PdfError::TooComplex)?;
    charge(document.budget(), count)?;
    let mut raw = Vec::new();
    raw.try_reserve_exact(count)?;
    for pixel in pixels {
        raw.extend_from_slice(pixel.get(..components).ok_or(PdfError::Unsupported)?);
    }
    Ok(raw)
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

fn bits_per_component(document: &Document, dict: &Dict) -> Result<u32, PdfError> {
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

/// A one-bit mask's samples, one a pixel, each whether it paints: where it is
/// 0, or 1 where `dict`'s `/Decode` inverts it, a short mask painting nowhere
/// past its end. What they are held in is charged to `document`'s budget.
fn mask_bits(
    document: &Document,
    dict: &Dict,
    decoded: &Decoded,
    (width, height): (u32, u32),
) -> Result<Vec<bool>, PdfError> {
    if decoded.image.is_some() {
        return Err(PdfError::Unsupported);
    }
    let decode = tolerated(numbers(document, dict, b"Decode", 2 * MAX_COMPONENTS))?;
    let invert = decode
        .flatten()
        .is_some_and(|decode| decode.first() == Some(&1.0));
    let row = row_bytes(width, 1, 1)?;
    let count =
        usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| PdfError::TooComplex)?;
    charge(document.budget(), count)?;
    let mut paints = Vec::new();
    paints.try_reserve_exact(count)?;
    let mut lines = decoded.bytes.chunks_exact(row);
    for _ in 0..height {
        let line = lines.next();
        let mut bits = Bits::new(line.unwrap_or_default());
        for _ in 0..width {
            paints.push(line.is_some() && (bits.take(1) == 0) != invert);
        }
    }
    Ok(paints)
}

/// The most entries a table of every colour an image's samples can spell
/// holds: past it, its colours are converted as they come, a memo keeping
/// those met.
const MAX_TABLE_BITS: u32 = 16;

/// The most colours that memo holds, each in the slot its samples hash to: a
/// colour met again is converted once, in memory fixed however many colours
/// an image holds.
const MEMO_SLOTS: usize = 1 << 12;

/// How an image's samples become colours.
enum Conversion {
    /// Through a table of every colour samples so many bits wide spell.
    Table(Vec<[u8; 4]>, u32),
    /// As they are: plain RGB bytes.
    Bytes,
    /// Through the colour space, each colour once while the memo holds it.
    Memoised(Memo),
}

/// Raw samples into `rgba`: directly for plain RGB bytes, through a table of
/// every colour the samples can spell where that is no larger than the image,
/// and otherwise through the colour space, each colour once while the memo
/// holds it; a tint transform's steps charged to `work` for each colour
/// converted.
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
        // A table costs a conversion an entry, so it is built only where the
        // image has at least as many pixels to convert.
        .filter(|&spelled| spelled <= MAX_TABLE_BITS && (1u64 << spelled) <= pixels);
    let mut conversion = match spelled {
        _ if device_bytes && matches!(space, Space::Rgb) => Conversion::Bytes,
        Some(spelled) => {
            Conversion::Table(table(&colour_of, spelled, (components, bits))?, spelled)
        }
        None => Conversion::Memoised(Memo::new(components, pixels)?),
    };
    let mut raw = [0u32; MAX_COMPONENTS];
    let raw = &mut raw[..components];
    for (line, out) in data
        .chunks_exact(row)
        .zip(rgba.chunks_exact_mut(width as usize * 4))
    {
        let pixels = out.as_chunks_mut::<4>().0;
        let mut reader = Bits::new(line);
        match &mut conversion {
            Conversion::Table(table, 8) => {
                for (pixel, &index) in pixels.iter_mut().zip(line) {
                    *pixel = table[usize::from(index)];
                }
            }
            Conversion::Table(table, 16) => {
                for (pixel, &index) in pixels.iter_mut().zip(line.as_chunks::<2>().0) {
                    *pixel = table[usize::from(u16::from_be_bytes(index))];
                }
            }
            Conversion::Table(table, spelled) => {
                for pixel in pixels {
                    *pixel = table[reader.take(*spelled) as usize];
                }
            }
            Conversion::Bytes => {
                for (pixel, &[r, g, b]) in pixels.iter_mut().zip(line.as_chunks::<3>().0) {
                    *pixel = [r, g, b, 255];
                }
            }
            Conversion::Memoised(memo) => {
                for pixel in pixels {
                    for sample in raw.iter_mut() {
                        *sample = reader.take(bits);
                    }
                    *pixel = memo.colour(raw, &colour_of)?;
                }
            }
        }
    }
    Ok(())
}

/// The colours samples came to, each held in the slot its samples hash to.
struct Memo {
    components: usize,
    /// Each slot's samples, `components` a slot.
    samples: Vec<u32>,
    /// Each slot's colour, once it holds one.
    colours: Vec<Option<[u8; 4]>>,
}

impl Memo {
    /// A memo for an image of `pixels` pixels of `components` samples: a slot
    /// for every four pixels, up to [`MEMO_SLOTS`].
    fn new(components: usize, pixels: u64) -> Result<Self, PdfError> {
        let slots = usize::try_from(pixels / 4)
            .unwrap_or(MEMO_SLOTS)
            .clamp(1, MEMO_SLOTS)
            .next_power_of_two();
        Ok(Self {
            components,
            samples: fallible::filled(slots * components, 0).ok_or(PdfError::OutOfMemory)?,
            colours: fallible::filled(slots, None).ok_or(PdfError::OutOfMemory)?,
        })
    }

    /// The colour samples `raw` come to: the one held where they were met
    /// last in their slot, and otherwise `convert`'s, held there in place of
    /// what it held.
    fn colour(
        &mut self,
        raw: &[u32],
        convert: &impl Fn(&[u32]) -> Result<[u8; 4], PdfError>,
    ) -> Result<[u8; 4], PdfError> {
        let slot = slot_of(raw) & (self.colours.len() - 1);
        let start = slot * self.components;
        let (Some(held), Some(colour)) = (
            self.samples.get_mut(start..start + self.components),
            self.colours.get_mut(slot),
        ) else {
            return convert(raw);
        };
        if let Some(known) = *colour {
            if *held == *raw {
                return Ok(known);
            }
        }
        let converted = convert(raw)?;
        for (kept, &sample) in held.iter_mut().zip(raw) {
            *kept = sample;
        }
        *colour = Some(converted);
        Ok(converted)
    }
}

/// Where samples `raw` fall among a memo's slots, before its size is taken:
/// the high bits of a multiplicative hash.
fn slot_of(raw: &[u32]) -> usize {
    let hash = raw.iter().fold(0u64, |hash, &sample| {
        (hash ^ u64::from(sample)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
    });
    usize::try_from(hash >> 40).unwrap_or(0)
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
    document: &Document,
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
    if decoded.pixels().len() != rgba.len() {
        return Err(PdfError::Unsupported);
    }
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
            if let (Some(pixel), Some(out)) = (source.get(from..from + 4), rgba.get_mut(to..to + 4))
            {
                out.copy_from_slice(pixel);
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
    document: &Document,
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
    document: &Document,
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
    sampled(
        document,
        dict,
        decoded,
        (&Space::Gray, (mask_width, mask_height)),
        &mut grey,
        None,
    )?;
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

/// An explicit stencil mask into `rgba`'s alpha: clear where the mask does
/// not paint, sampled to the image's size.
fn stencil_mask(
    document: &Document,
    dict: &Dict,
    decoded: &Decoded,
    (width, height): (u32, u32),
    rgba: &mut [u8],
) -> Result<(), PdfError> {
    let mask_width = side(document.get(dict, b"Width")?.as_int())?;
    let mask_height = side(document.get(dict, b"Height")?.as_int())?;
    let paints = mask_bits(document, dict, decoded, (mask_width, mask_height))?;
    let columns = nearest_columns(width, mask_width)?;
    for y in 0..height {
        let my = nearest(y, height, mask_height);
        for (x, &mx) in columns.iter().enumerate() {
            if !paints[my * mask_width as usize + mx] {
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
    document: &Document,
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
    document: &Document,
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

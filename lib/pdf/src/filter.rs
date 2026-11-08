//! Stream filters (ISO 32000-2 §7.4): what a stream's bytes are decoded
//! through, every stage's output charged to the document's budget.

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::cell::Cell;

use tairix_collections::try_box;
use tairix_compress::inflate::Inflater;
use tairix_util::fallible;

use crate::lex::{hex_value, is_white};
use crate::limits::MAX_FILTERS;
use crate::object::{Dict, Object};
use crate::PdfError;

/// An image codec a filter chain ends in, which the image stage decodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageCodec {
    /// `DCTDecode`: JPEG.
    Dct,
    /// `JPXDecode`: JPEG 2000.
    Jpx,
    /// `CCITTFaxDecode`: Group 3 or 4 facsimile.
    CcittFax,
    /// `JBIG2Decode`.
    Jbig2,
}

/// What a stream decoded to.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// Its bytes, decoded through every filter but an image codec.
    pub bytes: Vec<u8>,
    /// The image codec its chain ends in, its bytes still in that codec.
    pub image: Option<ImageCodec>,
    /// Whether a filter met damaged data and stopped, what it decoded kept.
    pub damaged: bool,
}

/// One filter of a chain.
enum Filter {
    Flate,
    Lzw,
    AsciiHex,
    Ascii85,
    RunLength,
    /// `Crypt`: its crypt filter is applied with decryption, so here it
    /// passes the bytes on.
    Crypt,
    Image(ImageCodec),
}

impl Filter {
    /// The filter `name` names, its abbreviation in an inline image among
    /// them.
    fn named(name: &[u8]) -> Result<Self, PdfError> {
        Ok(match name {
            b"FlateDecode" | b"Fl" => Self::Flate,
            b"LZWDecode" | b"LZW" => Self::Lzw,
            b"ASCIIHexDecode" | b"AHx" => Self::AsciiHex,
            b"ASCII85Decode" | b"A85" => Self::Ascii85,
            b"RunLengthDecode" | b"RL" => Self::RunLength,
            b"Crypt" => Self::Crypt,
            b"DCTDecode" | b"DCT" => Self::Image(ImageCodec::Dct),
            b"JPXDecode" => Self::Image(ImageCodec::Jpx),
            b"CCITTFaxDecode" | b"CCF" => Self::Image(ImageCodec::CcittFax),
            b"JBIG2Decode" => Self::Image(ImageCodec::Jbig2),
            _ => return Err(PdfError::Unsupported),
        })
    }
}

/// One filter of a chain: its name, and its parameters where it has any.
pub(crate) type Link<'o> = (&'o [u8], Option<&'o Dict>);

/// The filters `filter` names and the parameters `params` gives each, as a
/// stream's or an inline image's dictionary spells them: one name and one
/// dictionary, or arrays of each.
///
/// # Errors
///
/// [`PdfError::Malformed`] for entries of the wrong kind, and
/// [`PdfError::TooComplex`] past [`MAX_FILTERS`].
pub(crate) fn chain<'o>(
    filter: Option<&'o Object>,
    params: Option<&'o Object>,
) -> Result<Vec<Link<'o>>, PdfError> {
    let names: Vec<&[u8]> = match filter {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Name(name)) => alloc::vec![name.as_bytes()],
        Some(Object::Array(items)) if items.len() <= MAX_FILTERS => items
            .iter()
            .map(|item| item.as_name().ok_or(PdfError::Malformed))
            .collect::<Result<_, _>>()?,
        Some(Object::Array(_)) => return Err(PdfError::TooComplex),
        Some(_) => return Err(PdfError::Malformed),
    };
    let param = |index: usize| -> Option<&'o Dict> {
        match params {
            Some(Object::Dict(dict)) if index == 0 => Some(dict),
            Some(Object::Array(items)) => items.get(index).and_then(Object::as_dict),
            _ => None,
        }
    };
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, param(index)))
        .collect())
}

/// `data` decoded through `chain`, each stage's output charged to `budget`.
///
/// # Errors
///
/// [`PdfError::Unsupported`] for a filter this reader does not know,
/// [`PdfError::Malformed`] for data an ASCII or run-length filter or a
/// predictor refuses, [`PdfError::TooComplex`] past the budget, and
/// [`PdfError::OutOfMemory`]. Damaged Flate or LZW data is not refused: what
/// decoded before the damage is answered, marked damaged.
pub(crate) fn decode(
    data: &[u8],
    chain: &[Link<'_>],
    budget: &Cell<u64>,
) -> Result<Decoded, PdfError> {
    let mut bytes: Cow<'_, [u8]> = Cow::Borrowed(data);
    let mut damaged = false;
    let mut image = None;
    for (index, &(name, params)) in chain.iter().enumerate() {
        let filter = Filter::named(name)?;
        let stage = match filter {
            Filter::Flate => {
                let (out, cut) = inflate(&bytes, budget)?;
                damaged |= cut;
                predict(out, params, budget)?
            }
            Filter::Lzw => {
                let early = params
                    .and_then(|params| params.get(b"EarlyChange"))
                    .and_then(Object::as_int)
                    != Some(0);
                let (out, cut) = lzw(&bytes, early, budget)?;
                damaged |= cut;
                predict(out, params, budget)?
            }
            Filter::AsciiHex => ascii_hex(&bytes, budget)?,
            Filter::Ascii85 => ascii85(&bytes, budget)?,
            Filter::RunLength => run_length(&bytes, budget)?,
            Filter::Crypt => continue,
            Filter::Image(codec) => {
                // An image codec ends the chain: anything after it would
                // decode pixels, which no PDF filter does.
                if index + 1 != chain.len() {
                    return Err(PdfError::Unsupported);
                }
                image = Some(codec);
                break;
            }
        };
        bytes = Cow::Owned(stage);
    }
    let bytes = match bytes {
        Cow::Owned(bytes) => bytes,
        Cow::Borrowed(bytes) => copy(bytes, budget)?,
    };
    Ok(Decoded {
        bytes,
        image,
        damaged,
    })
}

/// Charge `count` bytes of output to `budget`.
pub(crate) fn charge(budget: &Cell<u64>, count: usize) -> Result<(), PdfError> {
    let count = u64::try_from(count).map_err(|_| PdfError::TooComplex)?;
    let left = budget
        .get()
        .checked_sub(count)
        .ok_or(PdfError::TooComplex)?;
    budget.set(left);
    Ok(())
}

/// `data` copied, charged to `budget`.
fn copy(data: &[u8], budget: &Cell<u64>) -> Result<Vec<u8>, PdfError> {
    charge(budget, data.len())?;
    fallible::copied(data).ok_or(PdfError::OutOfMemory)
}

/// `bytes` pushed onto `out`, charged to `budget`.
fn extend(out: &mut Vec<u8>, bytes: &[u8], budget: &Cell<u64>) -> Result<(), PdfError> {
    charge(budget, bytes.len())?;
    if !fallible::reserve(out, bytes.len()) {
        return Err(PdfError::OutOfMemory);
    }
    out.extend_from_slice(bytes);
    Ok(())
}

/// Flate: DEFLATE behind a zlib header, the header skipped where it is valid
/// and the stream read as raw DEFLATE where it is not, and the Adler-32
/// trailer not insisted on, as every reader of real files does. Answers the
/// bytes and whether the stream broke off or went bad, what came before kept.
fn inflate(data: &[u8], budget: &Cell<u64>) -> Result<(Vec<u8>, bool), PdfError> {
    let body = match data {
        [cmf, flg, rest @ ..]
            if cmf & 0x0f == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0 =>
        {
            rest
        }
        _ => data,
    };
    let mut inflater = try_box(Inflater::new())?;
    let mut out = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut src = body;
    loop {
        match inflater.inflate(src, &mut chunk) {
            Ok(progress) => {
                extend(&mut out, &chunk[..progress.produced], budget)?;
                src = &src[progress.consumed..];
                if progress.finished {
                    return Ok((out, false));
                }
                if progress.consumed == 0 && progress.produced == 0 {
                    return Ok((out, true));
                }
            }
            Err(_) => return Ok((out, true)),
        }
    }
}

/// LZW in the dialect TIFF and PDF share. Answers the bytes and whether the
/// stream broke off or went bad, what came before kept.
fn lzw(data: &[u8], early: bool, budget: &Cell<u64>) -> Result<(Vec<u8>, bool), PdfError> {
    let mut out = Vec::new();
    let mut refused = None;
    let expanded = tairix_image::lzw::expand_msb(data, early, &mut |run| match extend(
        &mut out, run, budget,
    ) {
        Ok(()) => true,
        Err(error) => {
            refused = Some(error);
            false
        }
    });
    if let Some(error) = refused {
        return Err(error);
    }
    match expanded {
        Ok(()) => Ok((out, false)),
        Err(tairix_image::DecodeError::OutOfMemory) => Err(PdfError::OutOfMemory),
        Err(_) => Ok((out, true)),
    }
}

/// The predictor `params` names undone over `data`: none, TIFF's horizontal
/// differencing, or PNG's per-row filters.
fn predict(data: Vec<u8>, params: Option<&Dict>, budget: &Cell<u64>) -> Result<Vec<u8>, PdfError> {
    let Some(params) = params else {
        return Ok(data);
    };
    let int = |key: &[u8], default: i64| {
        params
            .get(key)
            .map_or(Some(default), Object::as_int)
            .ok_or(PdfError::Malformed)
    };
    let predictor = int(b"Predictor", 1)?;
    if predictor == 1 {
        return Ok(data);
    }
    let colors = usize::try_from(int(b"Colors", 1)?).map_err(|_| PdfError::Malformed)?;
    let bits = usize::try_from(int(b"BitsPerComponent", 8)?).map_err(|_| PdfError::Malformed)?;
    let columns = usize::try_from(int(b"Columns", 1)?).map_err(|_| PdfError::Malformed)?;
    if !(1..=32).contains(&colors) || !matches!(bits, 1 | 2 | 4 | 8 | 16) || columns == 0 {
        return Err(PdfError::Malformed);
    }
    // A row no address can span is damage, not a large image.
    let row_bits = colors
        .checked_mul(bits)
        .and_then(|bits| bits.checked_mul(columns))
        .ok_or(PdfError::Malformed)?;
    let row = row_bits.div_ceil(8);
    let pixel = (colors * bits).div_ceil(8);
    match predictor {
        2 => tiff_predict(data, (row, pixel, bits, colors)),
        10..=15 => png_predict(&data, row, pixel, budget),
        _ => Err(PdfError::Unsupported),
    }
}

/// TIFF predictor 2 undone in place: each sample the sum of itself and the
/// same component of the pixel before it, at 8 or 16 bits.
fn tiff_predict(
    mut data: Vec<u8>,
    (row, pixel, bits, colors): (usize, usize, usize, usize),
) -> Result<Vec<u8>, PdfError> {
    match bits {
        8 => {
            for line in data.chunks_mut(row) {
                for at in pixel..line.len() {
                    line[at] = line[at].wrapping_add(line[at - pixel]);
                }
            }
        }
        16 => {
            let step = 2 * colors;
            for line in data.chunks_mut(row) {
                let mut at = step;
                while at + 1 < line.len() {
                    let before = u16::from_be_bytes([line[at - step], line[at - step + 1]]);
                    let here = u16::from_be_bytes([line[at], line[at + 1]]);
                    line[at..at + 2].copy_from_slice(&here.wrapping_add(before).to_be_bytes());
                    at += 2;
                }
            }
        }
        _ => return Err(PdfError::Unsupported),
    }
    Ok(data)
}

/// The PNG predictors undone: each row led by its own filter byte, which may
/// differ from the predictor the parameters name.
fn png_predict(
    data: &[u8],
    row: usize,
    pixel: usize,
    budget: &Cell<u64>,
) -> Result<Vec<u8>, PdfError> {
    let rows = data.len() / (row + 1);
    let size = rows.checked_mul(row).ok_or(PdfError::TooComplex)?;
    charge(budget, size)?;
    let mut out = fallible::zeroed(size).ok_or(PdfError::OutOfMemory)?;
    for index in 0..rows {
        let source = &data[index * (row + 1)..(index + 1) * (row + 1)];
        let (filter, line) = (source[0], &source[1..]);
        let (done, here) = out.split_at_mut(index * row);
        let above = done
            .get(done.len().saturating_sub(row)..)
            .filter(|_| index > 0);
        let here = &mut here[..row];
        for at in 0..row {
            let left = if at >= pixel { here[at - pixel] } else { 0 };
            let up = above.map_or(0, |above| above[at]);
            let corner = if at >= pixel {
                above.map_or(0, |above| above[at - pixel])
            } else {
                0
            };
            let predicted = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => left.midpoint(up),
                4 => paeth(left, up, corner),
                _ => return Err(PdfError::Malformed),
            };
            here[at] = line[at].wrapping_add(predicted);
        }
    }
    Ok(out)
}

/// The Paeth predictor of PNG.
fn paeth(left: u8, up: u8, corner: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(up) - i16::from(corner);
    let (to_left, to_up, to_corner) = (
        (estimate - i16::from(left)).abs(),
        (estimate - i16::from(up)).abs(),
        (estimate - i16::from(corner)).abs(),
    );
    if to_left <= to_up && to_left <= to_corner {
        left
    } else if to_up <= to_corner {
        up
    } else {
        corner
    }
}

/// `ASCIIHex`: pairs of hexadecimal digits up to `>`, white space skipped and
/// an odd final digit its high half.
fn ascii_hex(data: &[u8], budget: &Cell<u64>) -> Result<Vec<u8>, PdfError> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for &byte in data {
        if byte == b'>' {
            break;
        }
        if is_white(byte) {
            continue;
        }
        let value = hex_value(byte).ok_or(PdfError::Malformed)?;
        match high.take() {
            Some(high) => extend(&mut out, &[high << 4 | value], budget)?,
            None => high = Some(value),
        }
    }
    if let Some(high) = high {
        extend(&mut out, &[high << 4], budget)?;
    }
    Ok(out)
}

/// ASCII85: groups of five characters from `!` to `u` for four bytes, `z`
/// for four zeros, up to `~>`, a short final group its leading bytes.
fn ascii85(data: &[u8], budget: &Cell<u64>) -> Result<Vec<u8>, PdfError> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut have = 0usize;
    // A leading `<~` is not part of the data.
    let data = data.strip_prefix(b"<~").unwrap_or(data);
    for &byte in data {
        match byte {
            b'~' => break,
            b'z' if have == 0 => extend(&mut out, &[0; 4], budget)?,
            b'!'..=b'u' => {
                group[have] = byte - b'!';
                have += 1;
                if have == 5 {
                    extend(&mut out, &a85_group(group)?, budget)?;
                    have = 0;
                }
            }
            _ if is_white(byte) => {}
            _ => return Err(PdfError::Malformed),
        }
    }
    match have {
        0 => {}
        1 => return Err(PdfError::Malformed),
        _ => {
            for slot in &mut group[have..] {
                *slot = b'u' - b'!';
            }
            let bytes = a85_group(group)?;
            extend(&mut out, &bytes[..have - 1], budget)?;
        }
    }
    Ok(out)
}

/// The four bytes a group of five base-85 digits spells.
fn a85_group(group: [u8; 5]) -> Result<[u8; 4], PdfError> {
    let mut value = 0u64;
    for digit in group {
        value = value * 85 + u64::from(digit);
    }
    u32::try_from(value)
        .map(u32::to_be_bytes)
        .map_err(|_| PdfError::Malformed)
}

/// `RunLength`: a length byte, then that many plus one literal bytes below 128,
/// a byte repeated 257 less it times above it, and 128 the end.
fn run_length(data: &[u8], budget: &Cell<u64>) -> Result<Vec<u8>, PdfError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(&length) = data.get(at) {
        at += 1;
        match length {
            128 => break,
            0..=127 => {
                let count = usize::from(length) + 1;
                let literal = data.get(at..at + count).ok_or(PdfError::Malformed)?;
                extend(&mut out, literal, budget)?;
                at += count;
            }
            _ => {
                let &byte = data.get(at).ok_or(PdfError::Malformed)?;
                at += 1;
                let count = 257 - usize::from(length);
                extend(&mut out, &[byte; 128][..count], budget)?;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;

//! A complete, fail-closed WEBP container decoder (WebP Container
//! Specification).
//!
//! A WEBP file is a RIFF form holding one or both of the two bitstreams
//! [`crate::vp8`] and [`crate::vp8l`] decode, plus the container's own
//! alpha plane and animation. Two forms exist: the simple one, a bare
//! `VP8 ` or `VP8L` chunk, and the extended one, a `VP8X` header declaring
//! a canvas over an optional alpha chunk and either one still bitstream or a
//! chain of `ANMF` frames.
//!
//! # Five readings the specification leaves to the decoder
//!
//! **The canvas is authoritative and a disagreeing frame is refused.** A
//! `VP8X` canvas is a 24-bit declaration while a bitstream carries its own
//! 14-bit one, so the two can disagree; reconciling them would mean
//! cropping, padding, or scaling, none of which either declaration asks
//! for. A simple-format file has no container geometry, so there the
//! bitstream's own size *is* the canvas.
//!
//! **Nothing is sized from an `ANMF`'s own declaration.** Its frame extent
//! is checked to lie inside the canvas, which allocates nothing, and the
//! frame is then decoded at the size its payload declares and refused
//! unless the two agree.
//!
//! **Which kind a file is comes from the file.** One carrying `ANIM` is an
//! animation; one without is a still picture, because there is no loop count
//! to report and answering "for ever" would fabricate a declaration.
//!
//! **The canvas clears and disposes to fully transparent.** The
//! specification makes `ANIM`'s background colour explicitly optional, and a
//! straight-alpha decoder's job is to carry a file's transparency out to its
//! consumer rather than pre-flatten it against a colour the viewer will draw
//! its own backdrop behind.
//!
//! **`VP8X`'s alpha and metadata flags are hints; the chunks present are the
//! fact.** They say what a file "contains", and the decode is driven by the
//! chunks actually found — so a set alpha flag with no alpha chunk decodes,
//! and an alpha chunk with a clear flag decodes, rather than either being
//! refused over a disagreement that costs nothing. The **animation** flag is
//! not a hint: it is what says whether the file is an animation at all, so it
//! and the chunks must agree. The reserved bits and reserved field values are
//! refused either way.
//!
//! Colour profiles and metadata are read past: `ICCP`, `EXIF`, and `XMP `
//! are skipped like any unknown chunk, because nothing in this crate
//! colour-manages and the output is RGBA8 in the file's own primaries.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_raster::{RowOrder, RowReducer};
use tairix_util::fallible;

use crate::frames::{Animation, FrameSource};
use crate::{DecodeError, DecodeLimits, FitBox, RasterImage, MAX_ANIMATION_FRAMES, RGBA_BYTES};

/// The form identifiers a WEBP file opens with, four bytes apart.
pub(crate) const RIFF_MAGIC: [u8; 4] = *b"RIFF";
pub(crate) const WEBP_FORM: [u8; 4] = *b"WEBP";

/// Where the form identifier sits: after `RIFF` and the size of everything
/// that follows it.
const FORM_AT: usize = 8;

/// Bytes a chunk header occupies: its identifier and its payload size.
const CHUNK_HEADER: usize = 8;

/// The chunk identifiers the container defines.
const VP8_LOSSY: [u8; 4] = *b"VP8 ";
const VP8_LOSSLESS: [u8; 4] = *b"VP8L";
const EXTENDED: [u8; 4] = *b"VP8X";
const ALPHA: [u8; 4] = *b"ALPH";
const ANIMATION: [u8; 4] = *b"ANIM";
const ANIMATION_FRAME: [u8; 4] = *b"ANMF";

/// Bytes the extended header occupies: a flag word and two 24-bit
/// dimensions.
const EXTENDED_LEN: usize = 10;

/// Bytes the animation header occupies: a background colour and a loop
/// count.
const ANIMATION_LEN: usize = 6;

/// Bytes an animation frame's own header occupies before its payload.
const FRAME_HEADER: usize = 16;

/// Bytes an alpha chunk spends on its method and filter declaration.
const ALPHA_HEADER: usize = 1;

/// The bits of an `ALPH` declaration naming how its plane is stored, and the
/// value naming a lossless stream.
const ALPHA_METHOD: u8 = 0x03;
const ALPHA_LOSSLESS: u8 = 1;

/// The extended header's flag bits this decoder reads. Every other bit is
/// reserved and refused.
const FLAG_ANIMATION: u8 = 0x02;
const FLAG_RESERVED: u8 = 0x01;

/// The widest canvas the extended header can declare, as a 24-bit
/// dimension plus one.
const MAX_CANVAS: u32 = 1 << 24;

/// Whether `bytes` opens with the WEBP form identifiers.
///
/// Unlike every other format here the signature is in two parts with the
/// RIFF size between them, so it cannot be a prefix comparison against one
/// constant.
pub(crate) fn has_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(&RIFF_MAGIC)
        && bytes
            .get(FORM_AT..FORM_AT + WEBP_FORM.len())
            .is_some_and(|form| form == WEBP_FORM)
}

/// A rectangle of the canvas.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Which bitstream a frame carries.
#[derive(Copy, Clone)]
enum Bitstream<'a> {
    Lossy(&'a [u8]),
    Lossless(&'a [u8]),
}

impl Bitstream<'_> {
    fn geometry(self) -> Result<(u32, u32), DecodeError> {
        match self {
            Self::Lossy(bytes) => crate::vp8::probe(bytes),
            Self::Lossless(bytes) => crate::vp8l::probe(bytes),
        }
    }

    fn decode(self, limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
        match self {
            Self::Lossy(bytes) => crate::vp8::decode(bytes, limits),
            Self::Lossless(bytes) => crate::vp8l::decode(bytes, limits),
        }
    }
}

/// One picture the container holds: a bitstream and, for a lossy one, the
/// alpha plane beside it.
#[derive(Copy, Clone)]
struct Picture<'a> {
    alpha: Option<&'a [u8]>,
    bitstream: Bitstream<'a>,
}

impl<'a> Picture<'a> {
    /// Read the `ALPH` and bitstream chunks one picture is made of.
    fn read(chunks: &mut Chunks<'a>, first: Chunk<'a>) -> Result<Self, DecodeError> {
        let mut alpha = None;
        let mut chunk = Some(first);
        while let Some(current) = chunk {
            match current.id {
                ALPHA => {
                    if alpha.is_some() {
                        return Err(DecodeError::WebpInvalidChunkLayout);
                    }
                    alpha = Some(current.payload);
                }
                VP8_LOSSY => {
                    return Ok(Self {
                        alpha,
                        bitstream: Bitstream::Lossy(current.payload),
                    })
                }
                VP8_LOSSLESS => {
                    // A lossless stream carries its own alpha channel, so an
                    // alpha chunk beside one is two answers to one question.
                    if alpha.is_some() {
                        return Err(DecodeError::WebpInvalidChunkLayout);
                    }
                    return Ok(Self {
                        alpha,
                        bitstream: Bitstream::Lossless(current.payload),
                    });
                }
                _ => {}
            }
            chunk = chunks.next()?;
        }
        Err(DecodeError::WebpInvalidChunkLayout)
    }

    fn decode(self, limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
        let (width, height) = self.bitstream.geometry()?;
        let mut image = self.bitstream.decode(limits)?;
        if let Some(chunk) = self.alpha {
            let mut alphas = AlphaRows::open(chunk, width, height)?;
            let line = (width as usize).max(1) * RGBA_BYTES;
            for pixels in image.pixels_mut().chunks_exact_mut(line) {
                set_alpha(pixels, alphas.next_row()?);
            }
        }
        Ok(image)
    }

    /// Hand the picture's RGBA8 rows to `row` top first, its alpha plane,
    /// where it has one, applied to each.
    fn stream_rows(
        self,
        limits: &DecodeLimits,
        mut row: impl FnMut(&[u8]) -> Result<(), DecodeError>,
    ) -> Result<(), DecodeError> {
        let (width, height) = self.bitstream.geometry()?;
        let mut rows = self
            .alpha
            .map(|chunk| AlphaRows::open(chunk, width, height))
            .transpose()?;
        let held_len = if rows.is_some() {
            width as usize * RGBA_BYTES
        } else {
            0
        };
        let mut held = fallible::filled(held_len, 0u8).ok_or(DecodeError::OutOfMemory)?;
        let mut with_alpha = |line: &[u8]| -> Result<(), DecodeError> {
            let Some(alphas) = rows.as_mut() else {
                return row(line);
            };
            let alphas = alphas.next_row()?;
            held.get_mut(..line.len())
                .ok_or(DecodeError::WebpAlphaGeometryMismatch)?
                .copy_from_slice(line);
            set_alpha(&mut held, alphas);
            row(&held)
        };
        match self.bitstream {
            Bitstream::Lossy(bytes) => crate::vp8::decode_rows(bytes, limits, &mut with_alpha),
            Bitstream::Lossless(bytes) => crate::vp8l::decode_rows(bytes, limits, &mut with_alpha),
        }
    }
}

/// One animation frame: where it goes, how long it shows, and how it meets
/// what is already on the canvas.
struct Frame {
    rect: Rect,
    duration_ms: u32,
    blend: bool,
    dispose: bool,
    /// Where the frame's picture chunks lie in the whole document, so a
    /// chain can name them without holding the bytes it walks. Re-reading
    /// two chunk headers costs nothing beside decoding the frame they
    /// introduce.
    picture: Range<usize>,
}

/// A chunk of the RIFF form.
#[derive(Copy, Clone)]
struct Chunk<'a> {
    id: [u8; 4],
    payload: &'a [u8],
    /// Where [`Self::payload`] begins in the whole document, so a chunk can
    /// be named to a later call rather than held.
    at: usize,
}

/// The chunks of one RIFF region, walked in order.
struct Chunks<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Where [`Self::bytes`] begins in the whole document.
    base: usize,
}

impl<'a> Chunks<'a> {
    /// Walk the chunk region of a whole file, refusing one whose declared
    /// RIFF region is not present.
    fn open(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        if !has_signature(bytes) {
            return Err(DecodeError::WebpBadSignature);
        }
        let declared = usize::try_from(
            crate::le_u32(bytes, RIFF_MAGIC.len()).ok_or(DecodeError::WebpTruncated)?,
        )
        .unwrap_or(usize::MAX);
        // The size covers the form identifier and every chunk after it.
        let end = declared
            .checked_add(FORM_AT)
            .ok_or(DecodeError::WebpTruncated)?;
        if declared < WEBP_FORM.len() || end > bytes.len() {
            return Err(DecodeError::WebpTruncated);
        }
        Ok(Self {
            bytes: bytes.get(..end).ok_or(DecodeError::WebpTruncated)?,
            pos: FORM_AT + WEBP_FORM.len(),
            base: 0,
        })
    }

    /// Walk a chunk's own payload as a chunk region, which is what an
    /// animation frame's payload is. `base` is where that payload begins in
    /// the whole document.
    fn nested(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            pos: 0,
            base,
        }
    }

    fn next(&mut self) -> Result<Option<Chunk<'a>>, DecodeError> {
        if self.pos >= self.bytes.len() {
            return Ok(None);
        }
        let header = self
            .bytes
            .get(self.pos..self.pos + CHUNK_HEADER)
            .ok_or(DecodeError::WebpTruncated)?;
        let id = <[u8; 4]>::try_from(&header[..4]).map_err(|_| DecodeError::WebpTruncated)?;
        let size = usize::try_from(crate::le_u32(header, 4).ok_or(DecodeError::WebpTruncated)?)
            .unwrap_or(usize::MAX);
        let start = self.pos + CHUNK_HEADER;
        let end = start.checked_add(size).ok_or(DecodeError::WebpTruncated)?;
        let payload = self
            .bytes
            .get(start..end)
            .ok_or(DecodeError::WebpTruncated)?;
        // A chunk's payload is padded to an even length, and the pad byte is
        // outside the payload the size declares.
        self.pos = end
            .checked_add(size % 2)
            .ok_or(DecodeError::WebpTruncated)?;
        Ok(Some(Chunk {
            id,
            payload,
            at: self.base.saturating_add(start),
        }))
    }
}

/// What the container declares itself to be.
enum Layout<'a> {
    Still {
        canvas: Option<(u32, u32)>,
        picture: Picture<'a>,
    },
    Animation {
        canvas: (u32, u32),
        loop_count: Option<u32>,
        frames: Vec<Frame>,
    },
}

/// Read the canvas an extended header declares.
fn read_canvas(payload: &[u8]) -> Result<(u32, u32), DecodeError> {
    let head = payload
        .get(..EXTENDED_LEN)
        .ok_or(DecodeError::WebpTruncated)?;
    if head[0] & FLAG_RESERVED != 0 || head[0] >> 6 != 0 || head[1..4] != [0, 0, 0] {
        return Err(DecodeError::WebpInvalidCanvas);
    }
    let width = le_u24(head, 4) + 1;
    let height = le_u24(head, 7) + 1;
    if width > MAX_CANVAS || height > MAX_CANVAS {
        return Err(DecodeError::WebpInvalidCanvas);
    }
    Ok((width, height))
}

/// A 24-bit little-endian field, which only this container uses.
fn le_u24(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 3).map_or(0, |field| {
        u32::from(field[0]) | (u32::from(field[1]) << 8) | (u32::from(field[2]) << 16)
    })
}

/// Read one animation frame's header and its payload's chunks.
fn read_frame(payload: &[u8], at: usize, canvas: (u32, u32)) -> Result<Frame, DecodeError> {
    let head = payload
        .get(..FRAME_HEADER)
        .ok_or(DecodeError::WebpTruncated)?;
    // The offsets are in units of two pixels, so an odd placement cannot be
    // expressed and none is read.
    let rect = Rect {
        x: le_u24(head, 0) * 2,
        y: le_u24(head, 3) * 2,
        width: le_u24(head, 6) + 1,
        height: le_u24(head, 9) + 1,
    };
    if head[15] >> 2 != 0 {
        return Err(DecodeError::WebpInvalidChunkLayout);
    }
    let right = rect
        .x
        .checked_add(rect.width)
        .ok_or(DecodeError::WebpFrameOutsideCanvas)?;
    let bottom = rect
        .y
        .checked_add(rect.height)
        .ok_or(DecodeError::WebpFrameOutsideCanvas)?;
    if right > canvas.0 || bottom > canvas.1 {
        return Err(DecodeError::WebpFrameOutsideCanvas);
    }
    let picture = at
        .checked_add(FRAME_HEADER)
        .ok_or(DecodeError::WebpTruncated)?
        ..at.checked_add(payload.len())
            .ok_or(DecodeError::WebpTruncated)?;
    // Parsed here and discarded: a frame whose chunk layout will not read
    // refuses the whole container when it opens, as every other structural
    // fault does, rather than only when it is stepped to.
    read_picture(
        payload
            .get(FRAME_HEADER..)
            .ok_or(DecodeError::WebpTruncated)?,
        picture.start,
    )?;
    Ok(Frame {
        rect,
        duration_ms: le_u24(head, 12),
        blend: head[15] & 0x02 == 0,
        dispose: head[15] & 0x01 != 0,
        picture,
    })
}

/// Read the picture the chunk region `region` describes, `base` being where
/// that region begins in the whole document.
fn read_picture(region: &[u8], base: usize) -> Result<Picture<'_>, DecodeError> {
    let mut chunks = Chunks::nested(region, base);
    let first = chunks.next()?.ok_or(DecodeError::WebpInvalidChunkLayout)?;
    Picture::read(&mut chunks, first)
}

/// Read a file's chunk chain and answer the form it declares.
fn layout(bytes: &[u8]) -> Result<Layout<'_>, DecodeError> {
    let mut chunks = Chunks::open(bytes)?;
    let first = chunks.next()?.ok_or(DecodeError::WebpInvalidChunkLayout)?;
    if first.id != EXTENDED {
        // The simple form is one bitstream chunk and nothing else, so the
        // animation and alpha chunks the extended header defines cannot
        // appear here.
        if first.id == ALPHA || first.id == ANIMATION || first.id == ANIMATION_FRAME {
            return Err(DecodeError::WebpInvalidChunkLayout);
        }
        return Ok(Layout::Still {
            canvas: None,
            picture: Picture::read(&mut chunks, first)?,
        });
    }
    let canvas = read_canvas(first.payload)?;
    let animated = first
        .payload
        .first()
        .is_some_and(|flags| flags & FLAG_ANIMATION != 0);
    let mut loop_count = None;
    let mut frames: Vec<Frame> = Vec::new();
    while let Some(chunk) = chunks.next()? {
        match chunk.id {
            ANIMATION => {
                let head = chunk
                    .payload
                    .get(..ANIMATION_LEN)
                    .ok_or(DecodeError::WebpTruncated)?;
                let declared = crate::le_u16(head, 4).ok_or(DecodeError::WebpTruncated)?;
                loop_count = (declared != 0).then(|| u32::from(declared));
            }
            ANIMATION_FRAME => {
                if u32::try_from(frames.len()).unwrap_or(u32::MAX) >= MAX_ANIMATION_FRAMES {
                    return Err(DecodeError::WebpTooManyFrames);
                }
                if !fallible::reserve(&mut frames, 1) {
                    return Err(DecodeError::OutOfMemory);
                }
                frames.push(read_frame(chunk.payload, chunk.at, canvas)?);
            }
            ALPHA | VP8_LOSSY | VP8_LOSSLESS => {
                if animated {
                    return Err(DecodeError::WebpInvalidChunkLayout);
                }
                return Ok(Layout::Still {
                    canvas: Some(canvas),
                    picture: Picture::read(&mut chunks, chunk)?,
                });
            }
            _ => {}
        }
    }
    if !animated {
        return Err(DecodeError::WebpInvalidChunkLayout);
    }
    if frames.is_empty() {
        return Err(DecodeError::WebpNoFrames);
    }
    Ok(Layout::Animation {
        canvas,
        loop_count,
        frames,
    })
}

/// Undo the filtering an alpha chunk declares on one `row`, given the
/// unfiltered row `above` it. The first row has none, so every method
/// predicts along it; later rows predict from the one above as it says.
fn unfilter_row(row: &mut [u8], above: Option<&[u8]>, method: u8) {
    let vertical = method == 2 && above.is_some();
    let gradient = method == 3 && above.is_some();
    let mut left = above.and_then(|above| above.first().copied()).unwrap_or(0);
    let mut above_left = left;
    for (column, sample) in row.iter_mut().enumerate() {
        let up = above
            .and_then(|above| above.get(column).copied())
            .unwrap_or(0);
        let predicted = if vertical {
            up
        } else if gradient {
            let value = i32::from(left) + i32::from(up) - i32::from(above_left);
            u8::try_from(value.clamp(0, 255)).unwrap_or(0)
        } else if method == 0 {
            0
        } else {
            left
        };
        *sample = sample.wrapping_add(predicted);
        above_left = up;
        left = *sample;
    }
}

/// Where an alpha chunk's samples are read from.
enum AlphaSamples<'a> {
    /// Uncompressed, in place.
    Raw(&'a [u8]),
    /// Its lossless stream's decoded pixels.
    Words(Vec<u32>),
}

/// An alpha chunk's samples a row at a time, each unfiltered against the one
/// above as it is reached, so the plane is never held a second time.
struct AlphaRows<'a> {
    samples: AlphaSamples<'a>,
    width: usize,
    filter: u8,
    next: usize,
    above: Vec<u8>,
    current: Vec<u8>,
}

impl<'a> AlphaRows<'a> {
    /// The rows of `chunk`, the alpha of a `width`×`height` picture.
    fn open(chunk: &'a [u8], width: u32, height: u32) -> Result<Self, DecodeError> {
        let declaration = *chunk.first().ok_or(DecodeError::WebpTruncated)?;
        let method = declaration & ALPHA_METHOD;
        let filter = (declaration >> 2) & 0x03;
        let preprocessing = (declaration >> 4) & 0x03;
        // The pre-processing field is informative — it says what an encoder
        // did, not what a decoder must undo — so it is validated and then not
        // acted on: smoothing the stored values would invent pixels.
        if method > 1 || preprocessing > 1 || declaration >> 6 != 0 {
            return Err(DecodeError::WebpUnsupportedAlpha);
        }
        let body = chunk
            .get(ALPHA_HEADER..)
            .ok_or(DecodeError::WebpTruncated)?;
        let count = usize::try_from(u64::from(width) * u64::from(height))
            .map_err(|_| DecodeError::OutOfMemory)?;
        let samples = if method == 0 {
            AlphaSamples::Raw(
                body.get(..count)
                    .ok_or(DecodeError::WebpAlphaGeometryMismatch)?,
            )
        } else {
            let words = crate::vp8l::decode_alpha(body, width, height)?;
            if words.len() != count {
                return Err(DecodeError::WebpAlphaGeometryMismatch);
            }
            AlphaSamples::Words(words)
        };
        let width = width as usize;
        Ok(Self {
            samples,
            width,
            filter,
            next: 0,
            above: fallible::filled(width, 0u8).ok_or(DecodeError::OutOfMemory)?,
            current: fallible::filled(width, 0u8).ok_or(DecodeError::OutOfMemory)?,
        })
    }

    /// The next row's samples, unfiltered.
    fn next_row(&mut self) -> Result<&[u8], DecodeError> {
        let start = self
            .next
            .checked_mul(self.width)
            .ok_or(DecodeError::WebpAlphaGeometryMismatch)?;
        let end = start + self.width;
        core::mem::swap(&mut self.above, &mut self.current);
        match &self.samples {
            AlphaSamples::Raw(raw) => self.current.copy_from_slice(
                raw.get(start..end)
                    .ok_or(DecodeError::WebpAlphaGeometryMismatch)?,
            ),
            AlphaSamples::Words(words) => {
                let words = words
                    .get(start..end)
                    .ok_or(DecodeError::WebpAlphaGeometryMismatch)?;
                for (sample, &word) in self.current.iter_mut().zip(words) {
                    *sample = crate::vp8l::alpha_sample(word);
                }
            }
        }
        let above = (self.next > 0).then_some(&self.above[..]);
        unfilter_row(&mut self.current, above, self.filter);
        self.next += 1;
        Ok(&self.current)
    }
}

/// Write `alphas` into the alpha channel of the RGBA8 `pixels`.
fn set_alpha(pixels: &mut [u8], alphas: &[u8]) {
    for (pixel, &alpha) in pixels
        .as_chunks_mut::<RGBA_BYTES>()
        .0
        .iter_mut()
        .zip(alphas)
    {
        pixel[3] = alpha;
    }
}

/// Read the canvas a file declares, decoding no pixels.
pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), DecodeError> {
    match layout(bytes)? {
        Layout::Still {
            canvas: Some(canvas),
            picture,
        } => {
            check_canvas(picture.bitstream.geometry()?, canvas)?;
            Ok(canvas)
        }
        Layout::Still {
            canvas: None,
            picture,
        } => picture.bitstream.geometry(),
        Layout::Animation { canvas, .. } => Ok(canvas),
    }
}

/// An upper bound of the bytes a [`decode`] of `bytes` holds at once, read
/// from its chunks and its bitstreams' headers: the frame list the
/// container's walk collects, held twice while it regrows, then the picture
/// — or, for an animation, the canvas and its copy beside the first frame,
/// decoded at its own size.
///
/// # Errors
///
/// What [`decode`] would refuse before decoding: a malformed container or
/// bitstream header, or a size `limits` do not admit.
pub(crate) fn peak_bytes(bytes: &[u8], limits: &DecodeLimits) -> Result<u64, DecodeError> {
    let listed = frame_list_bytes(bytes);
    let held = match layout(bytes)? {
        Layout::Still { canvas, picture } => {
            let geometry = picture.bitstream.geometry()?;
            if let Some(canvas) = canvas {
                check_canvas(geometry, canvas)?;
            }
            limits.check(geometry.0, geometry.1)?;
            picture_peak(picture, geometry)
        }
        Layout::Animation { canvas, frames, .. } => {
            limits.check(canvas.0, canvas.1)?;
            let first = frames.first().ok_or(DecodeError::WebpNoFrames)?;
            let region = bytes
                .get(first.picture.clone())
                .ok_or(DecodeError::WebpTruncated)?;
            let picture = read_picture(region, first.picture.start)?;
            let geometry = picture.bitstream.geometry()?;
            limits.check(geometry.0, geometry.1)?;
            let canvas_bytes = u64::from(canvas.0) * u64::from(canvas.1) * RGBA_BYTES as u64;
            picture_peak(picture, geometry).saturating_add(2 * canvas_bytes)
        }
    };
    Ok(listed.saturating_add(held))
}

/// What the frame list a walk of `bytes` collects can hold at once: a record
/// per frame chunk, twice over while the list regrows by one.
fn frame_list_bytes(bytes: &[u8]) -> u64 {
    let Ok(mut chunks) = Chunks::open(bytes) else {
        return 0;
    };
    let mut frames = 0u64;
    while let Ok(Some(chunk)) = chunks.next() {
        if chunk.id == ANIMATION_FRAME {
            frames += 1;
        }
    }
    frames.saturating_mul(2 * core::mem::size_of::<Frame>() as u64)
}

/// What decoding `picture`, of `width`×`height`, holds at once: its
/// bitstream's own peak, then its alpha plane, decoded beside the picture
/// when the plane is a lossless stream.
fn picture_peak(picture: Picture<'_>, (width, height): (u32, u32)) -> u64 {
    let pixels = u64::from(width) * u64::from(height);
    let image = match picture.bitstream {
        Bitstream::Lossy(_) => crate::vp8::peak_bytes(width, height),
        // The decoded words, then the RGBA picture converted from them.
        Bitstream::Lossless(stream) => crate::vp8l::peak_ceiling(width, height, stream.len())
            .saturating_add(pixels * RGBA_BYTES as u64),
    };
    image.saturating_add(alpha_peak(picture.alpha, width, height))
}

/// What reading an alpha chunk a row at a time holds: its lossless stream's
/// working set where it is one, and the row and the row above it.
fn alpha_peak(chunk: Option<&[u8]>, width: u32, height: u32) -> u64 {
    chunk.map_or(0, |chunk| {
        let lossless = chunk
            .first()
            .is_some_and(|declaration| declaration & ALPHA_METHOD == ALPHA_LOSSLESS);
        let decoded = if lossless {
            crate::vp8l::peak_ceiling(width, height, chunk.len())
        } else {
            0
        };
        decoded.saturating_add(2 * u64::from(width))
    })
}

/// Refuse a picture whose own geometry differs from the rectangle the
/// container gives it.
fn check_canvas(picture: (u32, u32), declared: (u32, u32)) -> Result<(), DecodeError> {
    if picture == declared {
        Ok(())
    } else {
        Err(DecodeError::WebpFrameGeometryMismatch)
    }
}

/// Decode a WEBP file's picture: the still one, or an animation's first
/// composited frame.
pub(crate) fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
    match layout(bytes)? {
        Layout::Still { canvas, picture } => {
            let geometry = picture.bitstream.geometry()?;
            if let Some(canvas) = canvas {
                check_canvas(geometry, canvas)?;
            }
            limits.check(geometry.0, geometry.1)?;
            picture.decode(limits)
        }
        Layout::Animation {
            canvas,
            loop_count,
            frames,
        } => {
            let mut animation = Animation::new(Chain::new(canvas, loop_count, frames, limits)?);
            if !animation.step(bytes)? {
                return Err(DecodeError::WebpNoFrames);
            }
            let pixels =
                fallible::collected(animation.canvas().len(), animation.canvas().iter().copied())
                    .ok_or(DecodeError::OutOfMemory)?;
            Ok(RasterImage::from_parts(canvas.0, canvas.1, pixels))
        }
    }
}

/// Decode a WEBP file's picture no smaller than it must be to cover `fit`: a
/// still picture's rows streamed through a reduction, so the picture is never
/// held, and an animation's first composited frame reduced. A box that does
/// not reduce the picture is [`decode`]. It admits what [`decode`] admits.
pub(crate) fn decode_fitted(
    bytes: &[u8],
    limits: &DecodeLimits,
    fit: FitBox,
) -> Result<RasterImage, DecodeError> {
    let Layout::Still { canvas, picture } = layout(bytes)? else {
        return crate::reduce_held(decode(bytes, limits)?, fit);
    };
    let geometry = picture.bitstream.geometry()?;
    if let Some(canvas) = canvas {
        check_canvas(geometry, canvas)?;
    }
    limits.check(geometry.0, geometry.1)?;
    let target = fit.reduction(geometry.0, geometry.1);
    if target == geometry {
        return picture.decode(limits);
    }
    let mut stream =
        RowReducer::new(geometry, target, RowOrder::TopDown).map_err(crate::reduction_refused)?;
    picture.stream_rows(limits, |line| {
        stream.push_row(line).map_err(crate::reduction_refused)
    })?;
    let pixels = stream.finish().map_err(crate::reduction_refused)?;
    Ok(RasterImage::from_parts(target.0, target.1, pixels))
}

/// An upper bound of the bytes a [`decode_fitted`] of `bytes` to `fit` holds
/// at once: [`peak_bytes`] where the box does not reduce the picture, or an
/// animation's and its reduction; a reduced still picture's bitstream working
/// set, the row it converts and its alpha plane, and the reduction they are
/// fed into.
///
/// # Errors
///
/// What [`decode_fitted`] would refuse before decoding.
pub(crate) fn fitted_peak_bytes(
    bytes: &[u8],
    limits: &DecodeLimits,
    fit: FitBox,
) -> Result<u64, DecodeError> {
    let whole = peak_bytes(bytes, limits)?;
    let Layout::Still { picture, .. } = layout(bytes)? else {
        let canvas = probe(bytes)?;
        let reduced = fit.reduction(canvas.0, canvas.1);
        return Ok(whole.saturating_add(RowReducer::peak_bytes(canvas, reduced)));
    };
    let geometry = picture.bitstream.geometry()?;
    let reduced = fit.reduction(geometry.0, geometry.1);
    if reduced == geometry {
        return Ok(whole);
    }
    let (width, height) = geometry;
    let row = u64::from(width) * RGBA_BYTES as u64;
    let image = match picture.bitstream {
        Bitstream::Lossy(_) => crate::vp8::frame_peak_bytes(width),
        Bitstream::Lossless(stream) => crate::vp8l::peak_ceiling(width, height, stream.len()),
    };
    // The alpha rows, and the row they are applied to.
    let alpha = picture.alpha.map_or(0, |chunk| {
        alpha_peak(Some(chunk), width, height).saturating_add(row)
    });
    Ok([
        frame_list_bytes(bytes),
        image,
        row,
        alpha,
        RowReducer::peak_bytes(geometry, reduced),
    ]
    .into_iter()
    .fold(0, u64::saturating_add))
}

/// What [`open`] found: a still picture's geometry, or an animation ready to
/// step.
pub(crate) enum Opened {
    Still { width: u32, height: u32 },
    Animation(Animation<Chain>),
}

/// Validate a file's structure and prepare whichever kind it is, decoding no
/// pixels.
pub(crate) fn open(bytes: &[u8], limits: &DecodeLimits) -> Result<Opened, DecodeError> {
    match layout(bytes)? {
        Layout::Still { canvas, picture } => {
            let (width, height) = picture.bitstream.geometry()?;
            if let Some(canvas) = canvas {
                check_canvas((width, height), canvas)?;
            }
            Ok(Opened::Still { width, height })
        }
        Layout::Animation {
            canvas,
            loop_count,
            frames,
        } => Ok(Opened::Animation(Animation::new(Chain::new(
            canvas, loop_count, frames, limits,
        )?))),
    }
}

/// An animation's frames, composited onto the canvas the container declares.
pub(crate) struct Chain {
    frames: Vec<Frame>,
    limits: DecodeLimits,
    width: u32,
    height: u32,
    loop_count: Option<u32>,
    /// The composition canvas: straight-alpha RGBA8, canvas-sized.
    canvas: Vec<u8>,
    /// The rectangle the last frame shown asks to be cleared before the
    /// next is drawn.
    pending: Option<Rect>,
}

impl Chain {
    fn new(
        canvas: (u32, u32),
        loop_count: Option<u32>,
        frames: Vec<Frame>,
        limits: &DecodeLimits,
    ) -> Result<Self, DecodeError> {
        limits.check(canvas.0, canvas.1)?;
        let len = usize::try_from(
            u64::from(canvas.0)
                .checked_mul(u64::from(canvas.1))
                .and_then(|pixels| pixels.checked_mul(RGBA_BYTES as u64))
                .ok_or(DecodeError::DimensionsOverflow)?,
        )
        .map_err(|_| DecodeError::DimensionsOverflow)?;
        Ok(Self {
            frames,
            limits: *limits,
            width: canvas.0,
            height: canvas.1,
            loop_count,
            canvas: fallible::filled(len, 0u8).ok_or(DecodeError::OutOfMemory)?,
            pending: None,
        })
    }

    /// The canvas byte range `rect`'s `row`-th row occupies, or `None` when
    /// any of it falls outside the canvas.
    fn row(&self, rect: Rect, row: u32) -> Option<core::ops::Range<usize>> {
        let y = usize::try_from(rect.y.checked_add(row)?).ok()?;
        let x = usize::try_from(rect.x).ok()?;
        let width = usize::try_from(rect.width).ok()?;
        let stride = usize::try_from(self.width).ok()?.checked_mul(RGBA_BYTES)?;
        let start = y
            .checked_mul(stride)?
            .checked_add(x.checked_mul(RGBA_BYTES)?)?;
        let end = start.checked_add(width.checked_mul(RGBA_BYTES)?)?;
        (end <= self.canvas.len()).then_some(start..end)
    }

    /// Clear the rectangle the previous frame asked to be disposed.
    fn dispose(&mut self) {
        let Some(rect) = self.pending.take() else {
            return;
        };
        for row in 0..rect.height {
            if let Some(span) = self.row(rect, row) {
                if let Some(pixels) = self.canvas.get_mut(span) {
                    pixels.fill(0);
                }
            }
        }
    }

    /// Draw a decoded frame into its rectangle, blending or overwriting as
    /// the frame asks.
    fn draw(&mut self, rect: Rect, blend_over: bool, picture: &RasterImage) {
        let stride = usize::try_from(rect.width).unwrap_or(0) * RGBA_BYTES;
        for row in 0..rect.height {
            let Some(span) = self.row(rect, row) else {
                continue;
            };
            let source = usize::try_from(row).unwrap_or(0) * stride;
            let (Some(from), Some(into)) = (
                picture.pixels().get(source..source + stride),
                self.canvas.get_mut(span),
            ) else {
                continue;
            };
            for (target, incoming) in into
                .as_chunks_mut::<RGBA_BYTES>()
                .0
                .iter_mut()
                .zip(from.as_chunks::<RGBA_BYTES>().0)
            {
                *target = if blend_over {
                    crate::picture::over(*target, *incoming)
                } else {
                    *incoming
                };
            }
        }
    }
}

impl FrameSource for Chain {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn count(&self) -> u32 {
        u32::try_from(self.frames.len()).unwrap_or(u32::MAX)
    }

    fn loop_count(&self) -> Option<u32> {
        self.loop_count
    }

    fn canvas(&self) -> &[u8] {
        &self.canvas
    }

    fn advance(&mut self, bytes: &[u8], index: u32) -> Result<u64, DecodeError> {
        self.dispose();
        let frame = self
            .frames
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .ok_or(DecodeError::WebpNoFrames)?;
        let (rect, duration, dispose, blend_over, at) = (
            frame.rect,
            frame.duration_ms,
            frame.dispose,
            frame.blend,
            frame.picture.clone(),
        );
        let region = bytes.get(at.clone()).ok_or(DecodeError::WebpTruncated)?;
        let decoded = read_picture(region, at.start)?.decode(&self.limits)?;
        if decoded.width() != rect.width || decoded.height() != rect.height {
            return Err(DecodeError::WebpFrameGeometryMismatch);
        }
        self.draw(rect, blend_over, &decoded);
        self.pending = dispose.then_some(rect);
        Ok(u64::from(duration) * 1_000_000)
    }

    fn restart(&mut self) {
        self.canvas.fill(0);
        self.pending = None;
    }
}

#[cfg(test)]
#[path = "webp_tests.rs"]
mod tests;

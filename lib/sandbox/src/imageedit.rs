//! The sandboxed decode an image editor opens a document through.
//!
//! A viewer needs what a picture looks like; an editor needs what its file
//! stores — a paletted PNG's indices and palette, and every sprite of a
//! sprite area with its name, mode, palette and mask — at full resolution,
//! so that saving writes back what was read. This is that decode, served by
//! the same worker ([`crate::imagerender::ImageRenderService`]) over the same
//! one upload path ([`crate::imagerender::upload_document`] and
//! [`crate::imagerender::send_document`]), so there is still exactly one way
//! an untrusted file enters.
//!
//! The session is **open**, then **select** one entry at a time, then read
//! its rows in bands no reply of which exceeds [`MAX_FRAME`]; a sprite the
//! decoder cannot read is fetched as its exact bytes instead, so an editor
//! can keep it. The parent side trusts nothing beyond what it checks: every
//! geometry is held to the editing bounds, a palette to its depth, every
//! index to the palette, every mode word to one the decoder reads and to the
//! pixels it describes, and every echoed row range and length exactly.

use alloc::vec::Vec;

use tairix_image::{
    open_native, sniff, DecodeError, DecodeLimits, IndexDepth, NativeDocument, Picture, Pixels,
    Rgba8, SpriteAreaReader, SpriteEntry, SpriteLayout, SpriteMode, SpriteName, SpritePalette,
    Unkept, SPRITE_HEADER_LEN,
};

use crate::host::{Launcher, ParserSandbox, SandboxError, Unbelieved};
use crate::imagerender::{
    encode_error, rows_fitting, DecodeVerdict, Document, DocumentFailure, ReplyFailure, ViewFormat,
    MAX_DOCUMENT_BYTES, MAX_VIEW_DECODE_PIXELS, MAX_VIEW_PROGRESSIVE_COEFFICIENT_BYTES,
};
use crate::proto::MAX_FRAME;
use crate::wire::{Reader, Writer};

/// Largest picture, in pixels, an editor opens: the viewer's own bound,
/// since what a user must be able to open does not change with the purpose.
pub const MAX_EDIT_PIXELS: u64 = MAX_VIEW_DECODE_PIXELS;

/// Longest side, in pixels, an editor opens.
///
/// A containment bound that keeps one row of the widest picture far inside
/// one reply, so rows always travel whole.
pub const MAX_EDIT_SIDE: u32 = 1 << 16;

/// Most entries an edited document may hold: a fixed containment bound on
/// what a small file can make an editor track, far above the icon set of
/// any application.
pub const MAX_EDIT_ENTRIES: u32 = 4096;

/// Edit opcodes, after every other op this worker serves.
const OP_EDIT_OPEN: u8 = 13;
const OP_EDIT_SELECT: u8 = 14;
const OP_EDIT_ROWS: u8 = 15;
const OP_EDIT_KEPT: u8 = 16;
const OP_EDIT_RELEASE: u8 = 17;

/// Edit success reply tags, one per op.
const REPLY_EDIT_OPENED: u8 = 13;
const REPLY_EDIT_SELECTED: u8 = 14;
const REPLY_EDIT_ROWS: u8 = 15;
const REPLY_EDIT_KEPT: u8 = 16;
const REPLY_EDIT_RELEASED: u8 = 17;

/// Fixed overhead of a rows reply: the tag, the echoed range, and the two
/// planes' length prefixes.
const ROWS_REPLY_HEADER: usize = 1 + 4 + 4 + 4 + 4;

/// Fixed overhead of a kept-bytes reply: the tag, the echoed offset, and the
/// chunk's length prefix.
const KEPT_REPLY_HEADER: usize = 1 + 4 + 4;

/// Most kept bytes one reply carries.
const MAX_KEPT_CHUNK: usize = MAX_FRAME - KEPT_REPLY_HEADER;

/// Whether `op` is one of this module's.
pub(crate) const fn is_edit_op(op: u8) -> bool {
    matches!(
        op,
        OP_EDIT_OPEN | OP_EDIT_SELECT | OP_EDIT_ROWS | OP_EDIT_KEPT | OP_EDIT_RELEASE
    )
}

/// Why the service refused an edit request, carried typed over the wire.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EditRefusal {
    /// The request violated its grammar.
    MalformedRequest,
    /// No complete document has been uploaded.
    NoDocument,
    /// The document is not a format this service can open for editing.
    UnsupportedFormat,
    /// The document is malformed.
    MalformedDocument,
    /// No document is open for editing.
    NotOpen,
    /// The document holds no entry at that index.
    NoSuchEntry,
    /// No entry has been selected.
    NoEntry,
    /// Rows were asked for that the entry does not have, or more than one
    /// reply carries.
    RowsOutOfRange,
    /// The picture is larger than an editor opens, or the document holds
    /// more entries than one tracks.
    TooLarge,
    /// The worker could not hold the decode.
    OutOfMemory,
    /// Rows were asked of a kept sprite, or bytes of a picture.
    WrongKind,
}

impl EditRefusal {
    const fn to_wire(self) -> u8 {
        match self {
            Self::MalformedRequest => 1,
            Self::NoDocument => 2,
            Self::UnsupportedFormat => 3,
            Self::MalformedDocument => 4,
            Self::NotOpen => 5,
            Self::NoSuchEntry => 6,
            Self::NoEntry => 7,
            Self::RowsOutOfRange => 8,
            Self::TooLarge => 9,
            Self::OutOfMemory => 10,
            Self::WrongKind => 11,
        }
    }

    const fn from_wire(raw: u8) -> Option<Self> {
        Some(match raw {
            1 => Self::MalformedRequest,
            2 => Self::NoDocument,
            3 => Self::UnsupportedFormat,
            4 => Self::MalformedDocument,
            5 => Self::NotOpen,
            6 => Self::NoSuchEntry,
            7 => Self::NoEntry,
            8 => Self::RowsOutOfRange,
            9 => Self::TooLarge,
            10 => Self::OutOfMemory,
            11 => Self::WrongKind,
            _ => return None,
        })
    }

    /// The refusal a decode's error amounts to.
    const fn of_decode(err: &DecodeError) -> Self {
        match DecodeVerdict::of(err) {
            DecodeVerdict::Unsupported => Self::UnsupportedFormat,
            DecodeVerdict::Damaged => Self::MalformedDocument,
            DecodeVerdict::TooLarge => Self::TooLarge,
            DecodeVerdict::OutOfMemory => Self::OutOfMemory,
        }
    }
}

impl core::fmt::Display for EditRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::MalformedRequest => "malformed edit request",
            Self::NoDocument => "no document was handed over",
            Self::UnsupportedFormat => "not a picture format that can be edited",
            Self::MalformedDocument => "the picture is damaged",
            Self::NotOpen => "no picture is open",
            Self::NoSuchEntry => "no such picture in the document",
            Self::NoEntry => "no picture was chosen",
            Self::RowsOutOfRange => "rows outside the picture",
            Self::TooLarge => "the picture is too large to edit",
            Self::OutOfMemory => "there is not enough memory to open the picture",
            Self::WrongKind => "the picture was asked for in the wrong form",
        })
    }
}

/// Why an edit request failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EditFailure {
    /// The sandbox itself failed.
    Sandbox(SandboxError),
    /// Handing the document over failed.
    Document(DocumentFailure),
    /// The worker refused, for the carried reason.
    Refused(EditRefusal),
    /// The reply broke its grammar or described something impossible: it
    /// cannot be believed, and the caller is handed nothing.
    ReplyMalformed,
}

impl Unbelieved for EditFailure {
    fn unbelieved(&self) -> bool {
        match self {
            Self::ReplyMalformed => true,
            Self::Document(failure) => failure.unbelieved(),
            Self::Sandbox(_) | Self::Refused(_) => false,
        }
    }
}

impl From<DocumentFailure> for EditFailure {
    fn from(failure: DocumentFailure) -> Self {
        Self::Document(failure)
    }
}

impl ReplyFailure for EditFailure {
    const MALFORMED: Self = Self::ReplyMalformed;

    fn refused(code: u8) -> Option<Self> {
        EditRefusal::from_wire(code).map(Self::Refused)
    }
}

impl core::fmt::Display for EditFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Sandbox(inner) => write!(f, "the decoder failed: {inner}"),
            Self::Document(inner) => write!(f, "the document could not be handed over ({inner})"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::ReplyMalformed => f.write_str("the decoder's answer could not be believed"),
        }
    }
}

/// What an opened document is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EditDocument {
    /// The format it was read as.
    pub format: ViewFormat,
    /// Whether it is a sprite area, whose entries are sprites; otherwise it
    /// holds one picture.
    pub sprites: bool,
    /// How many entries it holds.
    pub count: u32,
    /// What the file held that its entries do not.
    pub unkept: Unkept,
}

/// How a picture's pixels arrive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditPixels {
    /// Four straight-alpha bytes a pixel.
    Rgba,
    /// One index a pixel into `palette`, and an alpha plane when `plane`.
    Indexed {
        /// Bits per index.
        depth: IndexDepth,
        /// The colours.
        palette: Vec<Rgba8>,
        /// Whether an alpha value per pixel follows each row.
        plane: bool,
    },
}

impl EditPixels {
    /// Bytes one pixel's sample occupies in a row.
    #[must_use]
    pub const fn sample_bytes(&self) -> usize {
        match self {
            Self::Rgba => 4,
            Self::Indexed { .. } => 1,
        }
    }

    /// Whether each row carries an alpha plane beside its samples.
    #[must_use]
    pub const fn has_plane(&self) -> bool {
        matches!(self, Self::Indexed { plane: true, .. })
    }
}

/// What a sprite says about itself beyond its pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditSprite {
    /// Its name.
    pub name: SpriteName,
    /// Its mode.
    pub mode: SpriteMode,
    /// Whether it carries a mask.
    pub masked: bool,
    /// How its colours are stated.
    pub palette: SpritePalette,
}

/// A picture entry, whose rows [`read_rows`] fetches.
///
/// Only [`select_entry`] makes one, after holding every field to its bounds,
/// so a picture handed to [`read_rows`] is always one its worker described
/// and a band asked for always makes progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditPicture {
    width: u32,
    height: u32,
    pixels: EditPixels,
    sprite: Option<EditSprite>,
}

impl EditPicture {
    /// A `width`×`height` picture whose pixels arrive as `pixels`, a sprite
    /// when `sprite` describes one, or `None` where any of it breaks the edit
    /// bounds or contradicts the rest: a side of zero or past
    /// [`MAX_EDIT_SIDE`], more than [`MAX_EDIT_PIXELS`], a palette empty or
    /// longer than its depth names, or a sprite whose mode lays out other
    /// pixels than these.
    #[must_use]
    pub fn new(
        width: u32,
        height: u32,
        pixels: EditPixels,
        sprite: Option<EditSprite>,
    ) -> Option<Self> {
        let sides = (1..=MAX_EDIT_SIDE).contains(&width) && (1..=MAX_EDIT_SIDE).contains(&height);
        if !sides || u64::from(width) * u64::from(height) > MAX_EDIT_PIXELS {
            return None;
        }
        if let EditPixels::Indexed { depth, palette, .. } = &pixels {
            if palette.is_empty() || palette.len() > depth.colours() {
                return None;
            }
        }
        if let Some(details) = &sprite {
            // The mode must lay out the pixels described, and a paletted
            // sprite's mask is its plane.
            let agrees = match (details.mode.layout(), &pixels) {
                (
                    SpriteLayout::Indexed(depth),
                    EditPixels::Indexed {
                        depth: had, plane, ..
                    },
                ) => depth == *had && *plane == details.masked,
                (SpriteLayout::Direct { .. }, EditPixels::Rgba) => {
                    details.palette == SpritePalette::Implied
                }
                _ => false,
            };
            if !agrees {
                return None;
            }
        }
        Some(Self {
            width,
            height,
            pixels,
            sprite,
        })
    }

    /// Width, in pixels: at least one.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height, in pixels: at least one.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How its pixels arrive.
    #[must_use]
    pub const fn pixels(&self) -> &EditPixels {
        &self.pixels
    }

    /// Its sprite details, exactly when it is an entry of a sprite area.
    #[must_use]
    pub const fn sprite(&self) -> Option<&EditSprite> {
        self.sprite.as_ref()
    }
}

/// Why a sprite could not be read, in the terms a user can act on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KeptReason {
    /// A sprite type no editor here reads: CMYK, JPEG or YCbCr.
    UnsupportedType,
    /// A screen mode with no pixel format here: Teletext or an extension.
    UnknownMode,
    /// Larger than an editor opens.
    TooLarge,
    /// Its bytes do not describe a whole sprite.
    Damaged,
}

impl KeptReason {
    const fn to_wire(self) -> u8 {
        match self {
            Self::UnsupportedType => 1,
            Self::UnknownMode => 2,
            Self::TooLarge => 3,
            Self::Damaged => 4,
        }
    }

    const fn from_wire(raw: u8) -> Option<Self> {
        Some(match raw {
            1 => Self::UnsupportedType,
            2 => Self::UnknownMode,
            3 => Self::TooLarge,
            4 => Self::Damaged,
            _ => return None,
        })
    }

    fn of_decode(err: &DecodeError) -> Self {
        match EditRefusal::of_decode(err) {
            EditRefusal::TooLarge => Self::TooLarge,
            _ => match err {
                DecodeError::SpriteUnsupportedType => Self::UnsupportedType,
                DecodeError::SpriteUnknownMode => Self::UnknownMode,
                _ => Self::Damaged,
            },
        }
    }
}

impl core::fmt::Display for KeptReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedType => "its sprite type (CMYK, JPEG or YCbCr) cannot be edited",
            Self::UnknownMode => "its screen mode has no pixel format that can be edited",
            Self::TooLarge => "it is too large to edit",
            Self::Damaged => "it is damaged",
        })
    }
}

/// A sprite kept as its bytes, which [`read_kept`] fetches.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EditKept {
    /// Its name.
    pub name: SpriteName,
    /// Why it could not be read.
    pub reason: KeptReason,
    /// How many bytes it is.
    pub length: u32,
}

/// One entry of an opened document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditEntry {
    /// A picture.
    Picture(EditPicture),
    /// A sprite kept as its bytes.
    Kept(EditKept),
}

/// What the worker holds for an edit: the document it opened, and the entry
/// selected last.
pub(crate) struct EditSession {
    opened: Opened,
    selected: Option<Selected>,
}

enum Opened {
    Single(Picture),
    Sprites(SpriteAreaReader<Vec<u8>>),
}

/// The entry selected last. A single-picture document's one entry is the
/// picture it opened as, which is lent rather than copied.
enum Selected {
    Opened,
    Sprite(Picture),
    Kept(Vec<u8>),
}

impl EditSession {
    /// The selected entry's picture, or why there is none to read rows of.
    fn picture(&self) -> Result<&Picture, EditRefusal> {
        match (&self.selected, &self.opened) {
            (Some(Selected::Opened), Opened::Single(picture))
            | (Some(Selected::Sprite(picture)), _) => Ok(picture),
            (Some(Selected::Kept(_)), _) => Err(EditRefusal::WrongKind),
            (None | Some(Selected::Opened), _) => Err(EditRefusal::NoEntry),
        }
    }
}

fn edit_limits() -> DecodeLimits {
    DecodeLimits::new(
        MAX_EDIT_SIDE,
        MAX_EDIT_SIDE,
        MAX_EDIT_PIXELS,
        MAX_VIEW_PROGRESSIVE_COEFFICIENT_BYTES,
    )
}

/// Serve one edit request against the uploaded `document` and the open
/// `session`.
pub(crate) fn dispatch(
    request: &[u8],
    document: &mut Option<Document>,
    session: &mut Option<EditSession>,
) -> Vec<u8> {
    let mut r = Reader::new(request);
    let result = match r.u8() {
        Ok(OP_EDIT_OPEN) => open(&mut r, document, session),
        Ok(OP_EDIT_SELECT) => select(&mut r, session),
        Ok(OP_EDIT_ROWS) => rows(&mut r, session),
        Ok(OP_EDIT_KEPT) => kept(&mut r, session),
        Ok(OP_EDIT_RELEASE) if r.is_exhausted() => {
            *session = None;
            *document = None;
            let mut w = Writer::new();
            w.u8(REPLY_EDIT_RELEASED);
            Ok(w.finish())
        }
        _ => Err(EditRefusal::MalformedRequest),
    };
    result.unwrap_or_else(|refusal| encode_error(refusal.to_wire()))
}

/// `OP_EDIT_OPEN`: take the uploaded document and open it as the format
/// named, or as its signature says when none is.
fn open(
    r: &mut Reader<'_>,
    document: &mut Option<Document>,
    session: &mut Option<EditSession>,
) -> Result<Vec<u8>, EditRefusal> {
    let named = r.u8().map_err(|_| EditRefusal::MalformedRequest)?;
    if !r.is_exhausted() {
        return Err(EditRefusal::MalformedRequest);
    }
    *session = None;
    let held = document.take().ok_or(EditRefusal::NoDocument)?;
    if !held.is_complete() {
        return Err(EditRefusal::NoDocument);
    }
    let bytes = held.into_bytes();
    let format = match named {
        0 => sniff(&bytes).ok_or(EditRefusal::UnsupportedFormat)?,
        raw => ViewFormat::from_wire(raw)
            .ok_or(EditRefusal::MalformedRequest)?
            .raster()
            .ok_or(EditRefusal::UnsupportedFormat)?,
    };
    let wire_format = ViewFormat::from_raster(format).ok_or(EditRefusal::UnsupportedFormat)?;
    let (opened, unkept) = match open_native(format, bytes, &edit_limits()) {
        Ok(NativeDocument::Picture {
            picture, unkept, ..
        }) => (Opened::Single(picture), unkept),
        Ok(NativeDocument::Sprites(reader)) => {
            if reader.count() > MAX_EDIT_ENTRIES {
                return Err(EditRefusal::TooLarge);
            }
            (Opened::Sprites(reader), Unkept::default())
        }
        Err(err) => return Err(EditRefusal::of_decode(&err)),
    };
    let (sprites, count) = match &opened {
        Opened::Single(_) => (false, 1),
        Opened::Sprites(reader) => (true, reader.count()),
    };
    *session = Some(EditSession {
        opened,
        selected: None,
    });
    let mut w = Writer::new();
    w.u8(REPLY_EDIT_OPENED);
    w.u8(wire_format.to_wire());
    w.u8(u8::from(sprites));
    w.u32(count);
    w.u8(u8::from(unkept.precision));
    w.u8(u8::from(unkept.extras));
    Ok(w.finish())
}

/// `OP_EDIT_SELECT`: decode entry `index` and describe it.
fn select(r: &mut Reader<'_>, session: &mut Option<EditSession>) -> Result<Vec<u8>, EditRefusal> {
    let index = r.u32().map_err(|_| EditRefusal::MalformedRequest)?;
    if !r.is_exhausted() {
        return Err(EditRefusal::MalformedRequest);
    }
    let session = session.as_mut().ok_or(EditRefusal::NotOpen)?;
    // The previous entry goes before the next is decoded, so holding one
    // never costs two.
    session.selected = None;
    let mut w = Writer::new();
    w.u8(REPLY_EDIT_SELECTED);
    w.u32(index);
    let selected = match &mut session.opened {
        Opened::Single(picture) => {
            if index != 0 {
                return Err(EditRefusal::NoSuchEntry);
            }
            write_picture(&mut w, picture, None);
            Selected::Opened
        }
        Opened::Sprites(reader) => match reader.sprite(index) {
            Ok(Some(SpriteEntry::Picture(sprite))) => {
                let details = EditSprite {
                    name: sprite.name,
                    mode: sprite.mode,
                    masked: sprite.masked,
                    palette: sprite.palette,
                };
                write_picture(&mut w, &sprite.picture, Some(&details));
                Selected::Sprite(sprite.picture)
            }
            Ok(Some(SpriteEntry::Opaque(kept))) => {
                w.u8(1);
                w.bytes(kept.name.as_bytes());
                w.u8(KeptReason::of_decode(&kept.reason).to_wire());
                w.u32(u32::try_from(kept.bytes.len()).map_err(|_| EditRefusal::TooLarge)?);
                Selected::Kept(kept.bytes)
            }
            Ok(None) => return Err(EditRefusal::NoSuchEntry),
            Err(err) => return Err(EditRefusal::of_decode(&err)),
        },
    };
    session.selected = Some(selected);
    Ok(w.finish())
}

/// Describe a picture entry: its geometry, how its pixels arrive, and its
/// sprite details.
fn write_picture(w: &mut Writer, picture: &Picture, sprite: Option<&EditSprite>) {
    w.u8(0);
    w.u32(picture.width());
    w.u32(picture.height());
    match picture.pixels() {
        Pixels::Rgba(_) => {
            w.u8(0);
            w.bytes(&[]);
            w.u8(0);
        }
        Pixels::Indexed {
            depth,
            palette,
            mask,
            ..
        } => {
            w.u8(u8::try_from(depth.bits()).unwrap_or(8));
            w.bytes(palette.as_flattened());
            w.u8(u8::from(mask.is_some()));
        }
    }
    match sprite {
        None => w.u8(0),
        Some(details) => {
            w.u8(1);
            w.bytes(details.name.as_bytes());
            w.u32(details.mode.value());
            w.u8(u8::from(details.masked));
            match &details.palette {
                SpritePalette::Implied => w.u8(0),
                SpritePalette::Stored(raw) => {
                    w.u8(1);
                    w.bytes(raw);
                }
                SpritePalette::Full => w.u8(2),
            }
        }
    }
}

/// Bytes one row of `picture` costs in a rows reply: its samples and, when
/// it has one, its alpha plane.
fn row_bytes(width: u32, sample_bytes: usize, plane: bool) -> u64 {
    u64::from(width) * (sample_bytes as u64 + u64::from(plane))
}

/// `OP_EDIT_ROWS`: rows `first..first + count` of the selected picture.
fn rows(r: &mut Reader<'_>, session: &mut Option<EditSession>) -> Result<Vec<u8>, EditRefusal> {
    let first = r.u32().map_err(|_| EditRefusal::MalformedRequest)?;
    let count = r.u32().map_err(|_| EditRefusal::MalformedRequest)?;
    if !r.is_exhausted() {
        return Err(EditRefusal::MalformedRequest);
    }
    let picture = session.as_ref().ok_or(EditRefusal::NotOpen)?.picture()?;
    let (samples, plane, sample_bytes) = match picture.pixels() {
        Pixels::Rgba(rgba) => (rgba.as_slice(), None, 4usize),
        Pixels::Indexed { indices, mask, .. } => (indices.as_slice(), mask.as_deref(), 1),
    };
    let per_row = row_bytes(picture.width(), sample_bytes, plane.is_some());
    let last = first
        .checked_add(count)
        .ok_or(EditRefusal::RowsOutOfRange)?;
    if count == 0 || last > picture.height() || count > rows_fitting(per_row, ROWS_REPLY_HEADER) {
        return Err(EditRefusal::RowsOutOfRange);
    }
    let width = picture.width() as usize;
    let (start, end) = (first as usize * width, last as usize * width);
    let payload = (end - start) * (sample_bytes + usize::from(plane.is_some()));
    let mut w = Writer::try_with_capacity(payload.saturating_add(ROWS_REPLY_HEADER))
        .ok_or(EditRefusal::OutOfMemory)?;
    w.u8(REPLY_EDIT_ROWS);
    w.u32(first);
    w.u32(count);
    w.bytes(&samples[start * sample_bytes..end * sample_bytes]);
    w.bytes(plane.map_or(&[][..], |plane| &plane[start..end]));
    Ok(w.finish())
}

/// `OP_EDIT_KEPT`: `len` bytes of the selected kept sprite from `offset`.
fn kept(r: &mut Reader<'_>, session: &mut Option<EditSession>) -> Result<Vec<u8>, EditRefusal> {
    let offset = r.u32().map_err(|_| EditRefusal::MalformedRequest)?;
    let len = r.u32().map_err(|_| EditRefusal::MalformedRequest)?;
    if !r.is_exhausted() {
        return Err(EditRefusal::MalformedRequest);
    }
    let session = session.as_ref().ok_or(EditRefusal::NotOpen)?;
    let bytes = match &session.selected {
        Some(Selected::Kept(bytes)) => bytes,
        Some(_) => return Err(EditRefusal::WrongKind),
        None => return Err(EditRefusal::NoEntry),
    };
    let (offset, len) = (offset as usize, len as usize);
    let end = offset.checked_add(len).ok_or(EditRefusal::RowsOutOfRange)?;
    if len == 0 || len > MAX_KEPT_CHUNK || end > bytes.len() {
        return Err(EditRefusal::RowsOutOfRange);
    }
    let mut w =
        Writer::try_with_capacity(KEPT_REPLY_HEADER + len).ok_or(EditRefusal::OutOfMemory)?;
    w.u8(REPLY_EDIT_KEPT);
    w.u32(u32::try_from(offset).map_err(|_| EditRefusal::RowsOutOfRange)?);
    w.bytes(&bytes[offset..end]);
    Ok(w.finish())
}

fn malformed<T>(_: T) -> EditFailure {
    EditFailure::ReplyMalformed
}

/// This process could not hold what the worker answered: no fault of the
/// worker's, so it is not contained for it.
const OUT_OF_MEMORY: EditFailure = EditFailure::Refused(EditRefusal::OutOfMemory);

/// Open the uploaded document for editing, as `format` or, given none, as
/// its signature says.
///
/// # Errors
///
/// [`EditFailure`]: the sandbox failed, the worker refused, or the reply
/// could not be believed.
pub fn open_edit<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    format: Option<ViewFormat>,
) -> Result<EditDocument, EditFailure> {
    let mut w = Writer::new();
    w.u8(OP_EDIT_OPEN);
    w.u8(format.map_or(0, ViewFormat::to_wire));
    let request = w.finish();
    let asked = format;
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(EditFailure::Sandbox)?;
        let mut r = Reader::new(&reply);
        EditFailure::expect_tag(&mut r, REPLY_EDIT_OPENED)?;
        // A document opened as a named format is that format, or nothing.
        let format = r
            .u8()
            .ok()
            .and_then(ViewFormat::from_wire)
            .filter(|format| format.raster().is_some())
            .filter(|format| asked.is_none_or(|asked| asked == *format))
            .ok_or(EditFailure::ReplyMalformed)?;
        let sprites = EditFailure::flag(&mut r)?;
        let count = r.u32().map_err(malformed)?;
        let unkept = Unkept {
            precision: EditFailure::flag(&mut r)?,
            extras: EditFailure::flag(&mut r)?,
        };
        // Only a PNG is narrowed, and only a PNG or a JPEG holds what its
        // picture does not.
        let consistent = sprites == (format == ViewFormat::Sprite)
            && if sprites {
                (1..=MAX_EDIT_ENTRIES).contains(&count)
            } else {
                count == 1
            }
            && (!unkept.precision || format == ViewFormat::Png)
            && (!unkept.extras || matches!(format, ViewFormat::Png | ViewFormat::Jpeg));
        if !r.is_exhausted() || !consistent {
            return Err(EditFailure::ReplyMalformed);
        }
        Ok(EditDocument {
            format,
            sprites,
            count,
            unkept,
        })
    })
}

/// Choose entry `index` of `document`, the document [`open_edit`] opened,
/// and learn what it is.
///
/// # Errors
///
/// [`EditFailure`], as [`open_edit`]; an entry whose description breaks any
/// bound, contradicts itself, or is not what `document` holds — a sprite's
/// details on a single picture or missing from a sprite, a kept sprite
/// outside a sprite area — is not believed.
pub fn select_entry<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    document: EditDocument,
    index: u32,
) -> Result<EditEntry, EditFailure> {
    let mut w = Writer::new();
    w.u8(OP_EDIT_SELECT);
    w.u32(index);
    let request = w.finish();
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(EditFailure::Sandbox)?;
        let mut r = Reader::new(&reply);
        EditFailure::expect_tag(&mut r, REPLY_EDIT_SELECTED)?;
        if r.u32().map_err(malformed)? != index {
            return Err(EditFailure::ReplyMalformed);
        }
        let entry = match r.u8().map_err(malformed)? {
            0 => {
                let picture = read_picture(&mut r)?;
                if picture.sprite.is_some() != document.sprites {
                    return Err(EditFailure::ReplyMalformed);
                }
                EditEntry::Picture(picture)
            }
            1 if document.sprites => {
                let name = SpriteName::from_bytes(r.bytes(SpriteName::MAX_LEN).map_err(malformed)?)
                    .ok_or(EditFailure::ReplyMalformed)?;
                let reason = r
                    .u8()
                    .ok()
                    .and_then(KeptReason::from_wire)
                    .ok_or(EditFailure::ReplyMalformed)?;
                let length = r.u32().map_err(malformed)?;
                let bounds = u64::from(SPRITE_HEADER_LEN)..=MAX_DOCUMENT_BYTES as u64;
                if !bounds.contains(&u64::from(length)) {
                    return Err(EditFailure::ReplyMalformed);
                }
                EditEntry::Kept(EditKept {
                    name,
                    reason,
                    length,
                })
            }
            _ => return Err(EditFailure::ReplyMalformed),
        };
        if !r.is_exhausted() {
            return Err(EditFailure::ReplyMalformed);
        }
        Ok(entry)
    })
}

/// Read and check a picture entry's description.
fn read_picture(r: &mut Reader<'_>) -> Result<EditPicture, EditFailure> {
    let width = r.u32().map_err(malformed)?;
    let height = r.u32().map_err(malformed)?;
    let bits = r.u8().map_err(malformed)?;
    let palette = r
        .bytes(IndexDepth::Eight.colours() * 4)
        .map_err(malformed)?;
    let plane = EditFailure::flag(r)?;
    let pixels = if bits == 0 {
        if !palette.is_empty() || plane {
            return Err(EditFailure::ReplyMalformed);
        }
        EditPixels::Rgba
    } else {
        let depth = IndexDepth::from_bits(u32::from(bits)).ok_or(EditFailure::ReplyMalformed)?;
        let (entries, rest) = palette.as_chunks::<4>();
        if !rest.is_empty() {
            return Err(EditFailure::ReplyMalformed);
        }
        let palette = tairix_util::fallible::collected(entries.len(), entries.iter().copied())
            .ok_or(OUT_OF_MEMORY)?;
        EditPixels::Indexed {
            depth,
            palette,
            plane,
        }
    };
    let sprite = if EditFailure::flag(r)? {
        let name = SpriteName::from_bytes(r.bytes(SpriteName::MAX_LEN).map_err(malformed)?)
            .ok_or(EditFailure::ReplyMalformed)?;
        let mode = SpriteMode::from_value(r.u32().map_err(malformed)?)
            .ok_or(EditFailure::ReplyMalformed)?;
        let masked = EditFailure::flag(r)?;
        let palette = match r.u8().map_err(malformed)? {
            0 => SpritePalette::Implied,
            1 => {
                let raw = r
                    .bytes(SpritePalette::MAX_STORED_BYTES)
                    .map_err(malformed)?;
                if !SpritePalette::stores(raw) {
                    return Err(EditFailure::ReplyMalformed);
                }
                SpritePalette::Stored(
                    tairix_util::fallible::collected(raw.len(), raw.iter().copied())
                        .ok_or(OUT_OF_MEMORY)?,
                )
            }
            2 => SpritePalette::Full,
            _ => return Err(EditFailure::ReplyMalformed),
        };
        Some(EditSprite {
            name,
            mode,
            masked,
            palette,
        })
    } else {
        None
    };
    EditPicture::new(width, height, pixels, sprite).ok_or(EditFailure::ReplyMalformed)
}

/// Fetch every row of the selected `picture`, handing each to `row` with
/// its index, its samples and its alpha plane (empty where it has none).
///
/// Each index is held to the palette before it is handed on, so a caller
/// never sees one the palette cannot resolve.
///
/// # Errors
///
/// [`EditFailure`], as [`open_edit`]; a band whose echoed range, length or
/// indices disagree with the description is not believed.
pub fn read_rows<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    picture: &EditPicture,
    mut row: impl FnMut(u32, &[u8], &[u8]),
) -> Result<(), EditFailure> {
    let sample_bytes = picture.pixels.sample_bytes();
    let plane = picture.pixels.has_plane();
    let per_row = row_bytes(picture.width, sample_bytes, plane);
    let band = rows_fitting(per_row, ROWS_REPLY_HEADER);
    let colours = match &picture.pixels {
        EditPixels::Indexed { palette, .. } => palette.len(),
        EditPixels::Rgba => 0,
    };
    let width = picture.width as usize;
    let mut first = 0u32;
    while first < picture.height {
        let count = band.min(picture.height - first);
        let mut w = Writer::new();
        w.u8(OP_EDIT_ROWS);
        w.u32(first);
        w.u32(count);
        let request = w.finish();
        sandbox.ask(|sandbox| {
            let reply = sandbox.request(&request).map_err(EditFailure::Sandbox)?;
            let mut r = Reader::new(&reply);
            EditFailure::expect_tag(&mut r, REPLY_EDIT_ROWS)?;
            if r.u32().map_err(malformed)? != first || r.u32().map_err(malformed)? != count {
                return Err(EditFailure::ReplyMalformed);
            }
            let rows = count as usize;
            let samples = r.bytes(rows * width * sample_bytes).map_err(malformed)?;
            let mask = r.bytes(rows * width).map_err(malformed)?;
            if !r.is_exhausted()
                || samples.len() != rows * width * sample_bytes
                || mask.len() != if plane { rows * width } else { 0 }
                || (colours > 0 && samples.iter().any(|&index| usize::from(index) >= colours))
            {
                return Err(EditFailure::ReplyMalformed);
            }
            for (at, y) in (first..first + count).enumerate() {
                let line = &samples[at * width * sample_bytes..(at + 1) * width * sample_bytes];
                let alpha = if plane {
                    &mask[at * width..(at + 1) * width]
                } else {
                    &[][..]
                };
                row(y, line, alpha);
            }
            Ok(())
        })?;
        first += count;
    }
    Ok(())
}

/// Fetch the selected kept sprite's bytes into `out`, which is cleared and
/// grown fallibly.
///
/// # Errors
///
/// [`EditFailure`], as [`open_edit`]; [`EditRefusal::OutOfMemory`] where
/// `out` cannot be grown to hold them.
pub fn read_kept<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    kept: &EditKept,
    out: &mut Vec<u8>,
) -> Result<(), EditFailure> {
    out.clear();
    let length = kept.length as usize;
    if !tairix_util::fallible::reserve(out, length) {
        return Err(OUT_OF_MEMORY);
    }
    while out.len() < length {
        let offset = out.len();
        let len = (length - offset).min(MAX_KEPT_CHUNK);
        let mut w = Writer::new();
        w.u8(OP_EDIT_KEPT);
        w.u32(u32::try_from(offset).map_err(malformed)?);
        w.u32(u32::try_from(len).map_err(malformed)?);
        let request = w.finish();
        sandbox.ask(|sandbox| {
            let reply = sandbox.request(&request).map_err(EditFailure::Sandbox)?;
            let mut r = Reader::new(&reply);
            EditFailure::expect_tag(&mut r, REPLY_EDIT_KEPT)?;
            if r.u32().map_err(malformed)? as usize != offset {
                return Err(EditFailure::ReplyMalformed);
            }
            let chunk = r.bytes(len).map_err(malformed)?;
            if chunk.len() != len || !r.is_exhausted() {
                return Err(EditFailure::ReplyMalformed);
            }
            out.extend_from_slice(chunk);
            Ok(())
        })?;
    }
    Ok(())
}

/// Drop the edit session and the document it held.
///
/// # Errors
///
/// [`EditFailure`], as [`open_edit`].
pub fn close_edit<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
) -> Result<(), EditFailure> {
    let mut w = Writer::new();
    w.u8(OP_EDIT_RELEASE);
    let request = w.finish();
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(EditFailure::Sandbox)?;
        let mut r = Reader::new(&reply);
        EditFailure::expect_tag(&mut r, REPLY_EDIT_RELEASED)?;
        if r.is_exhausted() {
            Ok(())
        } else {
            Err(EditFailure::ReplyMalformed)
        }
    })
}

#[cfg(test)]
#[path = "imageedit_tests.rs"]
mod tests;

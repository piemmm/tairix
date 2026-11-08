//! The sandboxed read a drawing editor opens a document through.
//!
//! A drawing file is untrusted input, so the editor holding it never parses
//! one: the bytes go over the one upload path
//! ([`crate::imagerender::send_document`],
//! [`crate::imagerender::upload_document`]) to a capability-empty worker,
//! which reads them with `tairix_drawing` and sends the drawing back as
//! records, a batch at a time. Every gradient and style travels once and is
//! referred to by number; a long path's nodes are cut across records, so no
//! record outgrows a frame.
//!
//! The editor believes nothing beyond what it checks as it rebuilds the
//! drawing: every value goes through the drawing's own constructors, which
//! refuse what a drawing could not hold; every reference names a gradient or
//! style already sent; every count is held to the drawing's bounds and the
//! tree's shape to what each group declared. A reply that fails any of it
//! cannot be believed, and retires the worker.
//!
//! The pictures a drawing's objects show travel first, each once however
//! many objects show it: its shape and what the bytes it was read from state,
//! then its premultiplied pixels and those bytes a frame at a time, each
//! pixel held to its alpha as it lands. A picture a user places is decoded
//! the same way, by its content, and travels the same way.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, FillRule, Point};
use tairix_collections::{try_arc, SharedSlice, SharedStr, TryReserveError};
use tairix_colour::Rgba;
use tairix_drawing::limits::{
    MAX_DASH_ENTRIES, MAX_DEPTH, MAX_FAMILY_BYTES, MAX_GUIDES, MAX_IMAGE_SIDE,
    MAX_IMAGE_SOURCE_BYTES, MAX_NAME_BYTES, MAX_OBJECTS, MAX_PATH_NODES, MAX_STOPS, MAX_SWATCHES,
    MAX_TEXT_BYTES, MAX_TEXT_PATH_NODES, MAX_TEXT_RUNS, MAX_TOTAL_IMAGE_PIXELS,
    MAX_TOTAL_PATH_NODES,
};
use tairix_drawing::numbering::Numbered;
use tairix_drawing::shape::{EllipseShape, RectShape, StarShape};
use tairix_drawing::unkept::Unkept;
use tairix_drawing::Baseline;
use tairix_drawing::{
    ArcKind, ArcPart, ArrowShape, Arrowhead, Cap, Children, Contour, Drawing, DrawingError,
    Gradient, GradientKind, Grid, Guide, GuideAxis, Ids, Info, Join, Kind, Node, NodeJoin, Page,
    Paint, PaintOrder, PathNode, PathShape, Run, RunPaint, RunStyle, Shape, Spread, Stop, Stroke,
    Style, Text, TextAlign, TextPath, Unit,
};
use tairix_drawing::{Encoding, Image, PlacedPicture, Read, ReadError, Source};
use tairix_image::{Density, DensityUnit, JpegFrame, Orientation};
use tairix_raster::{Pixel, Pyramid, Surface, MAX_SURFACE_PIXELS};
use tairix_util::fallible::{self, try_push};

use crate::host::{Launcher, ParserSandbox, SandboxError, Unbelieved};
use crate::imagerender::{encode_error, is_document_op, Document, DocumentRefusal, ReplyFailure};
use crate::memory::WorkerMemory;
use crate::proto::MAX_FRAME;
use crate::wire::{bytes_prefix, Reader, WireError, Writer, BYTES_PREFIX};
use crate::worker::Service;

/// The memory a drawing worker is held to: a document and the drawing read
/// from it.
pub const WORKER_MEMORY: WorkerMemory = WorkerMemory::Documents;

const OP_OPEN: u8 = 30;
const OP_NEXT: u8 = 31;
const OP_PICTURE: u8 = 32;
const OP_PIXELS: u8 = 33;
const OP_PLACE: u8 = 34;
const OP_PAGES: u8 = 35;
const REPLY_OPENED: u8 = 30;
const REPLY_BATCH: u8 = 31;
const REPLY_PICTURE: u8 = 32;
const REPLY_PIXELS: u8 = 33;
const REPLY_PLACED: u8 = 34;
const REPLY_PAGES: u8 = 35;

/// The longest password a document is opened with, in bytes: past what any
/// PDF security handler reads, so every password typed reaches the reader
/// whole, and a bound on what a request carries.
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// The bytes one page's size travels in: its width and height.
const PAGE_SIZE_WIRE: usize = 16;

/// The bytes a page list's reply begins with: its tag, the document's page
/// count and how many sizes follow.
const PAGES_HEADER: usize = 1 + 4 + 4;

/// The most pages whose sizes one reply lists: what a frame holds.
const MAX_LISTED_PAGES: usize = (MAX_FRAME - PAGES_HEADER) / PAGE_SIZE_WIRE;

const RECORD_GRADIENT: u8 = 1;
const RECORD_STYLE: u8 = 2;
const RECORD_GROUP: u8 = 3;
const RECORD_RECT: u8 = 4;
const RECORD_ELLIPSE: u8 = 5;
const RECORD_STAR: u8 = 6;
const RECORD_PATH: u8 = 7;
const RECORD_CONTOUR: u8 = 8;
const RECORD_NODES: u8 = 9;
const RECORD_TEXT: u8 = 10;
const RECORD_IMAGE: u8 = 11;

/// How a picture's source travels: none, or the encoding it states.
const SOURCE_NONE: u8 = 0;
const SOURCE_PNG: u8 = 1;
const SOURCE_JPEG: u8 = 2;
const SOURCE_SPRITE: u8 = 3;

/// The most bytes of a picture one reply carries, past its tag: whole
/// pixels, so every reply of a picture's pixels lands on a pixel's edge.
const PIXELS_BUDGET: usize = (MAX_FRAME - 1) / PIXEL_BYTES * PIXEL_BYTES;

/// Bytes one premultiplied pixel travels in: red, green, blue, alpha.
const PIXEL_BYTES: usize = 4;

/// Most path nodes one record carries.
const NODES_PER_RECORD: usize = 1 << 16;

/// The most one node takes on the wire: its point, two handles and a join.
const NODE_WIRE: usize = 16 + 2 * (1 + 16) + 1;

/// What a batch reply spends besides its records: its tag and whether it is
/// the last.
const BATCH_HEADER: usize = 2;

/// The most record bytes one batch carries.
const BATCH_BUDGET: usize = MAX_FRAME - BATCH_HEADER;

/// The most a text's own fixed fields take: its size, weight, posture,
/// letter spacing and alignment.
const TEXT_FIELDS_WIRE: usize = 8 + 4 + 1 + 8 + 1;

/// The most a text's baseline takes: its kind, then an origin and a line
/// height, or a start and a path of contours each its closure, its count and
/// at least one node.
const BASELINE_WIRE: usize = {
    let lines = 16 + 8;
    let path = 8 + 4 + MAX_TEXT_PATH_NODES * (1 + 4 + NODE_WIRE);
    1 + if lines > path { lines } else { path }
};

/// The most one run takes: its span, what it overrides, and every override.
const RUN_WIRE: usize =
    2 * 4 + 1 + BYTES_PREFIX + MAX_FAMILY_BYTES + 8 + 4 + 1 + 8 + 8 + 2 * (1 + 4);

/// The longest record: a text's content, family, fields, baseline and runs,
/// or a run of nodes, whichever is longer.
const MAX_RECORD: usize = {
    let text = 1
        + COMMON_WIRE
        + 4
        + 2 * BYTES_PREFIX
        + MAX_TEXT_BYTES
        + MAX_FAMILY_BYTES
        + TEXT_FIELDS_WIRE
        + BASELINE_WIRE
        + 4
        + MAX_TEXT_RUNS * RUN_WIRE;
    let nodes = 1 + 4 + NODES_PER_RECORD * NODE_WIRE;
    if text > nodes {
        text
    } else {
        nodes
    }
};

/// The most an object's common fields take: its name, flags, opacity and
/// transform.
const COMMON_WIRE: usize = 1 + BYTES_PREFIX + MAX_NAME_BYTES + 2 + 8 + 6 * 8;

const _: () = assert!(BYTES_PREFIX + MAX_RECORD <= BATCH_BUDGET);

/// Why the worker refused to read a drawing, carried typed over the wire.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DrawRefusal {
    /// The request broke its grammar.
    MalformedRequest,
    /// No complete document had been handed over.
    NoDocument,
    /// The document is no drawing this service reads.
    NotADrawing,
    /// The document is a drawing, but a broken one.
    Malformed,
    /// The document asks for more than a drawing may hold.
    TooComplex,
    /// The worker could not hold the drawing.
    OutOfMemory,
    /// Records were asked for with no drawing open.
    NotOpen,
    /// The document is no picture this service reads.
    NotAPicture,
    /// The document is protected by a password, and none was given.
    NeedsPassword,
    /// The password given does not open the document.
    WrongPassword,
    /// The page asked for is not one the document has.
    NoSuchPage,
}

impl DrawRefusal {
    const fn to_wire(self) -> u8 {
        match self {
            Self::MalformedRequest => 1,
            Self::NoDocument => 2,
            Self::NotADrawing => 3,
            Self::Malformed => 4,
            Self::TooComplex => 5,
            Self::OutOfMemory => 6,
            Self::NotOpen => 7,
            Self::NotAPicture => 8,
            Self::NeedsPassword => 9,
            Self::WrongPassword => 10,
            Self::NoSuchPage => 11,
        }
    }

    const fn from_wire(raw: u8) -> Option<Self> {
        Some(match raw {
            1 => Self::MalformedRequest,
            2 => Self::NoDocument,
            3 => Self::NotADrawing,
            4 => Self::Malformed,
            5 => Self::TooComplex,
            6 => Self::OutOfMemory,
            7 => Self::NotOpen,
            8 => Self::NotAPicture,
            9 => Self::NeedsPassword,
            10 => Self::WrongPassword,
            11 => Self::NoSuchPage,
            _ => return None,
        })
    }

    const fn of_read(error: ReadError) -> Self {
        match error {
            ReadError::Unrecognised => Self::NotADrawing,
            ReadError::Malformed => Self::Malformed,
            ReadError::TooComplex => Self::TooComplex,
            ReadError::OutOfMemory => Self::OutOfMemory,
            ReadError::NeedsPassword => Self::NeedsPassword,
            ReadError::WrongPassword => Self::WrongPassword,
            ReadError::NoSuchPage => Self::NoSuchPage,
        }
    }

    const fn of_drawing(error: DrawingError) -> Self {
        match error {
            DrawingError::Invalid => Self::Malformed,
            DrawingError::TooComplex => Self::TooComplex,
            DrawingError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

impl core::fmt::Display for DrawRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::MalformedRequest => "malformed drawing request",
            Self::NoDocument => "no document was handed over",
            Self::NotADrawing => "not a drawing that can be read",
            Self::Malformed => "the drawing is damaged",
            Self::TooComplex => "the drawing holds more than can be opened",
            Self::OutOfMemory => "there was not enough memory to read the drawing",
            Self::NotOpen => "no drawing is open",
            Self::NotAPicture => "not a picture that can be placed",
            Self::NeedsPassword => "the document is protected by a password",
            Self::WrongPassword => "the password does not open the document",
            Self::NoSuchPage => "the document has no such page",
        })
    }
}

/// Why no drawing came back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DrawFailure {
    /// The sandbox itself failed.
    Sandbox(SandboxError),
    /// The worker refused, for the carried reason.
    Refused(DrawRefusal),
    /// A reply broke its grammar or its bounds: it cannot be believed.
    ReplyMalformed,
    /// There was no memory for the drawing.
    NoMemory,
}

impl DrawFailure {
    /// Whether it failed for want of memory, on either side: worth asking
    /// again, since it says nothing of the document.
    #[must_use]
    pub const fn passing(&self) -> bool {
        match self {
            Self::Sandbox(failure) => {
                failure.out_of_memory() || matches!(failure, SandboxError::WorkerUnavailable(_))
            }
            Self::Refused(refusal) => matches!(refusal, DrawRefusal::OutOfMemory),
            Self::NoMemory => true,
            Self::ReplyMalformed => false,
        }
    }
}

impl Unbelieved for DrawFailure {
    fn unbelieved(&self) -> bool {
        matches!(self, Self::ReplyMalformed)
    }
}

impl ReplyFailure for DrawFailure {
    const MALFORMED: Self = Self::ReplyMalformed;

    fn refused(code: u8) -> Option<Self> {
        DrawRefusal::from_wire(code).map(Self::Refused)
    }
}

impl From<WireError> for DrawFailure {
    fn from(_: WireError) -> Self {
        Self::ReplyMalformed
    }
}

impl From<TryReserveError> for DrawFailure {
    fn from(error: TryReserveError) -> Self {
        DrawingError::from(error).into()
    }
}

impl From<alloc::collections::TryReserveError> for DrawFailure {
    fn from(_: alloc::collections::TryReserveError) -> Self {
        Self::NoMemory
    }
}

impl From<DrawingError> for DrawFailure {
    /// A value the drawing refuses was never one an honest worker sends.
    fn from(error: DrawingError) -> Self {
        match error {
            DrawingError::OutOfMemory => Self::NoMemory,
            DrawingError::Invalid | DrawingError::TooComplex => Self::ReplyMalformed,
        }
    }
}

impl core::fmt::Display for DrawFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Sandbox(inner) => write!(f, "the reader failed: {inner}"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::ReplyMalformed => f.write_str("the reader's answer could not be believed"),
            Self::NoMemory => f.write_str("there is not enough memory to hold the drawing"),
        }
    }
}

/// The worker a drawing is read in: a document handed over the one upload
/// path, and the drawing it comes to, a batch of records at a time.
#[derive(Default)]
pub struct DrawEditService {
    document: Option<Document>,
    reading: Option<Reading>,
    /// The pictures of the drawing or the picture last opened, by number.
    pictures: Vec<Shown>,
}

/// A picture that travels: its pixels, and the bytes it was read from.
#[derive(Clone)]
struct Shown {
    pixels: Held,
    source: Option<Source>,
}

/// Where a travelling picture's pixels are held.
#[derive(Clone)]
enum Held {
    /// Shared with the drawing that shows them.
    Pyramid(Arc<Pyramid>),
    /// A placed picture's own, which nothing here draws.
    Surface(Arc<Surface>),
}

impl Shown {
    fn pixels(&self) -> &Surface {
        match &self.pixels {
            Held::Pyramid(pyramid) => pyramid.base(),
            Held::Surface(surface) => surface,
        }
    }

    /// The bytes it travels as: its pixels, then its source.
    fn length(&self) -> u64 {
        let pixels = self.pixels().pixels().len() as u64 * PIXEL_BYTES as u64;
        pixels
            + self
                .source
                .as_ref()
                .map_or(0, |source| source.bytes.len() as u64)
    }
}

/// A drawing being sent: its records, each length-prefixed, where each one
/// ends, and how many have gone.
struct Reading {
    records: Records,
    sent: usize,
}

impl Service for DrawEditService {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let served = match request.first().copied() {
            Some(op) if is_document_op(op) => {
                self.reading = None;
                self.pictures.clear();
                Document::serve(request, &mut self.document).map_err(DocumentRefusal::to_wire)
            }
            Some(OP_OPEN) => self.open(request).map_err(DrawRefusal::to_wire),
            Some(OP_NEXT) => self.next(request).map_err(DrawRefusal::to_wire),
            Some(OP_PLACE) => self.place(request).map_err(DrawRefusal::to_wire),
            Some(OP_PAGES) => self.pages(request).map_err(DrawRefusal::to_wire),
            Some(OP_PICTURE) => self.picture(request).map_err(DrawRefusal::to_wire),
            Some(OP_PIXELS) => self.pixels(request).map_err(DrawRefusal::to_wire),
            _ => Err(DrawRefusal::MalformedRequest.to_wire()),
        };
        served.unwrap_or_else(encode_error)
    }
}

impl DrawEditService {
    /// `OP_OPEN`: read the document handed over, in whichever format its
    /// content is — a PDF at the page and with the password asked — and
    /// answer the drawing's page and settings, keeping its records to send.
    fn open(&mut self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        self.reading = None;
        self.pictures.clear();
        let mut r = Reader::new(request.get(1..).ok_or(DrawRefusal::MalformedRequest)?);
        let page = usize::try_from(r.u32().map_err(|_| DrawRefusal::MalformedRequest)?)
            .map_err(|_| DrawRefusal::MalformedRequest)?;
        let (budget, password) = take_access(&mut r)?;
        let pdf = tairix_drawing::pdf::Request {
            page,
            password,
            budget,
        };
        let bytes = self.document_bytes()?;
        let read =
            tairix_drawing::read(&bytes, &pdf, &mut Ids::new()).map_err(DrawRefusal::of_read)?;
        drop(bytes);
        let (records, pictures) = records_of(&read.drawing)?;
        let mut w = Writer::new();
        w.u8(REPLY_OPENED);
        put_header(&mut w, &read, pictures.len());
        self.reading = Some(Reading { records, sent: 0 });
        self.pictures = pictures;
        Ok(w.finish())
    }

    /// The bytes of the complete document handed over, taken: the document
    /// is read once.
    fn document_bytes(&mut self) -> Result<Vec<u8>, DrawRefusal> {
        let document = self.document.take().ok_or(DrawRefusal::NoDocument)?;
        if !document.is_complete() {
            return Err(DrawRefusal::NoDocument);
        }
        Ok(document.into_bytes())
    }

    /// `OP_PAGES`: the pages of the document handed over, kept to be opened:
    /// a PDF's count and as many of their sizes as a reply holds, and one
    /// page, of no stated size, for any other format.
    fn pages(&mut self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        let mut r = Reader::new(request.get(1..).ok_or(DrawRefusal::MalformedRequest)?);
        let (budget, password) = take_access(&mut r)?;
        let document = self.document.as_ref().ok_or(DrawRefusal::NoDocument)?;
        if !document.is_complete() {
            return Err(DrawRefusal::NoDocument);
        }
        let bytes = document.bytes();
        let mut w = Writer::new();
        w.u8(REPLY_PAGES);
        if !tairix_drawing::pdf::recognised(bytes) {
            w.u32(1);
            w.u32(0);
            return Ok(w.finish());
        }
        let pdf = tairix_drawing::pdf::Request {
            page: 0,
            password,
            budget,
        };
        let (pages, sizes) = tairix_drawing::pdf::pages(bytes, &pdf, MAX_LISTED_PAGES)
            .map_err(DrawRefusal::of_read)?;
        w.u32(count(pages));
        w.u32(count(sizes.len()));
        for &(width, height) in &sizes {
            put_f64(&mut w, width);
            put_f64(&mut w, height);
        }
        Ok(w.finish())
    }

    /// `OP_PLACE`: decode the document handed over as a picture to place,
    /// by its content, and answer the size it states for its pixels, keeping
    /// it as picture nought.
    fn place(&mut self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        self.reading = None;
        self.pictures.clear();
        if request.len() != 1 {
            return Err(DrawRefusal::MalformedRequest);
        }
        let bytes = self.document_bytes()?;
        let placed = tairix_drawing::decode_placed(&bytes).map_err(|error| match error {
            ReadError::Unrecognised => DrawRefusal::NotAPicture,
            other => DrawRefusal::of_read(other),
        })?;
        drop(bytes);
        let shown = Shown {
            pixels: Held::Surface(try_arc(placed.pixels).map_err(|_| DrawRefusal::OutOfMemory)?),
            source: placed.source,
        };
        if !fallible::push(&mut self.pictures, shown) {
            return Err(DrawRefusal::OutOfMemory);
        }
        let mut w = Writer::new();
        w.u8(REPLY_PLACED);
        put_point(&mut w, placed.natural);
        Ok(w.finish())
    }

    /// The picture a request names after its tag, and what follows.
    fn named<'r>(&self, request: &'r [u8]) -> Result<(&Shown, Reader<'r>), DrawRefusal> {
        let mut r = Reader::new(request.get(1..).ok_or(DrawRefusal::MalformedRequest)?);
        let index = r.u32().map_err(|_| DrawRefusal::MalformedRequest)?;
        let shown = usize::try_from(index)
            .ok()
            .and_then(|index| self.pictures.get(index))
            .ok_or(DrawRefusal::MalformedRequest)?;
        Ok((shown, r))
    }

    /// `OP_PICTURE`: a picture's size and what its source states.
    fn picture(&self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        let (shown, r) = self.named(request)?;
        if !r.is_exhausted() {
            return Err(DrawRefusal::MalformedRequest);
        }
        let mut w = Writer::new();
        w.u8(REPLY_PICTURE);
        w.u32(shown.pixels().width());
        w.u32(shown.pixels().height());
        put_source(&mut w, shown.source.as_ref());
        Ok(w.finish())
    }

    /// `OP_PIXELS`: a picture's bytes from the offset asked, as many as a
    /// frame holds — its premultiplied pixels, then its source.
    fn pixels(&self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        let (shown, mut r) = self.named(request)?;
        let offset = r.u64().map_err(|_| DrawRefusal::MalformedRequest)?;
        if !r.is_exhausted() || offset >= shown.length() {
            return Err(DrawRefusal::MalformedRequest);
        }
        let count = usize::try_from(shown.length() - offset)
            .unwrap_or(usize::MAX)
            .min(PIXELS_BUDGET);
        let mut w = Writer::try_with_capacity(1 + count).ok_or(DrawRefusal::OutOfMemory)?;
        w.u8(REPLY_PIXELS);
        let pixels = shown.pixels().pixels();
        let pixel_bytes = pixels.len() * PIXEL_BYTES;
        let start = usize::try_from(offset).map_err(|_| DrawRefusal::MalformedRequest)?;
        let end = start + count;
        let mut at = start;
        while at < end.min(pixel_bytes) {
            let pixel = pixels[at / PIXEL_BYTES];
            let bytes = [pixel.r, pixel.g, pixel.b, pixel.a];
            let within = at % PIXEL_BYTES;
            let take = (PIXEL_BYTES - within).min(end - at);
            w.raw(&bytes[within..within + take]);
            at += take;
        }
        if end > pixel_bytes {
            let source = shown
                .source
                .as_ref()
                .map_or(&[][..], |source| &*source.bytes);
            w.raw(&source[at - pixel_bytes..end - pixel_bytes]);
        }
        Ok(w.finish())
    }

    /// `OP_NEXT`: the next whole records that fit one reply.
    fn next(&mut self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        if request.len() != 1 {
            return Err(DrawRefusal::MalformedRequest);
        }
        let reading = self.reading.as_mut().ok_or(DrawRefusal::NotOpen)?;
        let ends = &reading.records.ends;
        let from = if reading.sent == 0 {
            0
        } else {
            ends[reading.sent - 1]
        };
        let mut upto = reading.sent;
        while upto < ends.len() && ends[upto] - from <= BATCH_BUDGET {
            upto += 1;
        }
        let to = if upto == 0 { 0 } else { ends[upto - 1] };
        let last = upto == ends.len();
        let mut w =
            Writer::try_with_capacity(BATCH_HEADER + to - from).ok_or(DrawRefusal::OutOfMemory)?;
        w.u8(REPLY_BATCH);
        w.u8(u8::from(last));
        w.raw(&reading.records.bytes[from..to]);
        reading.sent = upto;
        if last {
            self.reading = None;
        }
        Ok(w.finish())
    }
}

/// How a document is opened: which page of a PDF, and the password a
/// protected one is opened with, a secret this module neither keeps nor logs.
#[derive(Clone, Copy, Default)]
pub struct Access<'a> {
    /// The page, counting from 0; any other format has only page 0.
    pub page: u32,
    /// The password, where the empty one does not open it.
    pub password: Option<&'a [u8]>,
}

/// A request of `op` carrying `access`'s page where `paged`, the room the
/// worker's streams may decode to, and its password. Answered so the caller
/// sends it and then wipes it.
fn access_request(
    op: u8,
    paged: bool,
    access: &Access<'_>,
    room: u64,
) -> Result<Vec<u8>, DrawFailure> {
    let password = access.password.unwrap_or_default();
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(DrawFailure::Refused(DrawRefusal::MalformedRequest));
    }
    // Sized once, so the password is never left in a block a growth freed.
    let mut w = Writer::try_with_capacity(1 + 4 + 8 + BYTES_PREFIX + password.len())
        .ok_or(DrawFailure::NoMemory)?;
    w.u8(op);
    if paged {
        w.u32(access.page);
    }
    w.u64(room);
    w.bytes(password);
    Ok(w.finish())
}

/// `request` sent, then wiped: it may carry a password.
fn ask_wiping<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    mut request: Vec<u8>,
) -> Result<Vec<u8>, DrawFailure> {
    let reply = sandbox.request(&request).map_err(DrawFailure::Sandbox);
    tairix_util::secret::wipe(&mut request);
    reply
}

/// The room and the password a request carries after its tag and page.
fn take_access<'r>(r: &mut Reader<'r>) -> Result<(u64, Option<&'r [u8]>), DrawRefusal> {
    let room = r.u64().map_err(|_| DrawRefusal::MalformedRequest)?;
    let password = r
        .bytes(MAX_PASSWORD_BYTES)
        .map_err(|_| DrawRefusal::MalformedRequest)?;
    if !r.is_exhausted() {
        return Err(DrawRefusal::MalformedRequest);
    }
    Ok((room, (!password.is_empty()).then_some(password)))
}

/// A document's pages, as a chooser lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct Pages {
    /// How many it has, at least one.
    pub count: u32,
    /// The width and height of the first of them, in points, as many as one
    /// reply lists; a format other than PDF lists none.
    pub sizes: Vec<(f64, f64)>,
}

/// The room a worker holding a document `handed` bytes long has left to
/// decode into: what its share holds beside the document.
fn decode_room<L: Launcher, S: tairix_log::Sink>(
    sandbox: &ParserSandbox<L, S>,
    handed: u64,
) -> u64 {
    sandbox.document_room().saturating_sub(handed)
}

/// The pages of the document handed over last, `handed` bytes long, opened
/// with `access`'s password: what a chooser asks which of to open. The
/// document stays with the worker, to be read.
///
/// # Errors
///
/// [`DrawFailure`]: the sandbox failed, the worker refused — among them for
/// a password wanted or wrong — or a reply could not be believed.
pub fn read_pages<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    handed: u64,
    access: &Access<'_>,
) -> Result<Pages, DrawFailure> {
    let room = decode_room(sandbox, handed);
    sandbox.ask(|sandbox| {
        let reply = ask_wiping(sandbox, access_request(OP_PAGES, false, access, room)?)?;
        let mut r = Reader::new(&reply);
        DrawFailure::expect_tag(&mut r, REPLY_PAGES)?;
        let count = r.u32()?;
        let listed = r.u32()?;
        let Ok(listed) = usize::try_from(listed) else {
            return Err(DrawFailure::ReplyMalformed);
        };
        let stated = listed.checked_mul(PAGE_SIZE_WIRE);
        if count == 0
            || u32::try_from(listed).map_or(true, |listed| listed > count)
            || listed > MAX_LISTED_PAGES
            || stated != Some(r.remaining())
        {
            return Err(DrawFailure::ReplyMalformed);
        }
        let mut sizes = Vec::new();
        if !fallible::reserve(&mut sizes, listed) {
            return Err(DrawFailure::NoMemory);
        }
        for _ in 0..listed {
            let (width, height) = (take_f64(&mut r)?, take_f64(&mut r)?);
            let fits = |side: f64| side.is_finite() && side > 0.0;
            if !fits(width) || !fits(height) {
                return Err(DrawFailure::ReplyMalformed);
            }
            sizes.push((width, height));
        }
        if !r.is_exhausted() {
            return Err(DrawFailure::ReplyMalformed);
        }
        Ok(Pages { count, sizes })
    })
}

/// Read the document handed over last, `handed` bytes long — an SVG
/// document, a RISC OS Draw file or a PDF, told apart by its content, a PDF
/// at `access`'s page and with its password — and rebuild the drawing it
/// comes to, every value checked, issuing its objects' identities from `ids`.
///
/// # Errors
///
/// [`DrawFailure`]: the sandbox failed, the worker refused, or a reply could
/// not be believed — among them pictures whose sources together are longer
/// than both the document and all the worker may decode.
pub fn read_drawing<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    handed: u64,
    access: &Access<'_>,
    ids: &mut Ids,
) -> Result<Read, DrawFailure> {
    let room = decode_room(sandbox, handed);
    sandbox.ask(|sandbox| {
        let reply = ask_wiping(sandbox, access_request(OP_OPEN, true, access, room)?)?;
        let mut r = Reader::new(&reply);
        DrawFailure::expect_tag(&mut r, REPLY_OPENED)?;
        let header = take_header(&mut r)?;
        if !r.is_exhausted() {
            return Err(DrawFailure::ReplyMalformed);
        }
        let mut pictures = Vec::new();
        if !fallible::reserve(&mut pictures, header.pictures) {
            return Err(DrawFailure::NoMemory);
        }
        // A source is part of the document, or bytes its streams decoded
        // to, every one of which the worker charges to `room`.
        let mut allowance = Allowance::of(handed.max(room));
        for index in 0..header.pictures {
            let (pixels, source) = receive_picture(sandbox, index, &mut allowance)?;
            let pyramid = Pyramid::new(pixels).ok_or(DrawFailure::NoMemory)?;
            pictures.push((try_arc(pyramid)?, source));
        }
        let mut builder = Builder::new(header, pictures, ids);
        loop {
            let reply = sandbox.request(&[OP_NEXT]).map_err(DrawFailure::Sandbox)?;
            if builder.batch(&reply)? {
                return builder.finish();
            }
        }
    })
}

/// Decode the document handed over last, `handed` bytes long — any still
/// picture, by its content — as a picture to place in a drawing, its pixels
/// checked as they arrive.
///
/// # Errors
///
/// [`DrawFailure`]: the sandbox failed, the worker refused, or a reply could
/// not be believed — among them a source longer than the document.
pub fn read_placed<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    handed: u64,
) -> Result<PlacedPicture, DrawFailure> {
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&[OP_PLACE]).map_err(DrawFailure::Sandbox)?;
        let mut r = Reader::new(&reply);
        DrawFailure::expect_tag(&mut r, REPLY_PLACED)?;
        let natural = take_point(&mut r)?;
        if !r.is_exhausted() || !tairix_drawing::placeable(natural) {
            return Err(DrawFailure::ReplyMalformed);
        }
        let (pixels, source) = receive_picture(sandbox, 0, &mut Allowance::of(handed))?;
        // A picture placed is kept with its bytes only as a PNG or a JPEG.
        if let Some(Source {
            encoding: Encoding::Sprite { .. },
            ..
        }) = source
        {
            return Err(DrawFailure::ReplyMalformed);
        }
        Ok(PlacedPicture {
            pixels,
            source,
            natural,
        })
    })
}

/// What the pictures of one read may yet hold: their pixels, as a drawing's
/// may together, and the bytes they were read from, which are parts of the
/// one document handed over and so together no longer than it.
struct Allowance {
    pixels: u64,
    bytes: u64,
}

impl Allowance {
    /// All a read of a document `handed` bytes long may hold.
    const fn of(handed: u64) -> Self {
        Self {
            pixels: MAX_TOTAL_IMAGE_PIXELS,
            bytes: handed,
        }
    }
}

/// Picture `index` of what the worker opened, its pixels each held to their
/// alpha as they land and its size and source to what the read's pictures
/// may yet hold, `allowance`.
fn receive_picture<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    index: usize,
    allowance: &mut Allowance,
) -> Result<(Surface, Option<Source>), DrawFailure> {
    let number = u32::try_from(index).map_err(|_| DrawFailure::ReplyMalformed)?;
    let mut ask = [0; 13];
    ask[0] = OP_PICTURE;
    ask[1..5].copy_from_slice(&number.to_le_bytes());
    let reply = sandbox.request(&ask[..5]).map_err(DrawFailure::Sandbox)?;
    let mut r = Reader::new(&reply);
    DrawFailure::expect_tag(&mut r, REPLY_PICTURE)?;
    let (width, height) = (r.u32()?, r.u32()?);
    let (encoding, length) = take_source(&mut r)?;
    let count = u64::from(width) * u64::from(height);
    let sized = (1..=MAX_IMAGE_SIDE).contains(&width)
        && (1..=MAX_IMAGE_SIDE).contains(&height)
        && count <= MAX_SURFACE_PIXELS as u64
        && count <= allowance.pixels
        && length <= allowance.bytes;
    if !r.is_exhausted() || !sized {
        return Err(DrawFailure::ReplyMalformed);
    }
    allowance.pixels -= count;
    allowance.bytes -= length;
    let mut pixels = Surface::new(width, height).ok_or(DrawFailure::NoMemory)?;
    let length = usize::try_from(length).map_err(|_| DrawFailure::ReplyMalformed)?;
    let mut bytes = fallible::zeroed(length).ok_or(DrawFailure::NoMemory)?;
    let pixel_bytes = pixels.pixels().len() * PIXEL_BYTES;
    let total = pixel_bytes + length;
    ask[0] = OP_PIXELS;
    let mut at = 0;
    while at < total {
        ask[5..13].copy_from_slice(&(at as u64).to_le_bytes());
        let reply = sandbox.request(&ask).map_err(DrawFailure::Sandbox)?;
        let mut r = Reader::new(&reply);
        DrawFailure::expect_tag(&mut r, REPLY_PIXELS)?;
        let chunk = r.take(r.remaining())?;
        // Every chunk but the last fills a frame, so a picture comes in as few
        // exchanges as its size allows, and whole pixels arrive whole.
        if chunk.len() != (total - at).min(PIXELS_BUDGET) {
            return Err(DrawFailure::ReplyMalformed);
        }
        let end = at + chunk.len();
        let (into_pixels, into_source) =
            chunk.split_at(pixel_bytes.saturating_sub(at).min(chunk.len()));
        if !into_pixels.is_empty() {
            let first = at / PIXEL_BYTES;
            let landed = pixels.pixels_mut().get_mut(first..).unwrap_or_default();
            for (slot, &[r, g, b, a]) in landed
                .iter_mut()
                .zip(into_pixels.as_chunks::<PIXEL_BYTES>().0)
            {
                // Premultiplied, no channel exceeds its alpha.
                *slot = Pixel {
                    r: r.min(a),
                    g: g.min(a),
                    b: b.min(a),
                    a,
                };
            }
        }
        if !into_source.is_empty() {
            let from = (at + into_pixels.len()) - pixel_bytes;
            bytes[from..from + into_source.len()].copy_from_slice(into_source);
        }
        at = end;
    }
    let source = match encoding {
        Some(encoding) => Some(Source {
            encoding,
            bytes: SharedSlice::try_from_vec(bytes)?,
        }),
        None => None,
    };
    Ok((pixels, source))
}

/// What a picture's source states, and how many bytes it is.
fn take_source(r: &mut Reader<'_>) -> Result<(Option<Encoding>, u64), DrawFailure> {
    let encoding = match r.u8()? {
        SOURCE_NONE => None,
        SOURCE_PNG => Some(Encoding::Png),
        SOURCE_JPEG => {
            let stored = (r.u32()?, r.u32()?);
            let components = r.u8()?;
            let progressive = take_flag(r)?;
            let orientation =
                Orientation::from_tag(u32::from(r.u8()?)).ok_or(DrawFailure::ReplyMalformed)?;
            let density = if take_flag(r)? {
                let across = (r.u32()?, r.u32()?);
                let down = (r.u32()?, r.u32()?);
                let unit = take_choice(
                    r,
                    &[
                        DensityUnit::Aspect,
                        DensityUnit::Inch,
                        DensityUnit::Centimetre,
                        DensityUnit::Metre,
                    ],
                )?;
                Some(Density::new(across, down, unit).ok_or(DrawFailure::ReplyMalformed)?)
            } else {
                None
            };
            Some(Encoding::Jpeg(JpegFrame {
                stored,
                components,
                progressive,
                orientation,
                density,
            }))
        }
        SOURCE_SPRITE => Some(Encoding::Sprite {
            eig: (r.u8()?, r.u8()?),
        }),
        _ => return Err(DrawFailure::ReplyMalformed),
    };
    let length = r.u64()?;
    if encoding.is_some() == (length == 0) || length > MAX_IMAGE_SOURCE_BYTES as u64 {
        return Err(DrawFailure::ReplyMalformed);
    }
    Ok((encoding, length))
}

// The worker's half: a drawing as records.

fn put_f64(w: &mut Writer, value: f64) {
    w.u64(value.to_bits());
}

fn put_point(w: &mut Writer, point: Point) {
    put_f64(w, point.x);
    put_f64(w, point.y);
}

fn put_affine(w: &mut Writer, map: Affine) {
    for value in [map.a, map.b, map.c, map.d, map.e, map.f] {
        put_f64(w, value);
    }
}

fn put_rgba(w: &mut Writer, colour: Rgba) {
    w.u32(u32::from_be_bytes([colour.r, colour.g, colour.b, colour.a]));
}

fn put_text(w: &mut Writer, text: Option<&str>) {
    w.u8(u8::from(text.is_some()));
    if let Some(text) = text {
        w.str(text);
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// What a picture's source states, and how many bytes it is.
fn put_source(w: &mut Writer, source: Option<&Source>) {
    let Some(source) = source else {
        w.u8(SOURCE_NONE);
        w.u64(0);
        return;
    };
    match source.encoding {
        Encoding::Png => w.u8(SOURCE_PNG),
        Encoding::Jpeg(frame) => {
            w.u8(SOURCE_JPEG);
            w.u32(frame.stored.0);
            w.u32(frame.stored.1);
            w.u8(frame.components);
            w.u8(u8::from(frame.progressive));
            w.u8(u8::try_from(frame.orientation.tag()).unwrap_or(1));
            w.u8(u8::from(frame.density.is_some()));
            if let Some(density) = frame.density {
                for (numerator, denominator) in [density.across(), density.down()] {
                    w.u32(numerator);
                    w.u32(denominator);
                }
                w.u8(match density.unit() {
                    DensityUnit::Aspect => 0,
                    DensityUnit::Inch => 1,
                    DensityUnit::Centimetre => 2,
                    DensityUnit::Metre => 3,
                });
            }
        }
        Encoding::Sprite { eig } => {
            w.u8(SOURCE_SPRITE);
            w.u8(eig.0);
            w.u8(eig.1);
        }
    }
    w.u64(source.bytes.len() as u64);
}

fn put_header(w: &mut Writer, read: &Read, pictures: usize) {
    let drawing = &read.drawing;
    let page = drawing.page();
    put_f64(w, page.width);
    put_f64(w, page.height);
    w.u8(unit_wire(page.unit));
    w.u8(u8::from(page.background.is_some()));
    put_rgba(w, page.background.unwrap_or(Rgba::new(0, 0, 0, 0)));
    let grid = drawing.grid();
    put_f64(w, grid.spacing);
    w.u32(grid.subdivisions);
    w.u32(count(drawing.guides().len()));
    for guide in drawing.guides() {
        w.u8(match guide.axis {
            GuideAxis::Horizontal => 0,
            GuideAxis::Vertical => 1,
        });
        put_f64(w, guide.position);
    }
    put_text(w, drawing.info().title.as_deref());
    put_text(w, drawing.info().description.as_deref());
    w.u32(count(drawing.swatches().len()));
    for &swatch in drawing.swatches() {
        put_rgba(w, swatch);
    }
    w.u32(drawing.resolution());
    w.u32(read.unkept.bits());
    w.u32(count(drawing.layers().len()));
    w.u32(count(pictures));
}

/// Records, each length-prefixed, and where each one ends.
struct Records {
    bytes: Vec<u8>,
    ends: Vec<usize>,
    /// The record being written, its buffer kept between records.
    scratch: Vec<u8>,
}

impl Records {
    fn record(
        &mut self,
        fields: impl FnOnce(&mut Writer) -> Result<(), DrawRefusal>,
    ) -> Result<(), DrawRefusal> {
        let mut record = Writer::reusing(core::mem::take(&mut self.scratch));
        fields(&mut record)?;
        let mut body = record.finish();
        if body.len() > MAX_RECORD {
            return Err(DrawRefusal::TooComplex);
        }
        if self.bytes.try_reserve(BYTES_PREFIX + body.len()).is_err() {
            return Err(DrawRefusal::OutOfMemory);
        }
        self.bytes.extend_from_slice(&bytes_prefix(body.len()));
        self.bytes.extend_from_slice(&body);
        if !fallible::push(&mut self.ends, self.bytes.len()) {
            return Err(DrawRefusal::OutOfMemory);
        }
        body.clear();
        self.scratch = body;
        Ok(())
    }
}

/// Every record `drawing` comes to — its gradients, then its styles, then
/// its tree depth first, a group's clip before its children — and the
/// pictures its objects show, by the number the records name them by.
fn records_of(drawing: &Drawing) -> Result<(Records, Vec<Shown>), DrawRefusal> {
    let mut styles = Vec::new();
    let mut gradients = Vec::new();
    let mut pictures = Vec::new();
    let mut failed = false;
    let mut collect = |node: &Node| {
        if let Kind::Image { image } = node.kind() {
            let shown = Shown {
                pixels: Held::Pyramid(image.pyramid().clone()),
                source: image.source().cloned(),
            };
            failed |= !fallible::push(&mut pictures, (Arc::as_ptr(image.pyramid()).addr(), shown));
        }
        if let Some(style) = node.style() {
            failed |= !fallible::push(&mut styles, (Arc::as_ptr(style).addr(), style.clone()));
            for paint in [&style.fill, &style.stroke.paint] {
                if let Paint::Gradient(gradient) = paint {
                    failed |= !fallible::push(
                        &mut gradients,
                        (Arc::as_ptr(gradient).addr(), gradient.clone()),
                    );
                }
            }
        }
    };
    for layer in drawing.layers().iter() {
        walk(layer, &mut collect);
    }
    if failed {
        return Err(DrawRefusal::OutOfMemory);
    }
    let mut sender = Sender {
        records: Records {
            bytes: Vec::new(),
            ends: Vec::new(),
            scratch: Vec::new(),
        },
        gradients: Numbered::of(gradients).map_err(DrawRefusal::of_drawing)?,
        styles: Numbered::of(styles).map_err(DrawRefusal::of_drawing)?,
        pictures: Numbered::of(pictures).map_err(DrawRefusal::of_drawing)?,
    };
    for gradient in sender.gradients.items() {
        sender.records.record(|w| {
            put_gradient(w, gradient);
            Ok(())
        })?;
    }
    for style in sender.styles.items() {
        let gradients = &sender.gradients;
        sender.records.record(|w| put_style(w, style, gradients))?;
    }
    for layer in drawing.layers().iter() {
        sender.node(layer)?;
    }
    Ok((sender.records, sender.pictures.into_items()))
}

/// Call `visit` on `node` and everything within it, clips included.
fn walk(node: &Node, visit: &mut impl FnMut(&Node)) {
    visit(node);
    if let Kind::Group { children, clip } = node.kind() {
        if let Some(clip) = clip {
            walk(clip, visit);
        }
        for child in children.iter() {
            walk(child, visit);
        }
    }
}

struct Sender {
    records: Records,
    gradients: Numbered<usize, Arc<Gradient>>,
    styles: Numbered<usize, Arc<Style>>,
    pictures: Numbered<usize, Shown>,
}

impl Sender {
    /// Write one record of infallible fields.
    fn record(&mut self, fields: impl FnOnce(&mut Writer)) -> Result<(), DrawRefusal> {
        self.records.record(|w| {
            fields(w);
            Ok(())
        })
    }

    fn style_number(&self, style: &Arc<Style>) -> Result<u32, DrawRefusal> {
        let number = self
            .styles
            .number(Arc::as_ptr(style).addr())
            .ok_or(DrawRefusal::OutOfMemory)?;
        Ok(count(number))
    }

    /// A picture's object: its common fields, the number of the picture it
    /// shows and the rectangle it shows it over.
    fn image(&mut self, node: &Node, image: &Image) -> Result<(), DrawRefusal> {
        let number = self
            .pictures
            .number(Arc::as_ptr(image.pyramid()).addr())
            .ok_or(DrawRefusal::OutOfMemory)?;
        let place = image.place();
        self.record(|w| {
            w.u8(RECORD_IMAGE);
            put_common(w, node);
            w.u32(count(number));
            put_point(w, place.min);
            put_point(w, place.max);
        })
    }

    fn node(&mut self, node: &Arc<Node>) -> Result<(), DrawRefusal> {
        match node.kind() {
            Kind::Group { children, clip } => {
                self.record(|w| {
                    w.u8(RECORD_GROUP);
                    put_common(w, node);
                    w.u32(count(children.len()));
                    w.u8(u8::from(clip.is_some()));
                })?;
                if let Some(clip) = clip {
                    self.node(clip)?;
                }
                for child in children.iter() {
                    self.node(child)?;
                }
                Ok(())
            }
            Kind::Shape { shape, style } => {
                let style = self.style_number(style)?;
                match shape {
                    Shape::Rect(rect) => self.record(|w| {
                        w.u8(RECORD_RECT);
                        put_common(w, node);
                        w.u32(style);
                        for value in [rect.x, rect.y, rect.width, rect.height, rect.rx, rect.ry] {
                            put_f64(w, value);
                        }
                    }),
                    Shape::Ellipse(ellipse) => self.record(|w| {
                        w.u8(RECORD_ELLIPSE);
                        put_common(w, node);
                        w.u32(style);
                        put_point(w, ellipse.centre);
                        put_point(w, ellipse.radii);
                        w.u8(u8::from(ellipse.arc.is_some()));
                        if let Some(arc) = ellipse.arc {
                            put_f64(w, arc.start);
                            put_f64(w, arc.end);
                            w.u8(match arc.kind {
                                ArcKind::Open => 0,
                                ArcKind::Pie => 1,
                                ArcKind::Chord => 2,
                            });
                        }
                    }),
                    Shape::Star(star) => self.record(|w| {
                        w.u8(RECORD_STAR);
                        put_common(w, node);
                        w.u32(style);
                        put_point(w, star.centre);
                        w.u32(star.points);
                        for value in [star.outer, star.inner_ratio, star.rotation, star.rounding] {
                            put_f64(w, value);
                        }
                        w.u8(u8::from(star.polygon));
                    }),
                    Shape::Path(path) => {
                        self.record(|w| {
                            w.u8(RECORD_PATH);
                            put_common(w, node);
                            w.u32(style);
                            w.u32(count(path.contours.len()));
                        })?;
                        for contour in &path.contours {
                            self.record(|w| {
                                w.u8(RECORD_CONTOUR);
                                w.u8(u8::from(contour.closed));
                                w.u32(count(contour.nodes.len()));
                            })?;
                            for run in contour.nodes.chunks(NODES_PER_RECORD) {
                                self.record(|w| put_nodes(w, run))?;
                            }
                        }
                        Ok(())
                    }
                }
            }
            Kind::Image { image } => self.image(node, image),
            Kind::Text { text, style } => {
                let style = self.style_number(style)?;
                self.record(|w| put_lettering(w, node, style, text))
            }
        }
    }
}

/// A text's record: its fields, its baseline and its runs.
fn put_lettering(w: &mut Writer, node: &Node, style: u32, text: &Text) {
    w.u8(RECORD_TEXT);
    put_common(w, node);
    w.u32(style);
    w.str(&text.content);
    w.str(&text.family);
    put_f64(w, text.size);
    w.u32(u32::from(text.weight));
    w.u8(u8::from(text.italic));
    put_f64(w, text.letter_spacing);
    w.u8(match text.align {
        TextAlign::Start => 0,
        TextAlign::Middle => 1,
        TextAlign::End => 2,
    });
    match &text.baseline {
        Baseline::Lines {
            origin,
            line_height,
        } => {
            w.u8(0);
            put_point(w, *origin);
            put_f64(w, *line_height);
        }
        Baseline::Path(along) => {
            w.u8(1);
            put_f64(w, along.start);
            w.u32(count(along.path.contours.len()));
            for contour in &along.path.contours {
                w.u8(u8::from(contour.closed));
                w.u32(count(contour.nodes.len()));
                for node in &contour.nodes {
                    put_node(w, node);
                }
            }
        }
    }
    put_runs(w, &text.runs);
}

fn put_common(w: &mut Writer, node: &Node) {
    put_text(w, node.name());
    w.u8(u8::from(node.visible()));
    w.u8(u8::from(node.locked()));
    put_f64(w, node.opacity());
    put_affine(w, node.transform());
}

fn put_nodes(w: &mut Writer, nodes: &[PathNode]) {
    w.u8(RECORD_NODES);
    w.u32(count(nodes.len()));
    for node in nodes {
        put_node(w, node);
    }
}

fn put_node(w: &mut Writer, node: &PathNode) {
    put_point(w, node.point);
    for handle in [node.before, node.after] {
        w.u8(u8::from(handle.is_some()));
        if let Some(handle) = handle {
            put_point(w, handle);
        }
    }
    w.u8(match node.join {
        NodeJoin::Corner => 0,
        NodeJoin::Smooth => 1,
        NodeJoin::Symmetric => 2,
    });
}

/// Which of a run's overrides follow its mask, in the order they follow.
const RUN_FAMILY: u8 = 1;
const RUN_SIZE: u8 = 1 << 1;
const RUN_WEIGHT: u8 = 1 << 2;
const RUN_ITALIC: u8 = 1 << 3;
const RUN_SPACING: u8 = 1 << 4;
const RUN_SHIFT: u8 = 1 << 5;
const RUN_FILL: u8 = 1 << 6;
const RUN_STROKE: u8 = 1 << 7;

fn put_runs(w: &mut Writer, runs: &[Run]) {
    w.u32(count(runs.len()));
    for run in runs {
        w.u32(count(run.start));
        w.u32(count(run.end));
        let style = &run.style;
        let mask = [
            (style.family.is_some(), RUN_FAMILY),
            (style.size.is_some(), RUN_SIZE),
            (style.weight.is_some(), RUN_WEIGHT),
            (style.italic.is_some(), RUN_ITALIC),
            (style.letter_spacing.is_some(), RUN_SPACING),
            (style.baseline_shift.is_some(), RUN_SHIFT),
            (style.fill.is_some(), RUN_FILL),
            (style.stroke.is_some(), RUN_STROKE),
        ]
        .iter()
        .filter(|(set, _)| *set)
        .fold(0, |mask, (_, bit)| mask | bit);
        w.u8(mask);
        if let Some(family) = &style.family {
            w.str(family);
        }
        if let Some(size) = style.size {
            put_f64(w, size);
        }
        if let Some(weight) = style.weight {
            w.u32(u32::from(weight));
        }
        if let Some(italic) = style.italic {
            w.u8(u8::from(italic));
        }
        for length in [style.letter_spacing, style.baseline_shift]
            .into_iter()
            .flatten()
        {
            put_f64(w, length);
        }
        for paint in [style.fill, style.stroke].into_iter().flatten() {
            match paint {
                RunPaint::None => w.u8(0),
                RunPaint::Colour(colour) => {
                    w.u8(1);
                    put_rgba(w, colour);
                }
            }
        }
    }
}

fn put_gradient(w: &mut Writer, gradient: &Gradient) {
    w.u8(RECORD_GRADIENT);
    match gradient.kind {
        GradientKind::Linear { start, end } => {
            w.u8(0);
            put_point(w, start);
            put_point(w, end);
        }
        GradientKind::Radial {
            centre,
            radius,
            focus,
        } => {
            w.u8(1);
            put_point(w, centre);
            put_f64(w, radius);
            put_point(w, focus);
        }
    }
    w.u8(match gradient.spread {
        Spread::Pad => 0,
        Spread::Reflect => 1,
        Spread::Repeat => 2,
    });
    put_affine(w, gradient.transform);
    w.u32(count(gradient.stops.len()));
    for stop in &gradient.stops {
        put_f64(w, stop.offset);
        put_rgba(w, stop.colour);
    }
}

fn put_paint(
    w: &mut Writer,
    paint: &Paint,
    gradients: &Numbered<usize, Arc<Gradient>>,
) -> Result<(), DrawRefusal> {
    match paint {
        Paint::None => w.u8(0),
        Paint::Colour(colour) => {
            w.u8(1);
            put_rgba(w, *colour);
        }
        Paint::Gradient(gradient) => {
            let number = gradients
                .number(Arc::as_ptr(gradient).addr())
                .ok_or(DrawRefusal::OutOfMemory)?;
            w.u8(2);
            w.u32(count(number));
        }
    }
    Ok(())
}

fn put_head(w: &mut Writer, head: Arrowhead) {
    w.u8(match head.shape {
        ArrowShape::None => 0,
        ArrowShape::Triangle => 1,
        ArrowShape::Arrow => 2,
        ArrowShape::Circle => 3,
        ArrowShape::Square => 4,
        ArrowShape::Diamond => 5,
        ArrowShape::Bar => 6,
    });
    put_f64(w, head.width);
    put_f64(w, head.length);
}

fn put_style(
    w: &mut Writer,
    style: &Style,
    gradients: &Numbered<usize, Arc<Gradient>>,
) -> Result<(), DrawRefusal> {
    w.u8(RECORD_STYLE);
    put_paint(w, &style.fill, gradients)?;
    w.u8(u8::from(style.fill_rule == FillRule::EvenOdd));
    let stroke = &style.stroke;
    put_paint(w, &stroke.paint, gradients)?;
    put_f64(w, stroke.width);
    w.u8(match stroke.join {
        Join::Miter => 0,
        Join::Round => 1,
        Join::Bevel => 2,
    });
    put_f64(w, stroke.miter_limit);
    w.u8(match stroke.cap {
        Cap::Butt => 0,
        Cap::Round => 1,
        Cap::Square => 2,
    });
    w.u32(count(stroke.dashes.len()));
    for dash in stroke.dashes.iter() {
        put_f64(w, *dash);
    }
    put_f64(w, stroke.dash_offset);
    put_head(w, stroke.start);
    put_head(w, stroke.end);
    w.u8(match style.paint_order {
        PaintOrder::FillFirst => 0,
        PaintOrder::StrokeFirst => 1,
    });
    Ok(())
}

const fn unit_wire(unit: Unit) -> u8 {
    match unit {
        Unit::Px => 0,
        Unit::Pt => 1,
        Unit::Pc => 2,
        Unit::Mm => 3,
        Unit::Cm => 4,
        Unit::In => 5,
    }
}

// The editor's half: records believed only as far as they are checked.

fn take_f64(r: &mut Reader<'_>) -> Result<f64, WireError> {
    r.u64().map(f64::from_bits)
}

fn take_point(r: &mut Reader<'_>) -> Result<Point, WireError> {
    Ok(Point::new(take_f64(r)?, take_f64(r)?))
}

fn take_affine(r: &mut Reader<'_>) -> Result<Affine, WireError> {
    Ok(Affine {
        a: take_f64(r)?,
        b: take_f64(r)?,
        c: take_f64(r)?,
        d: take_f64(r)?,
        e: take_f64(r)?,
        f: take_f64(r)?,
    })
}

fn take_rgba(r: &mut Reader<'_>) -> Result<Rgba, WireError> {
    let [red, green, blue, alpha] = r.u32()?.to_be_bytes();
    Ok(Rgba::new(red, green, blue, alpha))
}

fn take_node(r: &mut Reader<'_>) -> Result<PathNode, WireError> {
    let point = take_point(r)?;
    let mut handle = || -> Result<Option<Point>, WireError> {
        if take_flag(r)? {
            Ok(Some(take_point(r)?))
        } else {
            Ok(None)
        }
    };
    let before = handle()?;
    let after = handle()?;
    Ok(PathNode {
        point,
        before,
        after,
        join: take_choice(
            r,
            &[NodeJoin::Corner, NodeJoin::Smooth, NodeJoin::Symmetric],
        )?,
    })
}

fn take_weight(r: &mut Reader<'_>) -> Result<u16, WireError> {
    u16::try_from(r.u32()?).map_err(|_| WireError::Malformed)
}

/// A text's runs, each only as far as its shape is checked: the text checks
/// what they set.
fn take_runs(r: &mut Reader<'_>) -> Result<SharedSlice<Run>, DrawFailure> {
    let runs = take_count(r, MAX_TEXT_RUNS)?;
    let mut held = Vec::new();
    if !fallible::reserve(&mut held, runs) {
        return Err(DrawFailure::NoMemory);
    }
    for _ in 0..runs {
        let start = take_count(r, MAX_TEXT_BYTES)?;
        let end = take_count(r, MAX_TEXT_BYTES)?;
        let mask = r.u8()?;
        let has = |bit: u8| mask & bit != 0;
        let family = if has(RUN_FAMILY) {
            Some(take_str(r, MAX_FAMILY_BYTES)?)
        } else {
            None
        };
        let size = has(RUN_SIZE).then(|| take_f64(r)).transpose()?;
        let weight = has(RUN_WEIGHT).then(|| take_weight(r)).transpose()?;
        let italic = has(RUN_ITALIC).then(|| take_flag(r)).transpose()?;
        let letter_spacing = has(RUN_SPACING).then(|| take_f64(r)).transpose()?;
        let baseline_shift = has(RUN_SHIFT).then(|| take_f64(r)).transpose()?;
        let fill = has(RUN_FILL).then(|| take_run_paint(r)).transpose()?;
        let stroke = has(RUN_STROKE).then(|| take_run_paint(r)).transpose()?;
        let style = RunStyle {
            family,
            size,
            weight,
            italic,
            letter_spacing,
            baseline_shift,
            fill,
            stroke,
        };
        held.push(Run {
            start,
            end,
            style: try_arc(style)?,
        });
    }
    Ok(SharedSlice::try_from_vec(held)?)
}

fn take_run_paint(r: &mut Reader<'_>) -> Result<RunPaint, WireError> {
    match r.u8()? {
        0 => Ok(RunPaint::None),
        1 => Ok(RunPaint::Colour(take_rgba(r)?)),
        _ => Err(WireError::Malformed),
    }
}

fn take_flag(r: &mut Reader<'_>) -> Result<bool, WireError> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

fn take_str(r: &mut Reader<'_>, cap: usize) -> Result<SharedStr, DrawFailure> {
    let text = core::str::from_utf8(r.bytes(cap)?).map_err(|_| DrawFailure::ReplyMalformed)?;
    Ok(SharedStr::try_from_str(text)?)
}

fn take_text(r: &mut Reader<'_>, cap: usize) -> Result<Option<SharedStr>, DrawFailure> {
    if take_flag(r)? {
        take_str(r, cap).map(Some)
    } else {
        Ok(None)
    }
}

/// A count of at most `bound` items.
fn take_count(r: &mut Reader<'_>, bound: usize) -> Result<usize, WireError> {
    let count = usize::try_from(r.u32()?).map_err(|_| WireError::Malformed)?;
    if count > bound {
        return Err(WireError::Malformed);
    }
    Ok(count)
}

/// One of `values`, chosen by the byte read.
fn take_choice<T: Copy>(r: &mut Reader<'_>, values: &[T]) -> Result<T, WireError> {
    values
        .get(usize::from(r.u8()?))
        .copied()
        .ok_or(WireError::Malformed)
}

/// What the open reply says of the drawing beside its records.
struct Header {
    page: Page,
    grid: Grid,
    guides: Vec<Guide>,
    info: Info,
    swatches: Vec<Rgba>,
    resolution: u32,
    unkept: Unkept,
    layers: usize,
    /// How many pictures its objects show.
    pictures: usize,
}

fn take_header(r: &mut Reader<'_>) -> Result<Header, DrawFailure> {
    let width = take_f64(r)?;
    let height = take_f64(r)?;
    let unit = take_choice(
        r,
        &[Unit::Px, Unit::Pt, Unit::Pc, Unit::Mm, Unit::Cm, Unit::In],
    )?;
    let has_background = take_flag(r)?;
    let background = take_rgba(r)?;
    let page = Page {
        width,
        height,
        unit,
        background: has_background.then_some(background),
    };
    let grid = Grid {
        spacing: take_f64(r)?,
        subdivisions: r.u32()?,
    };
    let guide_count = take_count(r, MAX_GUIDES)?;
    let mut guides = Vec::new();
    if !fallible::reserve(&mut guides, guide_count) {
        return Err(DrawFailure::NoMemory);
    }
    for _ in 0..guide_count {
        let axis = take_choice(r, &[GuideAxis::Horizontal, GuideAxis::Vertical])?;
        guides.push(Guide {
            axis,
            position: take_f64(r)?,
        });
    }
    let info = Info {
        title: take_text(r, MAX_TEXT_BYTES)?,
        description: take_text(r, MAX_TEXT_BYTES)?,
    };
    let swatch_count = take_count(r, MAX_SWATCHES)?;
    let mut swatches = Vec::new();
    if !fallible::reserve(&mut swatches, swatch_count) {
        return Err(DrawFailure::NoMemory);
    }
    for _ in 0..swatch_count {
        swatches.push(take_rgba(r)?);
    }
    let resolution = r.u32()?;
    let unkept = Unkept::from_bits(r.u32()?).ok_or(DrawFailure::ReplyMalformed)?;
    let layers = take_count(r, MAX_OBJECTS)?;
    let pictures = take_count(r, MAX_OBJECTS)?;
    Ok(Header {
        page,
        grid,
        guides,
        info,
        swatches,
        resolution,
        unkept,
        layers,
        pictures,
    })
}

/// An object's fields every kind has.
struct Common {
    name: Option<SharedStr>,
    visible: bool,
    locked: bool,
    opacity: f64,
    transform: Affine,
}

impl Common {
    fn take(r: &mut Reader<'_>) -> Result<Self, DrawFailure> {
        Ok(Self {
            name: take_text(r, MAX_NAME_BYTES)?,
            visible: take_flag(r)?,
            locked: take_flag(r)?,
            opacity: take_f64(r)?,
            transform: take_affine(r)?,
        })
    }

    /// `kind` as a node of these fields, its identity issued from `ids`.
    fn node(self, kind: Kind, ids: &mut Ids) -> Result<Arc<Node>, DrawingError> {
        let node = Node::new(ids.issue(), kind)?
            .with_name(self.name)?
            .with_opacity(self.opacity)?
            .with_transform(self.transform)?
            .with_visible(self.visible)
            .with_locked(self.locked);
        Ok(try_arc(node)?)
    }
}

/// A group whose clip and children are arriving.
struct OpenGroup {
    common: Common,
    clip: Clip,
    children: Vec<Arc<Node>>,
    expected: usize,
}

/// Where a group stands with its clip.
enum Clip {
    None,
    Awaited,
    Held(Arc<Node>),
}

/// A path whose contours are arriving.
struct OpenPath {
    common: Common,
    style: Arc<Style>,
    contours: Vec<Contour>,
    expected: usize,
    /// The contour whose nodes are arriving, and how many it declared.
    current: Option<(Contour, usize)>,
}

struct Builder<'i> {
    ids: &'i mut Ids,
    header: Header,
    /// Every picture the drawing's objects show, by its number.
    pictures: Vec<(Arc<Pyramid>, Option<Source>)>,
    gradients: Vec<Arc<Gradient>>,
    styles: Vec<Arc<Style>>,
    open: Vec<OpenGroup>,
    path: Option<OpenPath>,
    layers: Vec<Arc<Node>>,
    objects: usize,
    path_nodes: usize,
}

impl<'i> Builder<'i> {
    fn new(
        header: Header,
        pictures: Vec<(Arc<Pyramid>, Option<Source>)>,
        ids: &'i mut Ids,
    ) -> Self {
        Self {
            ids,
            header,
            pictures,
            gradients: Vec::new(),
            styles: Vec::new(),
            open: Vec::new(),
            path: None,
            layers: Vec::new(),
            objects: 0,
            path_nodes: 0,
        }
    }

    /// Take one batch reply, answering whether it was the last.
    fn batch(&mut self, reply: &[u8]) -> Result<bool, DrawFailure> {
        let mut r = Reader::new(reply);
        DrawFailure::expect_tag(&mut r, REPLY_BATCH)?;
        let last = take_flag(&mut r)?;
        let mut records = 0usize;
        while !r.is_exhausted() {
            let mut fields = Reader::new(r.bytes(MAX_RECORD)?);
            // A record's own failure, the editor's memory refused say, is
            // answered as itself rather than as the bytes it left unread.
            self.record(&mut fields)?;
            if !fields.is_exhausted() {
                return Err(DrawFailure::ReplyMalformed);
            }
            records += 1;
        }
        // A batch that brings nothing and is not the last would never end.
        if records == 0 && !last {
            return Err(DrawFailure::ReplyMalformed);
        }
        Ok(last)
    }

    fn record(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let tag = r.u8()?;
        if self.path.is_some() && !matches!(tag, RECORD_CONTOUR | RECORD_NODES) {
            return Err(DrawFailure::ReplyMalformed);
        }
        match tag {
            RECORD_GRADIENT => self.gradient(r),
            RECORD_STYLE => self.style(r),
            RECORD_GROUP => self.group(r),
            RECORD_RECT => self.rect(r),
            RECORD_ELLIPSE => self.ellipse(r),
            RECORD_STAR => self.star(r),
            RECORD_PATH => self.path(r),
            RECORD_CONTOUR => self.contour(r),
            RECORD_NODES => self.nodes(r),
            RECORD_TEXT => self.text(r),
            RECORD_IMAGE => self.image(r),
            _ => Err(DrawFailure::ReplyMalformed),
        }
    }

    fn image(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let common = Common::take(r)?;
        let (pyramid, source) = usize::try_from(r.u32()?)
            .ok()
            .and_then(|number| self.pictures.get(number))
            .ok_or(DrawFailure::ReplyMalformed)?;
        let place = tairix_bezier::Bounds::from_points([take_point(r)?, take_point(r)?]);
        let image = Image::shared(pyramid.clone(), place, source.clone())?;
        self.counted()?;
        let node = common.node(
            Kind::Image {
                image: try_arc(image)?,
            },
            self.ids,
        )?;
        self.deliver(node)
    }

    fn rect(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let (common, style) = self.object(r)?;
        let mut values = [0.0; 6];
        for value in &mut values {
            *value = take_f64(r)?;
        }
        let [x, y, width, height, rx, ry] = values;
        let shape = Shape::Rect(RectShape {
            x,
            y,
            width,
            height,
            rx,
            ry,
        });
        self.shape(common, shape, style)
    }

    fn ellipse(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let (common, style) = self.object(r)?;
        let centre = take_point(r)?;
        let radii = take_point(r)?;
        let arc = if take_flag(r)? {
            Some(ArcPart {
                start: take_f64(r)?,
                end: take_f64(r)?,
                kind: take_choice(r, &[ArcKind::Open, ArcKind::Pie, ArcKind::Chord])?,
            })
        } else {
            None
        };
        self.shape(
            common,
            Shape::Ellipse(EllipseShape { centre, radii, arc }),
            style,
        )
    }

    fn star(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let (common, style) = self.object(r)?;
        let centre = take_point(r)?;
        let points = r.u32()?;
        let star = StarShape {
            centre,
            points,
            outer: take_f64(r)?,
            inner_ratio: take_f64(r)?,
            rotation: take_f64(r)?,
            rounding: take_f64(r)?,
            polygon: take_flag(r)?,
        };
        self.shape(common, Shape::Star(star), style)
    }

    fn path(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let (common, style) = self.object(r)?;
        let expected = take_count(r, MAX_PATH_NODES)?;
        let mut contours = Vec::new();
        if !fallible::reserve(&mut contours, expected.min(NODES_PER_RECORD)) {
            return Err(DrawFailure::NoMemory);
        }
        self.path = Some(OpenPath {
            common,
            style,
            contours,
            expected,
            current: None,
        });
        self.path_done()
    }

    fn contour(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let path = self.path.as_mut().ok_or(DrawFailure::ReplyMalformed)?;
        if path.current.is_some() || path.contours.len() == path.expected {
            return Err(DrawFailure::ReplyMalformed);
        }
        let closed = take_flag(r)?;
        let nodes = take_count(r, MAX_PATH_NODES)?;
        if nodes == 0 {
            return Err(DrawFailure::ReplyMalformed);
        }
        self.path_nodes = self.path_nodes.saturating_add(nodes);
        if self.path_nodes > MAX_TOTAL_PATH_NODES {
            return Err(DrawFailure::ReplyMalformed);
        }
        let mut held = Vec::new();
        if !fallible::reserve(&mut held, nodes.min(NODES_PER_RECORD)) {
            return Err(DrawFailure::NoMemory);
        }
        path.current = Some((
            Contour {
                nodes: held,
                closed,
            },
            nodes,
        ));
        Ok(())
    }

    fn text(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let (common, style) = self.object(r)?;
        let content = take_str(r, MAX_TEXT_BYTES)?;
        let family = take_str(r, MAX_FAMILY_BYTES)?;
        let size = take_f64(r)?;
        let weight = take_weight(r)?;
        let italic = take_flag(r)?;
        let letter_spacing = take_f64(r)?;
        let align = take_choice(r, &[TextAlign::Start, TextAlign::Middle, TextAlign::End])?;
        let baseline = match r.u8()? {
            0 => Baseline::Lines {
                origin: take_point(r)?,
                line_height: take_f64(r)?,
            },
            1 => Baseline::Path(try_arc(self.text_path(r)?)?),
            _ => return Err(DrawFailure::ReplyMalformed),
        };
        let text = Text {
            content,
            family,
            size,
            weight,
            italic,
            letter_spacing,
            align,
            baseline,
            runs: take_runs(r)?,
        };
        let kind = Kind::Text {
            text: try_arc(text)?,
            style,
        };
        self.counted()?;
        let node = common.node(kind, self.ids)?;
        self.deliver(node)
    }

    /// The path a text is set along, its nodes counted with every path's.
    fn text_path(&mut self, r: &mut Reader<'_>) -> Result<TextPath, DrawFailure> {
        let start = take_f64(r)?;
        let contours = take_count(r, MAX_TEXT_PATH_NODES)?;
        let mut path = PathShape {
            contours: Vec::new(),
        };
        if !fallible::reserve(&mut path.contours, contours) {
            return Err(DrawFailure::NoMemory);
        }
        let mut held = 0;
        for _ in 0..contours {
            let closed = take_flag(r)?;
            let nodes = take_count(r, MAX_TEXT_PATH_NODES - held)?;
            held += nodes;
            self.path_nodes = self.path_nodes.saturating_add(nodes);
            if self.path_nodes > MAX_TOTAL_PATH_NODES {
                return Err(DrawFailure::ReplyMalformed);
            }
            let mut contour = Contour {
                nodes: Vec::new(),
                closed,
            };
            if !fallible::reserve(&mut contour.nodes, nodes) {
                return Err(DrawFailure::NoMemory);
            }
            for _ in 0..nodes {
                contour.nodes.push(take_node(r)?);
            }
            path.contours.push(contour);
        }
        Ok(TextPath {
            path: try_arc(path)?,
            start,
        })
    }

    /// An object's common fields and the style it names.
    fn object(&self, r: &mut Reader<'_>) -> Result<(Common, Arc<Style>), DrawFailure> {
        let common = Common::take(r)?;
        let style = usize::try_from(r.u32()?)
            .ok()
            .and_then(|number| self.styles.get(number))
            .ok_or(DrawFailure::ReplyMalformed)?;
        Ok((common, style.clone()))
    }

    /// One more object, held to the drawing's bound.
    fn counted(&mut self) -> Result<(), DrawFailure> {
        self.objects += 1;
        if self.objects > MAX_OBJECTS {
            return Err(DrawFailure::ReplyMalformed);
        }
        Ok(())
    }

    fn gradient(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        // Each style refers to two gradients at most.
        if self.gradients.len() >= 2 * MAX_OBJECTS {
            return Err(DrawFailure::ReplyMalformed);
        }
        let kind = match r.u8()? {
            0 => GradientKind::Linear {
                start: take_point(r)?,
                end: take_point(r)?,
            },
            1 => GradientKind::Radial {
                centre: take_point(r)?,
                radius: take_f64(r)?,
                focus: take_point(r)?,
            },
            _ => return Err(DrawFailure::ReplyMalformed),
        };
        let spread = take_choice(r, &[Spread::Pad, Spread::Reflect, Spread::Repeat])?;
        let transform = take_affine(r)?;
        let stop_count = take_count(r, MAX_STOPS)?;
        let mut stops = Vec::new();
        if !fallible::reserve(&mut stops, stop_count) {
            return Err(DrawFailure::NoMemory);
        }
        for _ in 0..stop_count {
            stops.push(Stop {
                offset: take_f64(r)?,
                colour: take_rgba(r)?,
            });
        }
        let gradient = Gradient {
            kind,
            stops,
            spread,
            transform,
        };
        gradient.check()?;
        Ok(try_push(&mut self.gradients, try_arc(gradient)?)?)
    }

    fn paint(&self, r: &mut Reader<'_>) -> Result<Paint, DrawFailure> {
        Ok(match r.u8()? {
            0 => Paint::None,
            1 => Paint::Colour(take_rgba(r)?),
            2 => {
                let gradient = usize::try_from(r.u32()?)
                    .ok()
                    .and_then(|number| self.gradients.get(number))
                    .ok_or(DrawFailure::ReplyMalformed)?;
                Paint::Gradient(gradient.clone())
            }
            _ => return Err(DrawFailure::ReplyMalformed),
        })
    }

    fn head(r: &mut Reader<'_>) -> Result<Arrowhead, WireError> {
        let shape = take_choice(
            r,
            &[
                ArrowShape::None,
                ArrowShape::Triangle,
                ArrowShape::Arrow,
                ArrowShape::Circle,
                ArrowShape::Square,
                ArrowShape::Diamond,
                ArrowShape::Bar,
            ],
        )?;
        Ok(Arrowhead {
            shape,
            width: take_f64(r)?,
            length: take_f64(r)?,
        })
    }

    fn style(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        if self.styles.len() >= MAX_OBJECTS {
            return Err(DrawFailure::ReplyMalformed);
        }
        let fill = self.paint(r)?;
        let fill_rule = if take_flag(r)? {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        let paint = self.paint(r)?;
        let width = take_f64(r)?;
        let join = take_choice(r, &[Join::Miter, Join::Round, Join::Bevel])?;
        let miter_limit = take_f64(r)?;
        let cap = take_choice(r, &[Cap::Butt, Cap::Round, Cap::Square])?;
        let dash_count = take_count(r, MAX_DASH_ENTRIES)?;
        let mut dashes = Vec::new();
        if !fallible::reserve(&mut dashes, dash_count) {
            return Err(DrawFailure::NoMemory);
        }
        for _ in 0..dash_count {
            dashes.push(take_f64(r)?);
        }
        let stroke = Stroke {
            paint,
            width,
            join,
            miter_limit,
            cap,
            dashes: SharedSlice::try_from_vec(dashes)?,
            dash_offset: take_f64(r)?,
            start: Self::head(r)?,
            end: Self::head(r)?,
        };
        let paint_order = take_choice(r, &[PaintOrder::FillFirst, PaintOrder::StrokeFirst])?;
        let style = Style {
            fill,
            fill_rule,
            stroke,
            paint_order,
        };
        style.check()?;
        Ok(try_push(&mut self.styles, try_arc(style)?)?)
    }

    fn group(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let common = Common::take(r)?;
        let expected = take_count(r, MAX_OBJECTS)?;
        let clip = if take_flag(r)? {
            Clip::Awaited
        } else {
            Clip::None
        };
        self.counted()?;
        if self.open.len() >= MAX_DEPTH {
            return Err(DrawFailure::ReplyMalformed);
        }
        let group = OpenGroup {
            common,
            clip,
            children: Vec::new(),
            expected,
        };
        if group.is_whole() {
            let node = self.group_node(group)?;
            return self.deliver(node);
        }
        if !fallible::push(&mut self.open, group) {
            return Err(DrawFailure::NoMemory);
        }
        Ok(())
    }

    fn nodes(&mut self, r: &mut Reader<'_>) -> Result<(), DrawFailure> {
        let path = self.path.as_mut().ok_or(DrawFailure::ReplyMalformed)?;
        let (contour, declared) = path.current.as_mut().ok_or(DrawFailure::ReplyMalformed)?;
        let room = *declared - contour.nodes.len();
        let count = take_count(r, NODES_PER_RECORD.min(room))?;
        if count == 0 || contour.nodes.try_reserve(count).is_err() {
            return Err(if count == 0 {
                DrawFailure::ReplyMalformed
            } else {
                DrawFailure::NoMemory
            });
        }
        for _ in 0..count {
            contour.nodes.push(take_node(r)?);
        }
        if contour.nodes.len() == *declared {
            if let Some((contour, _)) = path.current.take() {
                if !fallible::push(&mut path.contours, contour) {
                    return Err(DrawFailure::NoMemory);
                }
            }
        }
        self.path_done()
    }

    /// Build the open path once its every contour has arrived.
    fn path_done(&mut self) -> Result<(), DrawFailure> {
        let whole = self
            .path
            .as_ref()
            .is_some_and(|path| path.current.is_none() && path.contours.len() == path.expected);
        if !whole {
            return Ok(());
        }
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        let shape = Shape::Path(try_arc(PathShape {
            contours: path.contours,
        })?);
        self.shape(path.common, shape, path.style)
    }

    fn shape(
        &mut self,
        common: Common,
        shape: Shape,
        style: Arc<Style>,
    ) -> Result<(), DrawFailure> {
        self.counted()?;
        let node = common.node(Kind::Shape { shape, style }, self.ids)?;
        self.deliver(node)
    }

    fn group_node(&mut self, group: OpenGroup) -> Result<Arc<Node>, DrawFailure> {
        let clip = match group.clip {
            Clip::Held(clip) => Some(clip),
            Clip::None => None,
            Clip::Awaited => return Err(DrawFailure::ReplyMalformed),
        };
        let kind = Kind::Group {
            children: Children::from_nodes(group.children)?,
            clip,
        };
        Ok(group.common.node(kind, self.ids)?)
    }

    /// Hand a finished node to the group it belongs to, closing every group
    /// that it completes, or to the drawing's layers.
    fn deliver(&mut self, mut node: Arc<Node>) -> Result<(), DrawFailure> {
        loop {
            let Some(open) = self.open.last_mut() else {
                if self.layers.len() >= self.header.layers {
                    return Err(DrawFailure::ReplyMalformed);
                }
                return Ok(try_push(&mut self.layers, node)?);
            };
            if matches!(open.clip, Clip::Awaited) {
                open.clip = Clip::Held(node);
            } else if open.children.len() < open.expected {
                try_push(&mut open.children, node)?;
            } else {
                return Err(DrawFailure::ReplyMalformed);
            }
            if !open.is_whole() {
                return Ok(());
            }
            let Some(whole) = self.open.pop() else {
                return Ok(());
            };
            node = self.group_node(whole)?;
        }
    }

    /// The drawing, once every record has arrived.
    fn finish(self) -> Result<Read, DrawFailure> {
        if !self.open.is_empty() || self.path.is_some() || self.layers.len() != self.header.layers {
            return Err(DrawFailure::ReplyMalformed);
        }
        let header = self.header;
        let drawing = Drawing::new(
            header.page,
            Children::from_nodes(self.layers)?,
            SharedSlice::try_from_vec(header.guides)?,
            header.grid,
        )?
        .with_info(header.info)?
        .with_swatches(SharedSlice::try_from_vec(header.swatches)?)?
        .with_resolution(header.resolution)?;
        Ok(Read {
            drawing,
            unkept: header.unkept,
        })
    }
}

impl OpenGroup {
    fn is_whole(&self) -> bool {
        !matches!(self.clip, Clip::Awaited) && self.children.len() == self.expected
    }
}

#[cfg(test)]
#[path = "drawedit_tests.rs"]
mod tests;

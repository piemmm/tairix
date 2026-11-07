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

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, FillRule, Point};
use tairix_colour::Rgba;
use tairix_drawing::limits::{
    MAX_DASH_ENTRIES, MAX_DEPTH, MAX_FAMILY_BYTES, MAX_GUIDES, MAX_NAME_BYTES, MAX_OBJECTS,
    MAX_PATH_NODES, MAX_STOPS, MAX_TEXT_BYTES, MAX_TOTAL_PATH_NODES,
};
use tairix_drawing::numbering::Numbered;
use tairix_drawing::shape::{EllipseShape, RectShape, StarShape};
use tairix_drawing::svg::{self, Read, ReadError};
use tairix_drawing::unkept::Unkept;
use tairix_drawing::{
    ArcKind, ArcPart, ArrowShape, Arrowhead, Cap, Children, Contour, Drawing, DrawingError,
    Gradient, GradientKind, Grid, Guide, GuideAxis, Ids, Info, Join, Kind, Node, NodeJoin, Page,
    Paint, PaintOrder, PathNode, PathShape, Shape, Spread, Stop, Stroke, Style, Text, TextAlign,
    Unit,
};
use tairix_util::fallible;

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
const REPLY_OPENED: u8 = 30;
const REPLY_BATCH: u8 = 31;

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

/// Most path nodes one record carries.
const NODES_PER_RECORD: usize = 1 << 16;

/// The most one node takes on the wire: its point, two handles and a join.
const NODE_WIRE: usize = 16 + 2 * (1 + 16) + 1;

/// What a batch reply spends besides its records: its tag and whether it is
/// the last.
const BATCH_HEADER: usize = 2;

/// The most record bytes one batch carries.
const BATCH_BUDGET: usize = MAX_FRAME - BATCH_HEADER;

/// The longest record: a text's content and family with its fixed fields,
/// or a run of nodes, whichever is longer.
const MAX_RECORD: usize = {
    let text = 1 + COMMON_WIRE + 4 + 2 * BYTES_PREFIX + MAX_TEXT_BYTES + MAX_FAMILY_BYTES + 64;
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

/// A format a drawing is read from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DrawFormat {
    /// SVG.
    Svg,
}

impl DrawFormat {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Svg => 1,
        }
    }

    const fn from_wire(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Svg),
            _ => None,
        }
    }
}

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
            _ => return None,
        })
    }

    const fn of_read(error: ReadError) -> Self {
        match error {
            ReadError::NotText | ReadError::NotSvg => Self::NotADrawing,
            ReadError::Malformed => Self::Malformed,
            ReadError::TooComplex => Self::TooComplex,
            ReadError::OutOfMemory => Self::OutOfMemory,
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
                Document::serve(request, &mut self.document).map_err(DocumentRefusal::to_wire)
            }
            Some(OP_OPEN) => self.open(request).map_err(DrawRefusal::to_wire),
            Some(OP_NEXT) => self.next(request).map_err(DrawRefusal::to_wire),
            _ => Err(DrawRefusal::MalformedRequest.to_wire()),
        };
        served.unwrap_or_else(encode_error)
    }
}

impl DrawEditService {
    /// `OP_OPEN`: read the document handed over as the format named, and
    /// answer the drawing's page and settings, keeping its records to send.
    fn open(&mut self, request: &[u8]) -> Result<Vec<u8>, DrawRefusal> {
        self.reading = None;
        let mut r = Reader::new(request);
        let format = r
            .whole(|r| {
                r.u8()?;
                r.u8()
            })
            .ok()
            .and_then(DrawFormat::from_wire)
            .ok_or(DrawRefusal::MalformedRequest)?;
        let document = self.document.take().ok_or(DrawRefusal::NoDocument)?;
        if !document.is_complete() {
            return Err(DrawRefusal::NoDocument);
        }
        let bytes = document.into_bytes();
        let read = match format {
            DrawFormat::Svg => svg::read(&bytes, &mut Ids::new()).map_err(DrawRefusal::of_read)?,
        };
        drop(bytes);
        let records = records_of(&read.drawing)?;
        let mut w = Writer::new();
        w.u8(REPLY_OPENED);
        put_header(&mut w, &read);
        self.reading = Some(Reading { records, sent: 0 });
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

/// Read the document handed over last as `format`, and rebuild the drawing
/// it comes to, every value checked, issuing its objects' identities from
/// `ids`.
///
/// # Errors
///
/// [`DrawFailure`]: the sandbox failed, the worker refused, or a reply could
/// not be believed.
pub fn read_drawing<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    format: DrawFormat,
    ids: &mut Ids,
) -> Result<Read, DrawFailure> {
    sandbox.ask(|sandbox| {
        let reply = sandbox
            .request(&[OP_OPEN, format.to_wire()])
            .map_err(DrawFailure::Sandbox)?;
        let mut r = Reader::new(&reply);
        DrawFailure::expect_tag(&mut r, REPLY_OPENED)?;
        let header = take_header(&mut r)?;
        if !r.is_exhausted() {
            return Err(DrawFailure::ReplyMalformed);
        }
        let mut builder = Builder::new(header, ids);
        loop {
            let reply = sandbox.request(&[OP_NEXT]).map_err(DrawFailure::Sandbox)?;
            if builder.batch(&reply)? {
                return builder.finish();
            }
        }
    })
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

fn put_header(w: &mut Writer, read: &Read) {
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
    w.u32(read.unkept.bits());
    w.u32(count(drawing.layers().len()));
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

/// Every record `drawing` comes to: its gradients, then its styles, then
/// its tree depth first, a group's clip before its children.
fn records_of(drawing: &Drawing) -> Result<Records, DrawRefusal> {
    let mut styles = Vec::new();
    let mut gradients = Vec::new();
    let mut failed = false;
    let mut collect = |node: &Node| {
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
    Ok(sender.records)
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
            Kind::Text { text, style } => {
                let style = self.style_number(style)?;
                self.record(|w| {
                    w.u8(RECORD_TEXT);
                    put_common(w, node);
                    w.u32(style);
                    w.str(&text.content);
                    w.str(&text.family);
                    put_f64(w, text.size);
                    w.u32(u32::from(text.weight));
                    w.u8(u8::from(text.italic));
                    put_f64(w, text.letter_spacing);
                    put_f64(w, text.line_height);
                    w.u8(match text.align {
                        TextAlign::Start => 0,
                        TextAlign::Middle => 1,
                        TextAlign::End => 2,
                    });
                    put_point(w, text.origin);
                })
            }
        }
    }
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

fn take_flag(r: &mut Reader<'_>) -> Result<bool, WireError> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

fn take_str(r: &mut Reader<'_>, cap: usize) -> Result<Arc<str>, WireError> {
    let bytes = r.bytes(cap)?;
    core::str::from_utf8(bytes)
        .map(Arc::from)
        .map_err(|_| WireError::Malformed)
}

fn take_text(r: &mut Reader<'_>, cap: usize) -> Result<Option<Arc<str>>, WireError> {
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
    unkept: Unkept,
    layers: usize,
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
    let unkept = Unkept::from_bits(r.u32()?).ok_or(DrawFailure::ReplyMalformed)?;
    let layers = take_count(r, MAX_OBJECTS)?;
    Ok(Header {
        page,
        grid,
        guides,
        info,
        unkept,
        layers,
    })
}

/// An object's fields every kind has.
struct Common {
    name: Option<Arc<str>>,
    visible: bool,
    locked: bool,
    opacity: f64,
    transform: Affine,
}

impl Common {
    fn take(r: &mut Reader<'_>) -> Result<Self, WireError> {
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
        Ok(Arc::new(node))
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
    gradients: Vec<Arc<Gradient>>,
    styles: Vec<Arc<Style>>,
    open: Vec<OpenGroup>,
    path: Option<OpenPath>,
    layers: Vec<Arc<Node>>,
    objects: usize,
    path_nodes: usize,
}

impl<'i> Builder<'i> {
    fn new(header: Header, ids: &'i mut Ids) -> Self {
        Self {
            ids,
            header,
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
            let record = r.bytes(MAX_RECORD)?;
            let mut fields = Reader::new(record);
            fields.whole(|fields| Ok(self.record(fields)))??;
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
            _ => Err(DrawFailure::ReplyMalformed),
        }
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
        let text = Text {
            content: take_str(r, MAX_TEXT_BYTES)?,
            family: take_str(r, MAX_FAMILY_BYTES)?,
            size: take_f64(r)?,
            weight: u16::try_from(r.u32()?).map_err(|_| DrawFailure::ReplyMalformed)?,
            italic: take_flag(r)?,
            letter_spacing: take_f64(r)?,
            line_height: take_f64(r)?,
            align: take_choice(r, &[TextAlign::Start, TextAlign::Middle, TextAlign::End])?,
            origin: take_point(r)?,
        };
        let kind = Kind::Text {
            text: Arc::new(text),
            style,
        };
        self.counted()?;
        let node = common.node(kind, self.ids)?;
        self.deliver(node)
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
        push(&mut self.gradients, Arc::new(gradient))
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
            dashes: Arc::from(dashes),
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
        push(&mut self.styles, Arc::new(style))
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
            let point = take_point(r)?;
            let before = if take_flag(r)? {
                Some(take_point(r)?)
            } else {
                None
            };
            let after = if take_flag(r)? {
                Some(take_point(r)?)
            } else {
                None
            };
            let join = take_choice(
                r,
                &[NodeJoin::Corner, NodeJoin::Smooth, NodeJoin::Symmetric],
            )?;
            contour.nodes.push(PathNode {
                point,
                before,
                after,
                join,
            });
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
        let shape = Shape::Path(Arc::new(PathShape {
            contours: path.contours,
        }));
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
                return push(&mut self.layers, node);
            };
            if matches!(open.clip, Clip::Awaited) {
                open.clip = Clip::Held(node);
            } else if open.children.len() < open.expected {
                push(&mut open.children, node)?;
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
            Arc::from(header.guides),
            header.grid,
        )?
        .with_info(header.info)?;
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

fn push<T>(into: &mut Vec<T>, item: T) -> Result<(), DrawFailure> {
    if fallible::push(into, item) {
        Ok(())
    } else {
        Err(DrawFailure::NoMemory)
    }
}

#[cfg(test)]
#[path = "drawedit_tests.rs"]
mod tests;
